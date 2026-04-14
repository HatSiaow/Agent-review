//! HTTP API (axum) for Agent-review.

mod problem;
mod store;
mod v1;

pub use crate::store::{InMemoryStore, Store};

use axum::routing::get;
use axum::Router;
use tower_http::trace::TraceLayer;

#[must_use]
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(v1::healthz))
        .nest("/api/v1", v1::router(store))
        .layer(TraceLayer::new_for_http())
}

