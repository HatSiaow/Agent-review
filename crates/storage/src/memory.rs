use std::collections::HashMap;
use std::sync::Arc;

use domain::{
    DraftEvent, DraftFsm, Generator, ReplyDraft, Review, ReviewEvent, ReviewFsm, ReviewStatus,
};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::repo::{Repository, RepositoryError, RepositoryResult};

#[derive(Debug, Clone, Default)]
pub struct InMemoryRepository(Arc<Mutex<State>>);

#[derive(Debug, Default)]
struct State {
    reviews: HashMap<Uuid, Review>,
    drafts: HashMap<Uuid, ReplyDraft>,
    review_to_active_draft: HashMap<Uuid, Uuid>,
}

impl InMemoryRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl InMemoryRepository {
    fn api_err(err: RepositoryError) -> RepositoryError {
        err
    }
}

impl Repository for InMemoryRepository {
    async fn list_reviews(&self) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>> {
        let state = self.0.lock().await;
        Ok(state
            .reviews
            .values()
            .cloned()
            .map(|r| {
                let active = state
                    .review_to_active_draft
                    .get(&r.id)
                    .and_then(|id| state.drafts.get(id))
                    .cloned();
                (r, active)
            })
            .collect())
    }

    async fn get_review(&self, id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)> {
        let state = self.0.lock().await;
        let review = state.reviews.get(&id).cloned().ok_or(RepositoryError::NotFound)?;
        let active = state
            .review_to_active_draft
            .get(&id)
            .and_then(|draft_id| state.drafts.get(draft_id))
            .cloned();
        Ok((review, active))
    }

    async fn ingest_review(&self, review: Review) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        let existing = state.reviews.values().any(|r| {
            r.platform == review.platform && r.source_review_id == review.source_review_id
        });
        if existing {
            return Ok(());
        }
        state.reviews.insert(review.id, review);
        Ok(())
    }

    async fn upsert_review_with_draft(&self, review: Review, draft: ReplyDraft) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state.review_to_active_draft.insert(review.id, draft.id);
        state.reviews.insert(review.id, review);
        state.drafts.insert(draft.id, draft);
        Ok(())
    }

    async fn transition_review_to_drafting(&self, review_id: Uuid) -> RepositoryResult<Review> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(RepositoryError::NotFound)?;

        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::StartDrafting)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        review.status = fsm.state();
        let result = review.clone();

        state.review_to_active_draft.remove(&review_id);
        Ok(result)
    }

    async fn skip_review(&self, review_id: Uuid) -> RepositoryResult<Review> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(RepositoryError::NotFound)?;
        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::Skip)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        review.status = fsm.state();
        Ok(review.clone())
    }

    async fn unskip_review(&self, review_id: Uuid) -> RepositoryResult<Review> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(RepositoryError::NotFound)?;
        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::Unskip)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        review.status = fsm.state();
        Ok(review.clone())
    }

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>> {
        let state = self.0.lock().await;
        Ok(state.drafts.values().cloned().collect())
    }

    async fn store_agent_draft(&self, draft: ReplyDraft) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        let review_id = draft.review_id;
        state.review_to_active_draft.insert(review_id, draft.id);
        state.drafts.insert(draft.id, draft);
        if let Some(review) = state.reviews.get_mut(&review_id) {
            review.status = ReviewStatus::AwaitingHuman;
        }
        Ok(())
    }

    async fn approve_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        new_text: Option<String>,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let (review_id, updated) = {
            let draft = state
                .drafts
                .get_mut(&draft_id)
                .ok_or(RepositoryError::NotFound)?;
            let review_id = draft.review_id;

            let mut fsm = DraftFsm::new(draft.state);
            if let Some(text) = new_text {
                fsm = fsm
                    .apply(DraftEvent::Edit)
                    .map_err(|_| RepositoryError::InvalidTransition)?;
                draft.text = text;
                draft.char_count = u32::try_from(draft.text.chars().count()).unwrap_or(u32::MAX);
                draft.state = fsm.state();
                draft.generated_by = Generator::HumanEdit;
            }

            fsm = fsm
                .apply(DraftEvent::Approve)
                .map_err(|_| RepositoryError::InvalidTransition)?;
            draft.state = fsm.state();
            draft.reviewed_by = Some(reviewed_by);
            draft.reviewed_at = Some(OffsetDateTime::now_utc());

            (review_id, draft.clone())
        };

        if let Some(review) = state.reviews.get_mut(&review_id) {
            review.status = ReviewStatus::AwaitingHuman;
        }

        Ok(updated)
    }

    async fn reject_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        reason: String,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let draft = state
            .drafts
            .get_mut(&draft_id)
            .ok_or(RepositoryError::NotFound)?;

        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::Reject)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        draft.state = fsm.state();
        draft.reviewed_by = Some(reviewed_by);
        draft.reviewed_at = Some(OffsetDateTime::now_utc());
        draft.rejection_reason = Some(reason);
        Ok(draft.clone())
    }

    async fn bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let mut state = self.0.lock().await;
        let mut results = Vec::with_capacity(draft_ids.len());

        for &draft_id in draft_ids {
            let draft = state.drafts.get(&draft_id).ok_or(RepositoryError::NotFound)?;

            if !draft.guardrail_warnings.is_empty() {
                return Err(RepositoryError::Conflict("draft_has_guardrail_warnings"));
            }

            let review = state
                .reviews
                .get(&draft.review_id)
                .ok_or(RepositoryError::NotFound)?;
            if review.rating != 5 {
                return Err(RepositoryError::Conflict("bulk_approve_requires_5_star"));
            }

            let fsm = DraftFsm::new(draft.state)
                .apply(DraftEvent::Approve)
                .map_err(|_| RepositoryError::InvalidTransition)?;

            let draft = state
                .drafts
                .get_mut(&draft_id)
                .ok_or(RepositoryError::NotFound)?;
            draft.state = fsm.state();
            draft.reviewed_by = Some(reviewed_by);
            draft.reviewed_at = Some(OffsetDateTime::now_utc());
            results.push(draft.clone());
        }

        Ok(results)
    }

    async fn mark_draft_posted(
        &self,
        draft_id: Uuid,
        posted_at: OffsetDateTime,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let draft = state
            .drafts
            .get_mut(&draft_id)
            .ok_or(RepositoryError::NotFound)?;
        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::MarkPosted)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        draft.state = fsm.state();
        draft.posted_at = Some(posted_at);
        Ok(draft.clone())
    }

    async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let draft = state
            .drafts
            .get_mut(&draft_id)
            .ok_or(RepositoryError::NotFound)?;
        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::Fail)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        draft.state = fsm.state();
        draft.platform_post_error = Some(error);
        Ok(draft.clone())
    }
}

