//! HTTP API (axum) for Agent-review.

mod auth;
mod auth_cookies;
mod login_rate_limit;
mod problem;
mod request_ctx;
mod store;
mod v1;
mod web_ui;
mod webhooks;

pub use crate::store::Store;

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tower_http::trace::TraceLayer;

async fn capture_request_path(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    request_ctx::REQUEST_PATH.scope(path, next.run(req)).await
}

/// Build the full application router.
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(v1::healthz))
        .route("/readyz", get(v1::readyz))
        .route("/metrics", get(v1::metrics))
        .merge(web_ui::router())
        .nest("/api/v1", v1::router())
        .nest("/webhooks", webhooks::router())
        .layer(middleware::from_fn(capture_request_path))
        .layer(TraceLayer::new_for_http())
        .with_state(store)
}
