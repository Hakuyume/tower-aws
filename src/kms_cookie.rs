use aws_sdk_kms::error::SdkError;
use aws_sdk_kms::operation::decrypt::DecryptError;
use aws_sdk_kms::operation::encrypt::EncryptError;
use aws_sdk_kms::primitives::Blob;
use aws_sdk_kms::Client;
use axum::extract::{FromRef, FromRequestParts};
use axum::response::{IntoResponseParts, ResponseParts};
use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use cookie::{Cookie, CookieJar};
use http::header::{COOKIE, SET_COOKIE};
use http::request::Parts;
use http::StatusCode;
use std::marker::PhantomData;

#[derive(Clone)]
pub struct KeyId(pub String);

pub struct PrivateCookieJar<K = KeyId> {
    jar: CookieJar,
    client: Client,
    key_id: KeyId,
    _marker: PhantomData<fn(K) -> K>,
}

#[axum::async_trait]
impl<S, K> FromRequestParts<S> for PrivateCookieJar<K>
where
    S: Send + Sync,
    Client: FromRef<S>,
    K: FromRef<S> + Into<KeyId>,
{
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let client = Client::from_ref(state);
        let key_id = K::from_ref(state).into();

        let cookies = parts
            .headers
            .get_all(COOKIE)
            .into_iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(Cookie::split_parse)
            .filter_map(Result::ok)
            .map(|cookie| {
                async  {
                    let Ok(value) = BASE64_URL_SAFE_NO_PAD.decode(cookie.value()) else { return Ok(None) };
                    let output = client
                    .decrypt()
                    .key_id(&key_id.0)
                    .ciphertext_blob(Blob::new(value))
                    .send()
                    .await;
                    match output {
                        Ok(output) => {
                            let value = output.plaintext().cloned().unwrap();
                            let Ok(value) = String::from_utf8(value.into_inner()) else { return Ok(None) };
                            let mut cookie = cookie.into_owned();
                            cookie.set_value(value);
                            Ok(Some(cookie))
                    }
                        Err(SdkError::ServiceError(e))
                            if matches!(e.err(), DecryptError::InvalidCiphertextException(_)) =>
                        {
                            Ok(None)
                        }
                        Err(e) => Err(e),
                    }
                }});

        let mut jar = CookieJar::new();
        for cookie in futures::future::try_join_all(cookies)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        {
            if let Some(cookie) = cookie {
                jar.add_original(cookie);
            }
        }

        Ok(Self {
            jar,
            client,
            key_id,
            _marker: PhantomData,
        })
    }
}

impl<K> PrivateCookieJar<K> {
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

        let cookies = self.jar.delta().map(|cookie| async {
            let output = self
                .client
                .encrypt()
                .key_id(&self.key_id.0)
                .plaintext(Blob::new(cookie.value()))
                .send()
                .await?;
            let mut cookie = cookie.clone();
            cookie.set_value(BASE64_URL_SAFE_NO_PAD.encode(output.ciphertext_blob().unwrap()));
            Ok(cookie)
        });
        Cookies(futures::future::try_join_all(cookies).await)
    }
}
