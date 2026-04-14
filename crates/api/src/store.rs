use std::collections::HashMap;
use std::sync::Arc;

use domain::{
    DraftEvent, DraftFsm, Generator, ReplyDraft, Review, ReviewEvent, ReviewFsm, ReviewStatus,
};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::problem::ApiError;

/// In-memory store used until a real database backend is wired.
#[derive(Debug, Clone)]
pub struct Store(Arc<Mutex<State>>);

#[derive(Debug)]
struct State {
    reviews: HashMap<Uuid, Review>,
    drafts: HashMap<Uuid, ReplyDraft>,
    review_to_active_draft: HashMap<Uuid, Uuid>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            reviews: HashMap::new(),
            drafts: HashMap::new(),
            review_to_active_draft: HashMap::new(),
        })))
    }

    pub async fn list_reviews(&self) -> Vec<(Review, Option<ReplyDraft>)> {
        let state = self.0.lock().await;
        state
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
            .collect()
    }

    pub async fn get_review(&self, id: Uuid) -> Result<(Review, Option<ReplyDraft>), ApiError> {
        let state = self.0.lock().await;
        let review = state.reviews.get(&id).cloned().ok_or(ApiError::NotFound)?;
        let active = state
            .review_to_active_draft
            .get(&id)
            .and_then(|draft_id| state.drafts.get(draft_id))
            .cloned();
        Ok((review, active))
    }

    pub async fn upsert_review_with_draft(&self, review: Review, draft: ReplyDraft) {
        let mut state = self.0.lock().await;
        state.review_to_active_draft.insert(review.id, draft.id);
        state.reviews.insert(review.id, review);
        state.drafts.insert(draft.id, draft);
    }

    pub async fn list_drafts(&self) -> Vec<ReplyDraft> {
        let state = self.0.lock().await;
        state.drafts.values().cloned().collect()
    }

    pub async fn approve_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        new_text: Option<String>,
    ) -> Result<ReplyDraft, ApiError> {
        let mut state = self.0.lock().await;
        let (review_id, updated) = {
            let draft = state.drafts.get_mut(&draft_id).ok_or(ApiError::NotFound)?;
            let review_id = draft.review_id;

            let mut fsm = DraftFsm::new(draft.state);
            if let Some(text) = new_text {
                fsm = fsm
                    .apply(DraftEvent::Edit)
                    .map_err(|_| ApiError::InvalidTransition)?;
                draft.text = text;
                draft.char_count =
                    u32::try_from(draft.text.chars().count()).unwrap_or(u32::MAX);
                draft.state = fsm.state();
                draft.generated_by = Generator::HumanEdit;
            }

            fsm = fsm
                .apply(DraftEvent::Approve)
                .map_err(|_| ApiError::InvalidTransition)?;
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

    pub async fn reject_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        reason: String,
    ) -> Result<ReplyDraft, ApiError> {
        let mut state = self.0.lock().await;
        let draft = state.drafts.get_mut(&draft_id).ok_or(ApiError::NotFound)?;

        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::Reject)
            .map_err(|_| ApiError::InvalidTransition)?;
        draft.state = fsm.state();
        draft.reviewed_by = Some(reviewed_by);
        draft.reviewed_at = Some(OffsetDateTime::now_utc());
        draft.rejection_reason = Some(reason);

        Ok(draft.clone())
    }

    pub async fn skip_review(&self, review_id: Uuid) -> Result<Review, ApiError> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(ApiError::NotFound)?;

        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::Skip)
            .map_err(|_| ApiError::InvalidTransition)?;
        review.status = fsm.state();

        Ok(review.clone())
    }

    pub async fn unskip_review(&self, review_id: Uuid) -> Result<Review, ApiError> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(ApiError::NotFound)?;

        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::Unskip)
            .map_err(|_| ApiError::InvalidTransition)?;
        review.status = fsm.state();

        Ok(review.clone())
    }

    pub async fn bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
    ) -> Result<Vec<ReplyDraft>, ApiError> {
        let mut state = self.0.lock().await;
        let mut results = Vec::with_capacity(draft_ids.len());

        for &draft_id in draft_ids {
            let draft = state.drafts.get(&draft_id).ok_or(ApiError::NotFound)?;

            // Only 5-star, no-warning drafts can be bulk-approved
            if !draft.guardrail_warnings.is_empty() {
                return Err(ApiError::BadRequest(
                    "cannot bulk-approve drafts with guardrail warnings",
                ));
            }

            let review = state
                .reviews
                .get(&draft.review_id)
                .ok_or(ApiError::NotFound)?;
            if review.rating != 5 {
                return Err(ApiError::BadRequest(
                    "bulk-approve is only allowed for 5-star reviews",
                ));
            }

            let fsm = DraftFsm::new(draft.state)
                .apply(DraftEvent::Approve)
                .map_err(|_| ApiError::InvalidTransition)?;

            let draft = state.drafts.get_mut(&draft_id).ok_or(ApiError::NotFound)?;
            draft.state = fsm.state();
            draft.reviewed_by = Some(reviewed_by);
            draft.reviewed_at = Some(OffsetDateTime::now_utc());
            results.push(draft.clone());
        }

        Ok(results)
    }
}
