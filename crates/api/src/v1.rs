use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use domain::ReplyDraft;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::problem::ApiError;
use crate::Store;

#[derive(Debug, Clone)]
pub struct AppState {
    store: Store,
}

pub fn router(store: Store) -> Router {
    let state = AppState { store };

    Router::new()
        .route("/reviews", get(list_reviews))
        .route("/reviews/:id", get(get_review))
        .route("/reviews/:id/skip", post(skip_review))
        .route("/reviews/:id/unskip", post(unskip_review))
        .route("/drafts", get(list_drafts))
        .route("/drafts/:id/approve", post(approve_draft))
        .route("/drafts/:id/reject", post(reject_draft))
        .route("/drafts/bulk-approve", post(bulk_approve))
        .with_state(state)
}

// --- System endpoints ---

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn readyz() -> &'static str {
    // In a real deployment this would check DB + secrets reachability
    "ok"
}

// --- Reviews ---

#[derive(Debug, Serialize)]
pub struct ReviewListItem {
    pub review: domain::Review,
    pub active_draft: Option<domain::ReplyDraft>,
}

async fn list_reviews(State(state): State<AppState>) -> Json<Vec<ReviewListItem>> {
    let reviews = state.store.list_reviews().await;
    let out = reviews
        .into_iter()
        .map(|(review, active_draft)| ReviewListItem {
            review,
            active_draft,
        })
        .collect();
    Json(out)
}

#[derive(Debug, Serialize)]
pub struct ReviewWithDraft {
    pub review: domain::Review,
    pub active_draft: Option<domain::ReplyDraft>,
}

async fn get_review(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ReviewWithDraft>, ApiError> {
    let (review, active_draft) = state.store.get_review(id).await?;
    Ok(Json(ReviewWithDraft {
        review,
        active_draft,
    }))
}

async fn skip_review(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<domain::Review>, ApiError> {
    let review = state.store.skip_review(id).await?;
    Ok(Json(review))
}

async fn unskip_review(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<domain::Review>, ApiError> {
    let review = state.store.unskip_review(id).await?;
    Ok(Json(review))
}

// --- Drafts ---

async fn list_drafts(State(state): State<AppState>) -> Json<Vec<ReplyDraft>> {
    Json(state.store.list_drafts().await)
}

#[derive(Debug, Deserialize)]
struct ApproveRequest {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    user_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
struct ApproveResponse {
    id: Uuid,
    state: domain::DraftState,
    #[serde(with = "time::serde::rfc3339::option")]
    posted_at: Option<OffsetDateTime>,
}

async fn approve_draft(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<ApproveResponse>, ApiError> {
    let user_id = req.user_id.unwrap_or_else(Uuid::new_v4);
    let updated = state.store.approve_draft(id, user_id, req.text).await?;

    Ok(Json(ApproveResponse {
        id: updated.id,
        state: updated.state,
        posted_at: updated.posted_at,
    }))
}

#[derive(Debug, Deserialize)]
struct RejectRequest {
    reason: String,
    #[serde(default)]
    user_id: Option<Uuid>,
}

async fn reject_draft(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<RejectRequest>,
) -> Result<Json<ReplyDraft>, ApiError> {
    let user_id = req.user_id.unwrap_or_else(Uuid::new_v4);
    let updated = state.store.reject_draft(id, user_id, req.reason).await?;
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
struct BulkApproveRequest {
    ids: Vec<Uuid>,
    #[serde(default)]
    user_id: Option<Uuid>,
}

async fn bulk_approve(
    State(state): State<AppState>,
    Json(req): Json<BulkApproveRequest>,
) -> Result<Json<Vec<ReplyDraft>>, ApiError> {
    let user_id = req.user_id.unwrap_or_else(Uuid::new_v4);
    let updated = state.store.bulk_approve(&req.ids, user_id).await?;
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router as build_router;
    use axum::body::Body;
    use axum::http::Request;
    use domain::{Platform, Review, ReviewAuthor, ReviewStatus};
    use http::StatusCode;
    use serde_json::json;
    use time::macros::datetime;
    use tower::ServiceExt as _;

    fn make_review_and_draft() -> (Uuid, Review, ReplyDraft) {
        let review_id = Uuid::new_v4();
        let review = Review {
            id: review_id,
            platform: Platform::Google,
            source_review_id: "AbCd".into(),
            source_location_id: "loc1".into(),
            author: ReviewAuthor {
                display_name: "Maria L.".into(),
                avatar_url: None,
            },
            rating: 5,
            body_text: Some("Great pasta!".into()),
            body_language: Some("en".into()),
            created_at: datetime!(2026-04-10 18:22:11 UTC),
            updated_at: datetime!(2026-04-10 18:22:11 UTC),
            ingested_at: datetime!(2026-04-10 18:25:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({ "source": "fixture" }),
        };
        let draft =
            domain::ReplyDraft::new_pending(review_id, "Thanks!".into(), "en".into());
        (review_id, review, draft)
    }

    fn seeded_store() -> (Store, Uuid, Uuid) {
        let store = Store::new();
        let (review_id, review, draft) = make_review_and_draft();
        let draft_id = draft.id;

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { store.upsert_review_with_draft(review, draft).await });
        (store, review_id, draft_id)
    }

    fn oneshot(app: Router, req: Request<Body>) -> http::Response<axum::body::Body> {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { app.oneshot(req).await.unwrap() })
    }

    #[test]
    fn healthz_returns_200() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn readyz_returns_200() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn list_reviews_returns_items() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/api/v1/reviews")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn get_review_found() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri(format!("/api/v1/reviews/{review_id}"))
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn get_review_not_found() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let fake_id = Uuid::new_v4();
        let res = oneshot(
            app,
            Request::builder()
                .uri(format!("/api/v1/reviews/{fake_id}"))
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn skip_review_transitions() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/reviews/{review_id}/skip"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn approve_draft_returns_approved() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let body = serde_json::to_string(&json!({})).unwrap();
        let res = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/drafts/{draft_id}/approve"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn reject_draft_returns_rejected() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let body = serde_json::to_string(&json!({"reason": "too_generic"})).unwrap();
        let res = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn double_reject_fails() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store.clone());
        let body = serde_json::to_string(&json!({"reason": "too_generic"})).unwrap();

        // First reject
        let res = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);

        // Second reject should fail (terminal state)
        let app2 = build_router(store);
        let res2 = oneshot(
            app2,
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        );
        assert_eq!(res2.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn list_drafts_endpoint() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/api/v1/drafts")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }
}
