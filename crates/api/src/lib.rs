//! HTTP API (axum) for Agent-review.

mod problem;
mod auth;
mod store;
mod v1;
mod webhooks;

pub use crate::store::Store;

use axum::routing::get;
use axum::Router;
use tower_http::trace::TraceLayer;

/// Build the full application router.
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(v1::healthz))
        .route("/readyz", get(v1::readyz))
        .route("/metrics", get(v1::metrics))
        .nest("/api/v1", v1::router())
        .nest("/webhooks", webhooks::router())
        .layer(TraceLayer::new_for_http())
        .with_state(store)
}
