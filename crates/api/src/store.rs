use std::sync::Arc;

use domain::{ReplyDraft, Review};
use uuid::Uuid;

use crate::problem::ApiError;

#[derive(Clone)]
pub struct Store {
    repo: Arc<dyn storage::Repository>,
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

    fn map_err(err: storage::RepositoryError) -> ApiError {
        match err {
            storage::RepositoryError::NotFound => ApiError::NotFound,
            storage::RepositoryError::InvalidTransition => ApiError::InvalidTransition,
            storage::RepositoryError::Conflict("draft_has_guardrail_warnings") => {
                ApiError::BadRequest("cannot bulk-approve drafts with guardrail warnings")
            }
            storage::RepositoryError::Conflict("bulk_approve_requires_5_star") => {
                ApiError::BadRequest("bulk-approve is only allowed for 5-star reviews")
            }
            storage::RepositoryError::Conflict(_) | storage::RepositoryError::Storage(_) => {
                ApiError::BadRequest("storage error")
            }
        }
    }

    pub async fn list_reviews(&self) -> Vec<(Review, Option<ReplyDraft>)> {
        self.repo.list_reviews().await.unwrap_or_default()
    }

    pub async fn get_review(&self, id: Uuid) -> Result<(Review, Option<ReplyDraft>), ApiError> {
        self.repo.get_review(id).await.map_err(Self::map_err)
    }

    pub async fn upsert_review_with_draft(&self, review: Review, draft: ReplyDraft) {
        let _ = self.repo.upsert_review_with_draft(review, draft).await;
    }

    pub async fn ingest_review(&self, review: Review) {
        let _ = self.repo.ingest_review(review).await;
    }

    pub async fn store_agent_draft(&self, draft: ReplyDraft) {
        let _ = self.repo.store_agent_draft(draft).await;
    }

    pub async fn transition_review_to_drafting(&self, review_id: Uuid) -> Result<Review, ApiError> {
        self.repo
            .transition_review_to_drafting(review_id)
            .await
            .map_err(Self::map_err)
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
            .map_err(Self::map_err)
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
            .map_err(Self::map_err)
    }

    pub async fn skip_review(&self, review_id: Uuid) -> Result<Review, ApiError> {
        self.repo.skip_review(review_id).await.map_err(Self::map_err)
    }

    pub async fn unskip_review(&self, review_id: Uuid) -> Result<Review, ApiError> {
        self.repo
            .unskip_review(review_id)
            .await
            .map_err(Self::map_err)
    }

    pub async fn bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
    ) -> Result<Vec<ReplyDraft>, ApiError> {
        self.repo
            .bulk_approve(draft_ids, reviewed_by)
            .await
            .map_err(Self::map_err)
    }

    pub async fn mark_draft_posted(
        &self,
        draft_id: Uuid,
        posted_at: time::OffsetDateTime,
    ) -> Result<ReplyDraft, ApiError> {
        self.repo
            .mark_draft_posted(draft_id, posted_at)
            .await
            .map_err(Self::map_err)
    }

    pub async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> Result<ReplyDraft, ApiError> {
        self.repo
            .mark_draft_post_failed(draft_id, error)
            .await
            .map_err(Self::map_err)
    }
}
