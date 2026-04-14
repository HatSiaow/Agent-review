use std::sync::Arc;
use std::{fmt, fmt::Formatter};

use domain::{ReplyDraft, RestaurantSettings, RestaurantSettingsPatch, Review};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::problem::ApiError;

#[derive(Clone)]
pub struct Store {
    repo: Arc<dyn storage::Repository>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Store(..)")
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    #[must_use]
    pub fn new() -> Self {
        Self {
            repo: Arc::new(storage::InMemoryRepository::new()),
        }
    }

    #[must_use]
    pub fn from_repo(repo: Arc<dyn storage::Repository>) -> Self {
        Self { repo }
    }

    fn map_err(err: &storage::RepositoryError) -> ApiError {
        match err {
            storage::RepositoryError::NotFound => ApiError::NotFound,
            storage::RepositoryError::InvalidTransition => ApiError::InvalidTransition,
            storage::RepositoryError::Conflict("draft_has_guardrail_warnings") => {
                ApiError::BadRequest("cannot bulk-approve drafts with guardrail warnings")
            }
            storage::RepositoryError::Conflict("bulk_approve_requires_5_star") => {
                ApiError::BadRequest("bulk-approve is only allowed for 5-star reviews")
            }
            storage::RepositoryError::Conflict("undo_not_reviewer") => {
                ApiError::BadRequest("only the approving user can undo this bulk approve")
            }
            storage::RepositoryError::Conflict("undo_not_bulk_approved") => {
                ApiError::BadRequest("draft is not in bulk-approve undo window")
            }
            storage::RepositoryError::Conflict("undo_window_elapsed") => {
                ApiError::BadRequest("undo window elapsed")
            }
            storage::RepositoryError::Conflict(_) | storage::RepositoryError::Storage(_) => {
                ApiError::BadRequest("storage error")
            }
        }
    }

    pub async fn list_reviews(&self) -> Vec<(Review, Option<ReplyDraft>)> {
        self.repo.list_reviews().await.unwrap_or_default()
    }

    pub async fn list_reviews_filtered(
        &self,
        query: storage::ReviewListQuery,
    ) -> Vec<(Review, Option<ReplyDraft>)> {
        self.repo
            .list_reviews_filtered(query)
            .await
            .unwrap_or_default()
    }

    pub async fn list_drafts_filtered(&self, query: storage::DraftListQuery) -> Vec<ReplyDraft> {
        self.repo
            .list_drafts_filtered(query)
            .await
            .unwrap_or_default()
    }

    pub async fn get_restaurant_settings(&self) -> Result<RestaurantSettings, ApiError> {
        self.repo
            .get_restaurant_settings()
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn update_restaurant_settings(
        &self,
        patch: RestaurantSettingsPatch,
    ) -> Result<RestaurantSettings, ApiError> {
        let current = self.get_restaurant_settings().await?;
        let merged = current.merge(patch);
        self.repo
            .put_restaurant_settings(merged.clone())
            .await
            .map_err(|_| ApiError::ServiceUnavailable)?;
        Ok(merged)
    }

    pub async fn list_users(&self) -> Result<Vec<domain::User>, ApiError> {
        self.repo
            .list_users()
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn ping(&self) -> Result<(), ApiError> {
        self.repo.ping().await.map_err(|_| ApiError::ServiceUnavailable)
    }

    // --- Auth / sessions ---

    pub async fn get_user_auth_by_email(
        &self,
        email: &str,
    ) -> Result<Option<storage::UserAuth>, ApiError> {
        self.repo
            .get_user_auth_by_email(email)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn get_user_by_id(&self, user_id: Uuid) -> Result<Option<domain::User>, ApiError> {
        self.repo
            .get_user_by_id(user_id)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn create_session(&self, session: domain::Session) -> Result<(), ApiError> {
        self.repo
            .create_session(session)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn get_session_user(
        &self,
        session_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Option<(domain::Session, domain::User)>, ApiError> {
        self.repo
            .get_session_user(session_id, now)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn touch_session(
        &self,
        session_id: Uuid,
        new_expires_at: OffsetDateTime,
    ) -> Result<(), ApiError> {
        self.repo
            .touch_session(session_id, new_expires_at)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    pub async fn delete_session(&self, session_id: Uuid) -> Result<(), ApiError> {
        self.repo
            .delete_session(session_id)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)
    }

    // --- Notifications outbox ---

    pub async fn enqueue_notification_outbox(
        &self,
        id: Uuid,
        occurred_at: OffsetDateTime,
        notification_type: domain::NotificationType,
        review_id: Option<Uuid>,
        draft_id: Option<Uuid>,
        payload_json: serde_json::Value,
    ) {
        let _ = self
            .repo
            .enqueue_notification_outbox(
                id,
                occurred_at,
                notification_type,
                review_id,
                draft_id,
                payload_json,
            )
            .await;
    }

    pub async fn claim_notification_outbox_batch(
        &self,
        limit: u32,
    ) -> Vec<storage::NotificationOutboxItem> {
        self.repo
            .claim_notification_outbox_batch(limit)
            .await
            .unwrap_or_default()
    }

    pub async fn mark_notification_outbox_sent(&self, id: Uuid, sent_at: OffsetDateTime) {
        let _ = self.repo.mark_notification_outbox_sent(id, sent_at).await;
    }

    pub async fn get_review(&self, id: Uuid) -> Result<(Review, Option<ReplyDraft>), ApiError> {
        self.repo.get_review(id).await.map_err(|e| Self::map_err(&e))
    }

    pub async fn upsert_review_with_draft(&self, review: Review, draft: ReplyDraft) {
        let _ = self.repo.upsert_review_with_draft(review, draft).await;
    }

    pub async fn ingest_review(&self, review: Review) {
        let _ = self.repo.ingest_review(review).await;
    }

    pub async fn get_reviews_sync_state(
        &self,
        platform: domain::Platform,
    ) -> Option<time::OffsetDateTime> {
        self.repo.get_reviews_sync_state(platform).await.ok().flatten()
    }

    pub async fn set_reviews_sync_state(
        &self,
        platform: domain::Platform,
        last_seen_update_time: time::OffsetDateTime,
    ) {
        let _ = self
            .repo
            .set_reviews_sync_state(platform, last_seen_update_time)
            .await;
    }

    pub async fn register_webhook_event(
        &self,
        platform: domain::Platform,
        event_id: &str,
        received_at: time::OffsetDateTime,
    ) -> bool {
        self.repo
            .register_webhook_event(platform, event_id, received_at)
            .await
            .unwrap_or(false)
    }

    pub async fn get_idempotency_response(
        &self,
        idempotency_key: &str,
    ) -> Option<(u16, serde_json::Value)> {
        self.repo
            .get_idempotency_response(idempotency_key)
            .await
            .ok()
            .flatten()
    }

    pub async fn put_idempotency_response(
        &self,
        idempotency_key: &str,
        status: u16,
        body_json: serde_json::Value,
        created_at: time::OffsetDateTime,
    ) {
        let _ = self
            .repo
            .put_idempotency_response(idempotency_key, status, body_json, created_at)
            .await;
    }

    pub async fn store_agent_draft(&self, draft: ReplyDraft) {
        let _ = self.repo.store_agent_draft(draft).await;
    }

    pub async fn store_agent_run(&self, run: domain::AgentRun) {
        let _ = self.repo.store_agent_run(run).await;
    }

    pub async fn list_agent_runs(&self, review_id: Uuid) -> Vec<domain::AgentRun> {
        self.repo.list_agent_runs(review_id).await.unwrap_or_default()
    }

    pub async fn transition_review_to_drafting(&self, review_id: Uuid) -> Result<Review, ApiError> {
        self.repo
            .transition_review_to_drafting(review_id)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn list_drafts(&self) -> Vec<ReplyDraft> {
        self.repo.list_drafts().await.unwrap_or_default()
    }

    pub async fn approve_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        new_text: Option<String>,
    ) -> Result<ReplyDraft, ApiError> {
        self.repo
            .approve_draft(draft_id, reviewed_by, new_text)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn reject_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        reason: String,
    ) -> Result<ReplyDraft, ApiError> {
        self.repo
            .reject_draft(draft_id, reviewed_by, reason)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn skip_review(&self, review_id: Uuid) -> Result<Review, ApiError> {
        self.repo
            .skip_review(review_id)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn unskip_review(&self, review_id: Uuid) -> Result<Review, ApiError> {
        self.repo
            .unskip_review(review_id)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
    ) -> Result<Vec<ReplyDraft>, ApiError> {
        self.repo
            .bulk_approve(draft_ids, reviewed_by)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn undo_bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
        now: time::OffsetDateTime,
    ) -> Result<Vec<ReplyDraft>, ApiError> {
        self.repo
            .undo_bulk_approve(draft_ids, reviewed_by, now)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn mark_draft_posted(
        &self,
        draft_id: Uuid,
        posted_at: time::OffsetDateTime,
    ) -> Result<ReplyDraft, ApiError> {
        self.repo
            .mark_draft_posted(draft_id, posted_at)
            .await
            .map_err(|e| Self::map_err(&e))
    }

    pub async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> Result<ReplyDraft, ApiError> {
        self.repo
            .mark_draft_post_failed(draft_id, error)
            .await
            .map_err(|e| Self::map_err(&e))
    }
}
