use domain::ReplyDraft;
use domain::Review;
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
    async fn list_reviews(&self) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>>;
    async fn get_review(&self, id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)>;

    async fn ingest_review(&self, review: Review) -> RepositoryResult<()>;
    async fn upsert_review_with_draft(&self, review: Review, draft: ReplyDraft)
        -> RepositoryResult<()>;

    async fn transition_review_to_drafting(&self, review_id: Uuid) -> RepositoryResult<Review>;
    async fn skip_review(&self, review_id: Uuid) -> RepositoryResult<Review>;
    async fn unskip_review(&self, review_id: Uuid) -> RepositoryResult<Review>;

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>>;
    async fn store_agent_draft(&self, draft: ReplyDraft) -> RepositoryResult<()>;
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

