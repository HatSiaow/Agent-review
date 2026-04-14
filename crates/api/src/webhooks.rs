//! Webhook endpoints for platform push events.

use adapter_ubereats::verify_webhook_signature;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;

use crate::Store;

#[derive(Debug, Clone)]
struct WebhookState {
    #[allow(dead_code)]
    store: Store,
    ubereats_webhook_secret: Option<String>,
}

pub fn router(store: Store) -> Router {
    let state = WebhookState {
        store,
        ubereats_webhook_secret: None,
    };

    Router::new()
        .route("/ubereats", post(ubereats_webhook))
        .with_state(state)
}

async fn ubereats_webhook(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    // Verify HMAC signature if secret is configured
    if let Some(ref secret) = state.ubereats_webhook_secret {
        let signature = headers
            .get("x-uber-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if verify_webhook_signature(signature, secret.as_bytes(), &body).is_err() {
            tracing::warn!("UberEats webhook signature verification failed");
            return StatusCode::UNAUTHORIZED;
        }
    }

    // Parse and enqueue — for now just acknowledge
    match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(payload) => {
            let event_type = payload["event_type"].as_str().unwrap_or("unknown");
            tracing::info!(event_type, "received UberEats webhook");
            StatusCode::OK
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse UberEats webhook body");
            StatusCode::BAD_REQUEST
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router as build_router;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn ubereats_webhook_accepts_valid_json() {
        let store = Store::new();
        let app = build_router(store);

        let body = serde_json::json!({
            "event_type": "store.review_created",
            "review_uuid": "abc"
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/ubereats")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ubereats_webhook_rejects_invalid_body() {
        let store = Store::new();
        let app = build_router(store);

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/ubereats")
                    .header("content-type", "application/json")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
