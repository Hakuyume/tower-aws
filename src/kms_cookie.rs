use aws_sdk_kms::error::SdkError;
use aws_sdk_kms::operation::decrypt::DecryptError;
use aws_sdk_kms::operation::encrypt::EncryptError;
use aws_sdk_kms::primitives::Blob;
use aws_sdk_kms::Client;
use axum::extract::{FromRef, FromRequestParts};
use axum::response::{IntoResponseParts, ResponseParts};
use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
pub use cookie::Cookie;
use cookie::CookieJar;
use futures::{FutureExt, TryFutureExt};
use http::header::{COOKIE, SET_COOKIE};
use http::request::Parts;
use http::{HeaderMap, StatusCode};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone)]
pub struct KeyId(Arc<str>);

impl KeyId {
    pub fn new<K>(key_id: K) -> Self
    where
        K: Into<Arc<str>>,
    {
        Self(key_id.into())
    }
}

pub struct PrivateCookieJar<K = KeyId> {
    jar: CookieJar,
    client: Client,
    key_id: KeyId,
    _marker: PhantomData<fn(K) -> K>,
}

impl PrivateCookieJar {
    pub fn from_headers(
        headers: &HeaderMap,
        client: Client,
        key_id: KeyId,
    ) -> impl Future<Output = Result<Self, SdkError<DecryptError>>> {
        let cookie_outputs = headers
            .get_all(COOKIE)
            .into_iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(Cookie::split_parse)
            .filter_map(|cookie| {
                let cookie = cookie.ok()?;
                let value = BASE64_URL_SAFE_NO_PAD.decode(cookie.value()).ok()?;
                Some((cookie.into_owned(), value))
            })
            .map(|(cookie, value)| {
                client
                    .decrypt()
                    .key_id(&*key_id.0)
                    .ciphertext_blob(Blob::new(value))
                    .send()
                    .map(|output| match output {
                        Ok(output) => Ok(Some((cookie, output))),
                        Err(e @ SdkError::ServiceError(_)) => {
                            tracing::warn!(error = e.to_string());
                            Ok(None)
                        }
                        Err(e) => {
                            tracing::error!(error = e.to_string());
                            Err(e)
                        }
                    })
            });

        futures::future::try_join_all(cookie_outputs).map_ok(|cookie_outputs| {
            let mut jar = CookieJar::new();
            for (mut cookie, output) in cookie_outputs.into_iter().flatten() {
                if let Some(plaintext) = output.plaintext() {
                    if let Ok(value) = String::from_utf8(plaintext.clone().into_inner()) {
                        cookie.set_value(value);
                        jar.add_original(cookie);
                    }
                }
            }
            Self {
                jar,
                client,
                key_id,
                _marker: PhantomData,
            }
        })
    }
}

impl<K> PrivateCookieJar<K> {
    pub fn into_headers(self) -> impl Future<Output = Result<HeaderMap, SdkError<EncryptError>>> {
        futures::future::try_join_all(self.jar.delta().cloned().map(|cookie| {
            self.client
                .encrypt()
                .key_id(&*self.key_id.0)
                .plaintext(Blob::new(cookie.value()))
                .send()
                .map_ok(|output| (cookie, output))
                .inspect_err(|e| tracing::error!(error = e.to_string()))
        }))
        .map_ok(|cookie_outputs| {
            let mut headers = HeaderMap::new();
            for (mut cookie, output) in cookie_outputs {
                if let Some(ciphertext) = output.ciphertext_blob() {
                    cookie.set_value(BASE64_URL_SAFE_NO_PAD.encode(ciphertext));
                    if let Ok(value) = cookie.to_string().parse() {
                        headers.append(SET_COOKIE, value);
                    }
                }
            }
            headers
        })
    }

    pub fn get(&self, name: &str) -> Option<&Cookie<'static>> {
        self.jar.get(name)
    }

    pub fn remove<C>(mut self, cookie: C) -> Self
    where
        C: Into<Cookie<'static>>,
    {
        self.jar.remove(cookie);
        self
    }

    #[allow(clippy::should_implement_trait)]
    pub fn add<C>(mut self, cookie: C) -> Self
    where
        C: Into<Cookie<'static>>,
    {
        self.jar.add(cookie);
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cookie<'static>> {
        self.jar.iter()
    }

    pub fn finish(self) -> impl Future<Output = Finish<Result<HeaderMap, SdkError<EncryptError>>>> {
        self.into_headers().map(Finish)
    }
}

pub struct Finish<T>(T);

impl<S, K> FromRequestParts<S> for PrivateCookieJar<K>
where
    Client: FromRef<S>,
    K: FromRef<S> + Into<KeyId>,
{
    type Rejection = (StatusCode, String);

    fn from_request_parts<'a, 'b, 'c>(
        parts: &'a mut Parts,
        state: &'b S,
    ) -> Pin<Box<dyn Future<Output = Result<Self, Self::Rejection>> + Send + 'c>>
    where
        'a: 'c,
        'b: 'c,
    {
        PrivateCookieJar::from_headers(
            &parts.headers,
            Client::from_ref(state),
            K::from_ref(state).into(),
        )
        .map_ok(
            |PrivateCookieJar {
                 jar,
                 client,
                 key_id,
                 ..
             }| Self {
                jar,
                client,
                key_id,
                _marker: PhantomData,
            },
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        .boxed()
    }
}

impl IntoResponseParts for Finish<Result<HeaderMap, SdkError<EncryptError>>> {
    type Error = (StatusCode, String);

    fn into_response_parts(self, parts: ResponseParts) -> Result<ResponseParts, Self::Error> {
        Ok(self
            .0
            .map_err(|e| (http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .into_response_parts(parts)
            .unwrap())
    }
}
