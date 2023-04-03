use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
use axum::extract::FromRequestParts;
use axum::response::{IntoResponse, IntoResponseParts, ResponseParts};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use either::Either;
use futures::TryFutureExt;
use http::request::Parts;
use http::{Request, Response, StatusCode};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::{self, Future};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::Mutex;

pub fn layer<S>(client: Client, table_name: S) -> Layer
where
    S: Into<Arc<str>>,
{
    Layer {
        client,
        table_name: table_name.into(),
        rng: Arc::new(Mutex::new(ChaCha20Rng::from_entropy())),
    }
}

#[derive(Clone)]
pub struct Layer {
    client: Client,
    table_name: Arc<str>,
    rng: Arc<Mutex<ChaCha20Rng>>,
}

impl<S> tower::Layer<S> for Layer {
    type Service = Middleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Self::Service {
            inner,
            client: self.client.clone(),
            table_name: self.table_name.clone(),
            rng: self.rng.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Middleware<S> {
    inner: S,
    client: Client,
    table_name: Arc<str>,
    rng: Arc<Mutex<ChaCha20Rng>>,
}

impl<S> Middleware<S> {
    async fn call<T, U>(
        mut self,
        mut request: Request<T>,
    ) -> Result<S::Response, Either<S::Error, StatusCode>>
    where
        S: tower::Service<Request<T>, Response = Response<U>>,
    {
        let jar = CookieJar::from_headers(request.headers());
        let mut item = if let Some(cookie) = jar.get("session-id") {
            let session_id = cookie.value();
            let output = self
                .client
                .get_item()
                .table_name(&*self.table_name)
                .key("id", AttributeValue::S(session_id.to_owned()))
                .send()
                .await
                .map_err(|_| Either::Right(StatusCode::INTERNAL_SERVER_ERROR))?;
            output.item().cloned().unwrap_or_default()
        } else {
            HashMap::new()
        };
        let session_id = if let Some(AttributeValue::S(session_id)) = item.remove("id") {
            session_id
        } else {
            format!("{:032x}", self.rng.lock().await.gen::<u128>())
        };

        let prev = item.clone();
        request.extensions_mut().insert(Session(item));
        let mut response = self.inner.call(request).await.map_err(Either::Left)?;
        if let Some(Session(new)) = response.extensions_mut().remove() {
            if new != prev {
                let builder = self
                    .client
                    .put_item()
                    .table_name(&*self.table_name)
                    .item("id", AttributeValue::S(session_id.clone()));
                new.into_iter()
                    .fold(builder, |builder, (key, value)| builder.item(key, value))
                    .send()
                    .await
                    .map_err(|_| Either::Right(StatusCode::INTERNAL_SERVER_ERROR))?;
            }
        }

        let jar = jar.add(
            Cookie::build("session-id", session_id.clone())
                .http_only(true)
                .secure(true)
                .finish(),
        );
        let (parts, body) = response.into_parts();
        let (parts, _) = (parts, jar).into_response().into_parts();
        Ok(Response::from_parts(parts, body))
    }
}

impl<S, T, U> tower::Service<Request<T>> for Middleware<S>
where
    Self: Clone,
    S: tower::Service<Request<T>, Response = Response<U>> + Send + 'static,
    S::Future: Send,
    S::Error: Send,
    T: Send + 'static,
    U: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<T>) -> Self::Future {
        Box::pin(self.clone().call(request).or_else(|e| {
            future::ready(match e {
                Either::Left(e) => Err(e),
                Either::Right(e) => Ok(Response::builder().status(e).body(U::default()).unwrap()),
            })
        }))
    }
}

#[derive(Clone, Default)]
pub struct Session(pub HashMap<String, AttributeValue>);

impl<S> FromRequestParts<S> for Session {
    type Rejection = Infallible;

    fn from_request_parts<'a, 'b, 'c>(
        parts: &'a mut Parts,
        _: &'b S,
    ) -> Pin<Box<dyn Future<Output = Result<Self, Self::Rejection>> + Send + 'c>>
    where
        'a: 'c,
        'b: 'c,
    {
        Box::pin(future::ready(Ok(parts
            .extensions
            .get()
            .cloned()
            .unwrap_or_default())))
    }
}

impl IntoResponseParts for Session {
    type Error = Infallible;

    fn into_response_parts(self, mut parts: ResponseParts) -> Result<ResponseParts, Self::Error> {
        parts.extensions_mut().insert(self);
        Ok(parts)
    }
}
