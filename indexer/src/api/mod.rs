mod history;
mod live;
mod proxy;
pub mod wallet;
use crate::{
    App, auth,
    error::{Error, Result},
    metadata,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{net::SocketAddr, time::Duration};

pub fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/config", get(config))
        .route("/v1/market", get(catalog))
        .route("/v1/market/catalog", get(catalog))
        .route("/v1/market/metadata/{hash}", get(document))
        .route("/v1/opencode/challenge", post(auth::challenge))
        .route("/v1/opencode/connect", post(auth::connect))
        .route("/v1/opencode/refresh", post(auth::refresh))
        .route("/v1/opencode/logout", post(auth::logout))
        .route("/v1/opencode/device", post(auth::device))
        .route("/v1/opencode/device/{id}", get(auth::device))
        .route("/v1/opencode/device/{id}/poll", get(auth::device))
        .route("/v1/opencode/device/{id}/approve", post(auth::device))
        .route("/v1/opencode/wallet", get(wallet::wallet))
        .route("/v1/opencode/funds", get(wallet::funds))
        .route("/v1/opencode/receipts", get(history::native))
        .route("/v1/opencode/live", get(live::upgrade))
        .route("/v1/opencode/rpc", post(proxy::rpc))
        .route("/v1/market/orders", get(wallet::orders))
        .route("/v1/market/profile", get(wallet::profile))
        .route("/v1/market/providers/{wallet}", get(wallet::provider))
        .fallback(|| async { Error(StatusCode::NOT_FOUND, "Unknown endpoint") })
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let _permit = match app.http_requests.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return Error(StatusCode::TOO_MANY_REQUESTS, "Server busy").into_response(),
    };
    let headers = request.headers();
    if let Some(origin) = headers.get(header::ORIGIN) {
        let expected = reqwest::Url::parse(&app.config.public_url)
            .expect("validated URL")
            .origin()
            .ascii_serialization();
        if origin.to_str().ok() != Some(expected.as_str()) {
            return Error(StatusCode::FORBIDDEN, "Origin not allowed").into_response();
        }
    }
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|v| v.0.ip().to_string())
        .unwrap_or_else(|| "local".into());
    let auth = request.uri().path().ends_with("/challenge")
        || request.uri().path().ends_with("/connect")
        || request.uri().path().ends_with("/refresh");
    if let Err(error) = app.limits.check(
        format!("{}:{ip}", if auth { "auth" } else { "request" }),
        if auth {
            app.config.auth_limit
        } else {
            app.config.request_limit
        },
    ) {
        return error.into_response();
    }
    let mut response = match tokio::time::timeout(Duration::from_secs(25), next.run(request)).await
    {
        Ok(r) => r,
        Err(_) => Error(StatusCode::GATEWAY_TIMEOUT, "Request timed out").into_response(),
    };
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    if !response.headers().contains_key(header::CACHE_CONTROL) {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    response
}
async fn health(State(app): State<App>) -> impl IntoResponse {
    let ready = app.index.ready(&app.config).await;
    let state = app.index.state.read().await;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(
            json!({"ok":ready,"ready":ready,"version":env!("CARGO_PKG_VERSION"),"slot":state.slot,"snapshotSlot":state.floor,"connected":state.connected,"commitment":app.config.commitment,"accounts":state.accounts.len()}),
        ),
    )
}
async fn config(State(app): State<App>) -> Json<Value> {
    Json(
        json!({"programId":app.config.program.to_string(),"market":app.config.market.to_string(),"usdcMint":app.config.mint.to_string(),"solanaRpc":format!("{}/v1/opencode/rpc",app.config.public_url),"liveUrl":format!("{}/v1/opencode/live",app.config.public_url.replacen("http","ws",1)),"opencode":{"siteUrl":app.config.public_url,"deviceTtlSecs":120,"maxInputTokens":4_194_304,"maxRequestBytes":16_777_216,"catalogMaxAgeMs":10_000,"requestTimeoutMs":180_000,"pollIntervalMs":1000,"mainnetGenesisHash":app.config.genesis,"localBookingPoolSize":2,"manualImport":true,"deviceConnect":false}}),
    )
}
async fn catalog(State(app): State<App>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        app.index.catalog(&app.config).await.body.clone(),
    )
}
async fn document(State(app): State<App>, Path(hash): Path<String>) -> Result<impl IntoResponse> {
    let raw = metadata::document(&app, &hash).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        Bytes::copy_from_slice(&raw),
    ))
}
