//! Webhook endpoints for platform push events.

use adapter_ubereats::{normalize_ubereats_review, verify_webhook_signature};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use sha2::Digest as _;

use crate::Store;

#[derive(Debug, Clone)]
struct WebhookState {
    store: Store,
    ubereats_webhook_secret: Option<String>,
}

pub fn router(store: Store) -> Router {
    let state = WebhookState {
        store,
        ubereats_webhook_secret: std::env::var("UBEREATS_WEBHOOK_SECRET").ok(),
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

    let payload = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse UberEats webhook body");
            return StatusCode::BAD_REQUEST;
        }
    };

    let event_type = payload["event_type"].as_str().unwrap_or("unknown");
    tracing::info!(event_type, "received UberEats webhook");

    // Replay protection: use provided event id when present; otherwise fall back to a stable hash
    // of the raw body (still useful against retry storms for identical payloads).
    let event_id = payload
        .get("event_id")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let mut hasher = sha2::Sha256::new();
            hasher.update(&body);
            format!("sha256:{}", hex::encode(hasher.finalize()))
        });
    let is_first = state
        .store
        .register_webhook_event(domain::Platform::Ubereats, &event_id, time::OffsetDateTime::now_utc())
        .await;
    if !is_first {
        tracing::info!(event_type, event_id, "duplicate UberEats webhook event ignored");
        return StatusCode::OK;
    }

    if event_type == "store.review_created" {
        let review_data = if payload.get("review").is_some() {
            &payload["review"]
        } else {
            &payload
        };

        match normalize_ubereats_review(review_data) {
            Ok(review) => {
                let review_id = review.id;
                state.store.ingest_review(review).await;
                tracing::info!(%review_id, "ingested UberEats review from webhook");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to normalize UberEats review from webhook");
            }
        }
    }

    StatusCode::OK
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

    #[tokio::test]
    async fn ubereats_webhook_ingests_review_created_event() {
        let store = Store::new();
        let app = build_router(store.clone());

        let body = serde_json::json!({
            "event_type": "store.review_created",
            "review": {
                "review_uuid": "ue-webhook-test-1",
                "store_uuid": "store-abc",
                "eater": { "first_name": "Marco" },
                "rating": { "overall": 4 },
                "comment": { "text": "Good food", "language": "en" },
                "created_at": "2026-04-10T14:30:00Z"
            }
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

        let reviews = store.list_reviews().await;
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].0.source_review_id, "ue-webhook-test-1");
    }

    #[tokio::test]
    async fn ubereats_webhook_non_review_event_ok() {
        let store = Store::new();
        let app = build_router(store.clone());

        let body = serde_json::json!({
            "event_type": "store.order_created",
            "order_uuid": "order-1"
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
        let reviews = store.list_reviews().await;
        assert!(reviews.is_empty());
    }

    #[tokio::test]
    async fn ubereats_webhook_dedupes_replayed_event_id() {
        let store = Store::new();
        let app = build_router(store.clone());

        let body = serde_json::json!({
            "event_type": "store.review_created",
            "event_id": "evt-1",
            "review": {
                "review_uuid": "ue-webhook-test-dup",
                "store_uuid": "store-abc",
                "eater": { "first_name": "Marco" },
                "rating": { "overall": 4 },
                "comment": { "text": "Good food", "language": "en" },
                "created_at": "2026-04-10T14:30:00Z"
            }
        });
        let bytes = serde_json::to_vec(&body).unwrap();

        for _ in 0..2 {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/webhooks/ubereats")
                        .header("content-type", "application/json")
                        .body(Body::from(bytes.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        let reviews = store.list_reviews().await;
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].0.source_review_id, "ue-webhook-test-dup");
    }
}
