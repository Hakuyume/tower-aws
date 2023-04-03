use lambda_http::RequestExt;
use std::fmt::{Debug, Display};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use url::Url;

pub fn layer<B>() -> Layer<B> {
    Layer {
        _data: PhantomData::default(),
    }
}

#[derive(Clone)]
pub struct Layer<B> {
    _data: PhantomData<fn(B) -> B>,
}

impl<S, B> tower::Layer<S> for Layer<B> {
    type Service = Middleware<S, B>;

    fn layer(&self, inner: S) -> Self::Service {
        Self::Service {
            inner,
            _data: PhantomData::default(),
        }
    }
}

#[derive(Clone)]
pub struct Middleware<S, B> {
    inner: S,
    _data: PhantomData<fn(B) -> B>,
}

impl<S, B> tower::Service<lambda_http::Request> for Middleware<S, B>
where
    S: tower::Service<http::Request<B>>,
    S::Response: lambda_http::IntoResponse,
    S::Error: Debug + Display,
    S::Future: Send + 'static,
    B: Default + From<String> + From<Vec<u8>>,
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

        let mut url = Url::parse(&parts.uri.to_string()).unwrap();
        url.set_path(&raw_http_path);
        parts.uri = url.to_string().parse().unwrap();

        let body = match body {
            lambda_http::Body::Empty => B::default(),
            lambda_http::Body::Text(body) => body.into(),
            lambda_http::Body::Binary(body) => body.into(),
        };

        Box::pin(self.inner.call(http::Request::from_parts(parts, body)))
    }
}
