//! HTTP API (axum) for Agent-review.

mod problem;
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
        .nest("/api/v1", v1::router(store.clone()))
        .nest("/webhooks", webhooks::router(store))
        .layer(TraceLayer::new_for_http())
}
