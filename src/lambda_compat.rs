use http::Request;
use lambda_http::request::RequestContext;
use lambda_http::RequestExt;
use std::marker::PhantomData;
use std::net::IpAddr;
use std::task::{Context, Poll};

pub fn layer<T>() -> Layer<T> {
    Layer {
        _marker: PhantomData,
    }
}

#[derive(Clone)]
pub struct Layer<T> {
    _marker: PhantomData<fn(T) -> T>,
}

impl<S, T> tower::Layer<S> for Layer<T> {
    type Service = Service<S, T>;

    fn layer(&self, inner: S) -> Self::Service {
        Self::Service {
            inner,
            _marker: PhantomData,
        }
    }
}

#[derive(Clone)]
pub struct Service<S, T> {
    inner: S,
    _marker: PhantomData<fn(T) -> T>,
}

impl<S, T> tower::Service<lambda_http::Request> for Service<S, T>
where
    S: tower::Service<Request<T>>,
    T: Default + From<String> + From<Vec<u8>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: lambda_http::Request) -> Self::Future {
        let source_ip = match request.request_context() {
            RequestContext::ApiGatewayV1(context) => context.identity.source_ip,
            RequestContext::ApiGatewayV2(context) => context.http.source_ip,
            RequestContext::Alb(_) => None,
            RequestContext::WebSocket(context) => context.identity.source_ip,
        };
        let (mut parts, body) = request.into_parts();
        if let Some(source_ip) = source_ip {
            if let Ok(source_ip) = source_ip.parse::<IpAddr>() {
                parts.extensions.insert(SourceIp(source_ip));
            }
            parts.extensions.insert(SourceIp(source_ip));
        }
        let body = match body {
            lambda_http::Body::Empty => T::default(),
            lambda_http::Body::Text(body) => body.into(),
            lambda_http::Body::Binary(body) => body.into(),
        };
        self.inner.call(Request::from_parts(parts, body))
    }
}

#[derive(Clone, Debug)]
pub struct SourceIp<T>(pub T);
