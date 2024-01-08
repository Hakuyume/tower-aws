use aws_config::BehaviorVersion;
use axum::body::Body;
use axum::extract::FromRef;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Json};
use axum::routing;
use axum::Router;
use serde::Serialize;
use std::env;
use tower::Layer;
use tower_aws::kms_cookie::{Cookie, KeyId, PrivateCookieJar};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    let config = aws_config::load_defaults(BehaviorVersion::latest()).await;

    #[derive(Clone, FromRef)]
    struct State {
        kms_client: aws_sdk_kms::Client,
        kms_key_id: KeyId,
    }

    let app = Router::new()
        .route("/counter", routing::get(counter))
        .fallback(fallback)
        .with_state(State {
            kms_client: aws_sdk_kms::Client::new(&config),
            kms_key_id: KeyId::new(env::var("KMS_KEY_ID")?),
        });

    lambda_http::run(tower_aws::lambda_compat::layer::<Body>().layer(app))
        .await
        .map_err(|e| anyhow::format_err!(e))?;

    Ok(())
}

async fn counter(jar: PrivateCookieJar) -> impl IntoResponse {
    #[derive(Serialize)]
    struct Response {
        count: usize,
    }

    let count = jar
        .get("count")
        .and_then(|cookie| cookie.value().parse::<usize>().ok())
        .unwrap_or_default();
    let count = count + 1;

    let jar = jar.add(
        Cookie::build(("count", count.to_string()))
            .http_only(true)
            .secure(true),
    );

    (jar.finish().await, Json(Response { count }))
}

async fn fallback(uri: Uri) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, uri.to_string())
}
