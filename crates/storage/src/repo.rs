use domain::ReplyDraft;
use domain::Review;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("not found")]
    NotFound,
    #[error("invalid transition")]
    InvalidTransition,
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("storage error: {0}")]
    Storage(String),
}

pub type RepositoryResult<T> = Result<T, RepositoryError>;

/// Storage boundary for reviews + drafts.
///
/// This abstraction lets the API and background workers operate against either
/// an in-memory store (tests/dev) or Postgres (production).
#[async_trait::async_trait]
pub trait Repository: Send + Sync + 'static {
    /// Lightweight dependency check for readiness probes.
    ///
    /// Implementations should perform a cheap round-trip (e.g. `SELECT 1` for Postgres).
    async fn ping(&self) -> RepositoryResult<()>;

    async fn list_reviews(&self) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>>;
    async fn get_review(&self, id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)>;

    async fn ingest_review(&self, review: Review) -> RepositoryResult<()>;
    async fn upsert_review_with_draft(&self, review: Review, draft: ReplyDraft)
        -> RepositoryResult<()>;

    /// Get the last-seen update watermark for a platform sync.
    ///
    /// Used to implement "stop-at-cursor" polling for adapters that return results
    /// ordered by most-recent `updated_at` (e.g., Google Business Profile).
    async fn get_reviews_sync_state(
        &self,
        platform: domain::Platform,
    ) -> RepositoryResult<Option<OffsetDateTime>>;

    /// Persist the last-seen update watermark for a platform sync.
    ///
    /// Callers should write the maximum `updated_at` observed during a successful poll.
    async fn set_reviews_sync_state(
        &self,
        platform: domain::Platform,
        last_seen_update_time: OffsetDateTime,
    ) -> RepositoryResult<()>;

    /// Register a webhook delivery for replay protection.
    ///
    /// Returns `true` if this event id was not seen before (caller should process),
    /// or `false` if it is a replay/duplicate (caller should no-op but still return 200).
    ///
    /// Implementations should retain the record for at least 24 hours.
    async fn register_webhook_event(
        &self,
        platform: domain::Platform,
        event_id: &str,
        received_at: OffsetDateTime,
    ) -> RepositoryResult<bool>;

    /// List audit events for a specific entity.
    async fn list_audit_events(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> RepositoryResult<Vec<domain::AuditEvent>>;

    /// Get a previously-stored idempotent response payload for `idempotency_key`.
    async fn get_idempotency_response(
        &self,
        idempotency_key: &str,
    ) -> RepositoryResult<Option<(u16, serde_json::Value)>>;

    /// Store an idempotent response payload for `idempotency_key` if not already present.
    async fn put_idempotency_response(
        &self,
        idempotency_key: &str,
        status: u16,
        body_json: serde_json::Value,
        created_at: OffsetDateTime,
    ) -> RepositoryResult<()>;

    async fn transition_review_to_drafting(&self, review_id: Uuid) -> RepositoryResult<Review>;
    async fn skip_review(&self, review_id: Uuid) -> RepositoryResult<Review>;
    async fn unskip_review(&self, review_id: Uuid) -> RepositoryResult<Review>;

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>>;
    async fn store_agent_draft(&self, draft: ReplyDraft) -> RepositoryResult<()>;

    /// Persist a trace record for a single agent run.
    async fn store_agent_run(&self, run: domain::AgentRun) -> RepositoryResult<()>;

    /// List agent runs for a review, most recent first.
    async fn list_agent_runs(&self, review_id: Uuid) -> RepositoryResult<Vec<domain::AgentRun>>;

    async fn approve_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        new_text: Option<String>,
    ) -> RepositoryResult<ReplyDraft>;
    async fn reject_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        reason: String,
    ) -> RepositoryResult<ReplyDraft>;
    async fn bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
    ) -> RepositoryResult<Vec<ReplyDraft>>;

    /// Undo a previous bulk-approve during the undo window.
    async fn undo_bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
        now: time::OffsetDateTime,
    ) -> RepositoryResult<Vec<ReplyDraft>>;

    async fn mark_draft_posted(
        &self,
        draft_id: Uuid,
        posted_at: time::OffsetDateTime,
    ) -> RepositoryResult<ReplyDraft>;

    async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> RepositoryResult<ReplyDraft>;
}

