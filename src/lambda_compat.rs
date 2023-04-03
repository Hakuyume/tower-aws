use http::{Request, Response, StatusCode};
use lambda_http::RequestExt;
use std::future::{self, Future};
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use url::Url;

pub fn layer<T>() -> Layer<T> {
    Layer {
        _data: PhantomData::default(),
    }
}

#[derive(Clone)]
pub struct Layer<T> {
    _data: PhantomData<fn(T) -> T>,
}

impl<S, T> tower::Layer<S> for Layer<T> {
    type Service = Middleware<S, T>;

    fn layer(&self, inner: S) -> Self::Service {
        Self::Service {
            inner,
            _data: PhantomData::default(),
        }
    }
}

#[derive(Clone)]
pub struct Middleware<S, T> {
    inner: S,
    _data: PhantomData<fn(T) -> T>,
}

impl<S, T, U> tower::Service<lambda_http::Request> for Middleware<S, T>
where
    S: tower::Service<Request<T>, Response = Response<U>>,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
    T: Default + From<String> + From<Vec<u8>>,
    U: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: lambda_http::Request) -> Self::Future {
        let raw_http_path = request.raw_http_path();
        let (mut parts, body) = request.into_parts();

        if let Some(uri) = Url::parse(&parts.uri.to_string()).ok().and_then(|mut url| {
            url.set_path(&raw_http_path);
            url.to_string().parse().ok()
        }) {
            parts.uri = uri;

            let body = match body {
                lambda_http::Body::Empty => T::default(),
                lambda_http::Body::Text(body) => body.into(),
                lambda_http::Body::Binary(body) => body.into(),
            };

            Box::pin(self.inner.call(Request::from_parts(parts, body)))
        } else {
            Box::pin(future::ready(Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(U::default())
                .unwrap())))
        }
    }
}
