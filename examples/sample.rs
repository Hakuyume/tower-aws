use axum::body::Body;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Json};
use axum::routing;
use axum::Router;
use serde::Serialize;
use std::env;
use std::time::Duration;
use tower::Layer;
use tower_aws::dynamodb_session::Data;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    let config = aws_config::from_env().load().await;

    let app = Router::new()
        .route("/counter", routing::get(counter))
        .layer(tower_aws::dynamodb_session::layer(
            aws_sdk_dynamodb::Client::new(&config),
            env::var("SESSION_TABLE_NAME").unwrap(),
            Duration::from_secs(60),
        ))
        .fallback(fallback);

    lambda_http::run(tower_aws::lambda_compat::layer::<Body>().layer(app))
        .await
        .unwrap();
}

async fn counter(Data(count): Data<Option<u64>>) -> impl IntoResponse {
    #[derive(Serialize)]
    struct Response {
        count: u64,
    }

    let count = count.unwrap_or_default() + 1;
    (Data(count), Json(Response { count }))
}

async fn fallback(uri: Uri) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, uri.to_string())
}