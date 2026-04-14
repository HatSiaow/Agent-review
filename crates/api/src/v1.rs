use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
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
        .route("/drafts/:id/approve", post(approve_draft))
        .route("/drafts/:id/reject", post(reject_draft))
        .with_state(state)
}

pub async fn healthz() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize)]
pub struct ReviewListItem {
    pub review: domain::Review,
    pub active_draft: Option<domain::ReplyDraft>,
}

async fn list_reviews(State(state): State<AppState>) -> Json<Vec<ReviewListItem>> {
    let reviews = state.store.list_reviews().await;
    let out = reviews
        .into_iter()
        .map(|(review, active_draft)| ReviewListItem { review, active_draft })
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
    Ok(Json(ReviewWithDraft { review, active_draft }))
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
    let updated = state
        .store
        .approve_draft(id, user_id, req.text)
        .await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router as build_router;
    use axum::body::Body;
    use axum::http::Request;
    use domain::{Platform, Review, ReviewAuthor, ReviewStatus};
    use serde_json::json;
    use time::macros::datetime;
    use tower::ServiceExt as _;

    fn seeded_store() -> Store {
        let store = Store::new();
        let review_id = Uuid::new_v4();
        let review = Review {
            id: review_id,
            platform: Platform::Google,
            source_review_id: "AbCd".to_string(),
            source_location_id: "loc1".to_string(),
            author: ReviewAuthor {
                display_name: "Maria L.".to_string(),
                avatar_url: None,
            },
            rating: 5,
            body_text: Some("Great pasta!".to_string()),
            body_language: Some("en".to_string()),
            created_at: datetime!(2026-04-10 18:22:11 UTC),
            updated_at: datetime!(2026-04-10 18:22:11 UTC),
            ingested_at: datetime!(2026-04-10 18:25:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({ "source": "fixture" }),
        };
        let draft = domain::ReplyDraft::new_pending(review_id, "Thanks!".to_string(), "en".to_string());

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { store.upsert_review_with_draft(review, draft).await });
        store
    }

    #[test]
    fn healthz_ok() {
        let app = build_router(seeded_store());
        let res = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async {
                app.oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
                    .await
                    .unwrap()
            });
        assert_eq!(res.status(), http::StatusCode::OK);
    }
}

