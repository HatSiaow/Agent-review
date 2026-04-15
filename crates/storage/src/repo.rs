use domain::ReplyDraft;
use domain::RestaurantSettings;
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

/// Queue tab for the owner inbox (`specs/coder/16-frontend-web-ui.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTab {
    NeedsYouNow,
    ReadyToSend,
    History,
}

/// Sort order for `list_reviews_filtered`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewSort {
    #[default]
    UpdatedAtDesc,
    UpdatedAtAsc,
    RatingDesc,
    CreatedAtDesc,
}

/// Filters for listing reviews (API query params).
#[derive(Debug, Clone, Default)]
pub struct ReviewListQuery {
    pub platform: Option<domain::Platform>,
    pub status: Option<domain::ReviewStatus>,
    pub rating: Option<u8>,
    /// Case-insensitive substring match on author display name and review body.
    pub q: Option<String>,
    pub queue: Option<QueueTab>,
    pub sort: ReviewSort,
}

/// Filters for listing drafts.
#[derive(Debug, Clone, Default)]
pub struct DraftListQuery {
    pub state: Option<domain::DraftState>,
    pub rating: Option<u8>,
    /// When `true`, only drafts with at least one guardrail warning.
    pub flag_warnings: Option<bool>,
}

/// Authentication-focused view of a user.
///
/// This is intentionally **not** part of the domain model because it contains
/// sensitive material used for credential verification.
#[derive(Debug, Clone)]
pub struct UserAuth {
    pub user: domain::User,
    pub password_hash: String,
    pub totp_secret: Option<String>,
}

/// Durable work-job type (`specs/coder/17-work-queues-and-outbox-processing.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkJobType {
    AgentDraftReview,
    PosterPostReply,
    NotifierDispatch,
}

impl WorkJobType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentDraftReview => "agent_draft_review",
            Self::PosterPostReply => "poster_post_reply",
            Self::NotifierDispatch => "notifier_dispatch",
        }
    }
}

/// Durable work-job state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkJobState {
    Pending,
    Running,
    Succeeded,
    Failed,
    DeadLetter,
}

impl WorkJobState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::DeadLetter => "dead_letter",
        }
    }
}

/// A durable job claimed from `work_jobs`.
#[derive(Debug, Clone)]
pub struct WorkJob {
    pub id: Uuid,
    pub job_type: WorkJobType,
    pub dedupe_key: String,
    pub payload_json: serde_json::Value,
    pub state: WorkJobState,
    pub attempts: i32,
    pub max_attempts: i32,
    pub run_after: OffsetDateTime,
    pub locked_by: Option<String>,
    pub locked_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

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

    /// Filtered + sorted review list for the JSON API and HTML queue.
    async fn list_reviews_filtered(
        &self,
        query: ReviewListQuery,
    ) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>>;

    async fn get_review(&self, id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)>;

    async fn ingest_review(&self, review: Review) -> RepositoryResult<()>;
    async fn upsert_review_with_draft(
        &self,
        review: Review,
        draft: ReplyDraft,
    ) -> RepositoryResult<()>;

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

    async fn list_drafts_filtered(
        &self,
        query: DraftListQuery,
    ) -> RepositoryResult<Vec<ReplyDraft>>;
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

    // --- Auth / sessions ---

    /// Fetch auth material by email (case-insensitive).
    ///
    /// Implementations must not log the password hash or TOTP secret.
    async fn get_user_auth_by_email(&self, email: &str) -> RepositoryResult<Option<UserAuth>>;

    /// Fetch a user record by id.
    async fn get_user_by_id(&self, user_id: Uuid) -> RepositoryResult<Option<domain::User>>;

    /// All users for the single-restaurant deployment (owner UI).
    async fn list_users(&self) -> RepositoryResult<Vec<domain::User>>;

    /// Persisted restaurant profile (singleton). Missing row uses domain defaults.
    async fn get_restaurant_settings(&self) -> RepositoryResult<RestaurantSettings>;

    /// Replace settings from a full merged value (caller merges patch + defaults).
    async fn put_restaurant_settings(&self, settings: RestaurantSettings) -> RepositoryResult<()>;

    /// Create a new server-side session.
    async fn create_session(&self, session: domain::Session) -> RepositoryResult<()>;

    /// Lookup a session and its user if still valid at `now`.
    async fn get_session_user(
        &self,
        session_id: Uuid,
        now: OffsetDateTime,
    ) -> RepositoryResult<Option<(domain::Session, domain::User)>>;

    /// Extend a session expiry ("sliding" sessions).
    async fn touch_session(
        &self,
        session_id: Uuid,
        new_expires_at: OffsetDateTime,
    ) -> RepositoryResult<()>;

    /// Revoke a session.
    async fn delete_session(&self, session_id: Uuid) -> RepositoryResult<()>;

    // --- Password reset ---

    /// Create a password reset token for a user. Only the SHA-256 hash is stored.
    /// Returns the generated token id. Callers are responsible for delivering the raw
    /// token to the user (e.g. via email). Expires after 30 minutes.
    async fn create_password_reset_token(
        &self,
        user_id: Uuid,
        token_hash: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> RepositoryResult<Uuid>;

    /// Atomically verify and consume a password reset token.
    ///
    /// Returns the `user_id` when the token is valid (not expired, not yet used).
    /// Marks the token as used by setting `used_at`. Returns `None` if the token
    /// is unknown, already used, or expired.
    async fn consume_password_reset_token(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Option<Uuid>>;

    /// Replace a user's stored password hash (called after a successful reset).
    /// Returns `RepositoryError::NotFound` if `user_id` does not exist.
    async fn update_user_password_hash(
        &self,
        user_id: Uuid,
        new_password_hash: &str,
    ) -> RepositoryResult<()>;

    /// Garbage-collect expired and already-used password reset tokens.
    /// Deletes rows where `expires_at < cutoff` or `used_at is not null`.
    /// Returns the number of rows deleted.
    async fn gc_delete_expired_reset_tokens(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64>;

    // --- Notifications outbox ---

    /// Enqueue a notification delivery request into the durable outbox.
    ///
    /// This is the persistence boundary that makes notifications restart-safe.
    async fn enqueue_notification_outbox(
        &self,
        id: Uuid,
        occurred_at: OffsetDateTime,
        notification_type: domain::NotificationType,
        review_id: Option<Uuid>,
        draft_id: Option<Uuid>,
        payload_json: serde_json::Value,
    ) -> RepositoryResult<()>;

    /// Claim a batch of unsent outbox rows for delivery.
    ///
    /// Implementations must ensure that concurrent workers can safely claim
    /// distinct rows, and that a claimed row is not returned again until it is
    /// either marked sent or its claim expires (v0.1 uses a simple claim marker).
    async fn claim_notification_outbox_batch(
        &self,
        limit: u32,
        claimed_by: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<NotificationOutboxItem>>;

    /// Mark an outbox item as sent.
    async fn mark_notification_outbox_sent(
        &self,
        id: Uuid,
        sent_at: OffsetDateTime,
    ) -> RepositoryResult<()>;

    /// Release stale notification outbox claims held for longer than the lease window.
    ///
    /// Rows whose `claimed_at` is before `claim_cutoff` and whose `sent_at` is null
    /// are returned to claimable state by clearing `claimed_at`/`claimed_by`.
    /// This handles crash recovery when a worker claimed rows but never marked them sent.
    ///
    /// Returns the number of rows released.
    async fn release_stale_notification_claims(
        &self,
        claim_cutoff: OffsetDateTime,
    ) -> RepositoryResult<u64>;

    // --- Durable work jobs (spec 17) ---

    /// Enqueue a durable work job, idempotent by `(job_type, dedupe_key)`.
    async fn enqueue_work_job(
        &self,
        id: Uuid,
        job_type: WorkJobType,
        dedupe_key: &str,
        payload_json: serde_json::Value,
        run_after: OffsetDateTime,
        max_attempts: i32,
        now: OffsetDateTime,
    ) -> RepositoryResult<()>;

    /// Claim up to `limit` pending jobs due at `now`, transitioning them to `running`.
    async fn claim_work_jobs(
        &self,
        job_type: WorkJobType,
        limit: u32,
        locked_by: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<WorkJob>>;

    async fn mark_work_job_succeeded(
        &self,
        job_id: Uuid,
        now: OffsetDateTime,
    ) -> RepositoryResult<()>;

    /// Mark a job as failed (and schedule retry) or dead-letter it.
    async fn mark_work_job_failed(
        &self,
        job_id: Uuid,
        error: &str,
        next_state: WorkJobState,
        run_after: OffsetDateTime,
        now: OffsetDateTime,
    ) -> RepositoryResult<()>;

    // --- Garbage collection ---

    /// Redact `raw_payload` on reviews older than `cutoff`, replacing it with `{}`.
    ///
    /// Returns the number of rows affected.
    async fn gc_redact_raw_payloads(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64>;

    /// Delete `agent_runs` rows older than `cutoff`.
    ///
    /// Returns the number of rows deleted.
    async fn gc_delete_agent_runs(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64>;

    /// Delete sent `notifications_outbox` rows whose `sent_at` is older than `cutoff`.
    ///
    /// Returns the number of rows deleted.
    async fn gc_delete_sent_notifications(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64>;

    /// Delete `webhook_events` rows older than `cutoff`.
    ///
    /// Returns the number of rows deleted.
    async fn gc_delete_old_webhook_events(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64>;

    /// Delete expired `idempotency_responses` rows older than `cutoff`.
    ///
    /// Returns the number of rows deleted.
    async fn gc_delete_expired_idempotency_keys(
        &self,
        cutoff: OffsetDateTime,
    ) -> RepositoryResult<u64>;
}

/// A row claimed from `notifications_outbox` for delivery.
#[derive(Debug, Clone)]
pub struct NotificationOutboxItem {
    pub id: Uuid,
    pub occurred_at: OffsetDateTime,
    pub notification_type: domain::NotificationType,
    pub review_id: Option<Uuid>,
    pub draft_id: Option<Uuid>,
    pub payload_json: serde_json::Value,
}
