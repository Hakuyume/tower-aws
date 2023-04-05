use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
use axum::extract::FromRequestParts;
use axum::response::{IntoResponse, IntoResponseParts, ResponseParts};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::{DateTime, NaiveDateTime, Utc};
use either::Either;
use futures::TryFutureExt;
use http::request::Parts;
use http::{Request, Response, StatusCode};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::collections::HashMap;
use std::future::{self, Future};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Mutex;

pub fn layer<S>(client: Client, table_name: S, ttl: Duration) -> Layer
where
    S: Into<Arc<str>>,
{
    Layer {
        client,
        table_name: table_name.into(),
        ttl: chrono::Duration::from_std(ttl).unwrap(),
        rng: Arc::new(Mutex::new(ChaCha20Rng::from_entropy())),
    }
}

#[derive(Clone)]
pub struct Layer {
    client: Client,
    table_name: Arc<str>,
    ttl: chrono::Duration,
    rng: Arc<Mutex<ChaCha20Rng>>,
}

impl<S> tower::Layer<S> for Layer {
    type Service = Middleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Self::Service {
            inner,
            client: self.client.clone(),
            table_name: self.table_name.clone(),
            ttl: self.ttl,
            rng: self.rng.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Middleware<S> {
    inner: S,
    client: Client,
    table_name: Arc<str>,
    ttl: chrono::Duration,
    rng: Arc<Mutex<ChaCha20Rng>>,
}

struct Item(HashMap<String, AttributeValue>);

impl<S> Middleware<S> {
    async fn get(
        client: &Client,
        table_name: &str,
        jar: CookieJar,
        now: DateTime<Utc>,
    ) -> Result<(String, DateTime<Utc>, HashMap<String, AttributeValue>), Either<(), StatusCode>>
    {
        let cookie = jar.get("session-id").ok_or(Either::Left(()))?;
        let id = cookie.value();
        let output = client
            .get_item()
            .table_name(table_name)
            .key("id", AttributeValue::S(id.to_owned()))
            .send()
            .await
            .map_err(|_| Either::Right(StatusCode::INTERNAL_SERVER_ERROR))?;
        let mut item = output.item().cloned().ok_or(Either::Left(()))?;
        let (Some(AttributeValue::S(id)), Some(AttributeValue::N(expires))) = (item.remove("id"), item.remove("expires")) else { return Err(Either::Left(())); };
        let expires = DateTime::from_utc(
            NaiveDateTime::from_timestamp_opt(expires.parse().map_err(|_| Either::Left(()))?, 0)
                .ok_or(Either::Left(()))?,
            Utc,
        );
        if now < expires {
            Ok((id, expires, item))
        } else {
            Err(Either::Left(()))
        }
    }

    async fn call<T, U>(
        mut self,
        mut request: Request<T>,
    ) -> Result<S::Response, Either<S::Error, StatusCode>>
    where
        S: tower::Service<Request<T>, Response = Response<U>>,
    {
        let now = Utc::now();

        let jar = CookieJar::from_headers(request.headers());
        let (id, expires, item) = match Self::get(&self.client, &self.table_name, jar, now).await {
            Ok((id, expires, item)) => Ok((id, expires, item)),
            Err(Either::Left(_)) => Ok((
                format!("{:032x}", self.rng.lock().await.gen::<u128>()),
                now + self.ttl,
                HashMap::new(),
            )),
            Err(Either::Right(e)) => Err(Either::Right(e)),
        }?;

        let prev = item.clone();
        request.extensions_mut().insert(Item(item));
        let mut response = self.inner.call(request).await.map_err(Either::Left)?;
        if let Some(Item(new)) = response.extensions_mut().remove() {
            if new != prev {
                let builder = self
                    .client
                    .put_item()
                    .table_name(&*self.table_name)
                    .item("id", AttributeValue::S(id.clone()))
                    .item(
                        "expires",
                        AttributeValue::N(expires.timestamp().to_string()),
                    );
                new.into_iter()
                    .fold(builder, |builder, (key, value)| builder.item(key, value))
                    .send()
                    .await
                    .map_err(|_| Either::Right(StatusCode::INTERNAL_SERVER_ERROR))?;
            }
        }

        let jar = CookieJar::from_headers(response.headers()).add(
            Cookie::build("session-id", id.clone())
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

pub struct Storage<T>(pub T);

impl<'de, T, S> FromRequestParts<S> for Storage<Option<T>>
where
    T: serde::Deserialize<'de> + Send + 'static,
{
    type Rejection = StatusCode;

    fn from_request_parts<'a, 'b, 'c>(
        parts: &'a mut Parts,
        _: &'b S,
    ) -> Pin<Box<dyn Future<Output = Result<Self, Self::Rejection>> + Send + 'c>>
    where
        'a: 'c,
        'b: 'c,
    {
        let Item(item) = parts.extensions.get().unwrap();
        Box::pin(future::ready(
            item.get("storage")
                .map(|value| serde_dynamo::from_attribute_value(value.clone()))
                .transpose()
                .map(Self)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR),
        ))
    }
}

impl<T> IntoResponseParts for Storage<T>
where
    T: serde::Serialize,
{
    type Error = StatusCode;

    fn into_response_parts(self, mut parts: ResponseParts) -> Result<ResponseParts, Self::Error> {
        parts.extensions_mut().insert(Item(
            [(
                "storage".to_owned(),
                serde_dynamo::to_attribute_value(self.0)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            )]
            .into_iter()
            .collect(),
        ));
        Ok(parts)
    }
}
