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
use http::StatusCode;
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
        let client = Client::from_ref(state);
        let key_id = K::from_ref(state).into();

        let cookies = parts
            .headers
            .get_all(COOKIE)
            .into_iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(Cookie::split_parse)
            .filter_map(|cookie| {
                let cookie = cookie.ok()?;
                let value = BASE64_URL_SAFE_NO_PAD.decode(cookie.value()).ok()?;
                Some((cookie, value))
            })
            .map(|(cookie, value)| {
                client
                    .decrypt()
                    .key_id(&*key_id.0)
                    .ciphertext_blob(Blob::new(value))
                    .send()
                    .map(|output| match output {
                        Ok(output) => Ok(String::from_utf8(
                            output.plaintext().unwrap().clone().into_inner(),
                        )
                        .ok()
                        .map(|value| {
                            let mut cookie = cookie.into_owned();
                            cookie.set_value(value);
                            cookie
                        })),
                        Err(SdkError::ServiceError(e))
                            if matches!(e.err(), DecryptError::InvalidCiphertextException(_)) =>
                        {
                            Ok(None)
                        }
                        Err(e) => Err(e),
                    })
            });

        Box::pin(
            futures::future::try_join_all(cookies)
                .map_ok(|cookies| {
                    let mut jar = CookieJar::new();
                    for cookie in cookies.into_iter().flatten() {
                        jar.add_original(cookie);
                    }
                    Self {
                        jar,
                        client,
                        key_id,
                        _marker: PhantomData,
                    }
                })
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        )
    }
}

impl<K> PrivateCookieJar<K> {
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, cookie: Cookie<'static>) -> Self {
        self.jar.add(cookie);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Cookie<'static>> {
        self.jar.get(name)
    }

    pub fn remove(mut self, cookie: Cookie<'static>) -> Self {
        self.jar.remove(cookie);
        self
    }

    pub async fn finish(self) -> impl IntoResponseParts {
        struct Cookies(Result<Vec<Cookie<'static>>, SdkError<EncryptError>>);

        impl IntoResponseParts for Cookies {
            type Error = (StatusCode, String);

            fn into_response_parts(
                self,
                mut parts: ResponseParts,
            ) -> Result<ResponseParts, Self::Error> {
                for cookie in self
                    .0
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                {
                    parts
                        .headers_mut()
                        .append(SET_COOKIE, cookie.to_string().parse().unwrap());
                }
                Ok(parts)
            }
        }

        let cookies = self.jar.delta().map(|cookie| {
            self.client
                .encrypt()
                .key_id(&*self.key_id.0)
                .plaintext(Blob::new(cookie.value()))
                .send()
                .map_ok(|output| {
                    let mut cookie = cookie.clone();
                    cookie.set_value(
                        BASE64_URL_SAFE_NO_PAD.encode(output.ciphertext_blob().unwrap()),
                    );
                    cookie
                })
        });
        Cookies(futures::future::try_join_all(cookies).await)
    }
}
