use domain::{DraftEvent, DraftFsm, ReplyDraft, Review, ReviewStatus};
use std::collections::HashMap;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::problem::ApiError;

#[derive(Debug, Clone)]
pub struct Store(Arc<Mutex<State>>);

#[derive(Debug)]
struct State {
    reviews: HashMap<Uuid, Review>,
    drafts: HashMap<Uuid, ReplyDraft>,
    review_to_active_draft: HashMap<Uuid, Uuid>,
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
            if new_text.is_some() {
                fsm = fsm.apply(DraftEvent::Edit).map_err(|_| ApiError::InvalidTransition)?;
                draft.text = new_text.expect("checked some");
                draft.char_count = u32::try_from(draft.text.chars().count()).unwrap_or(u32::MAX);
                draft.state = fsm.state();
                draft.generated_by = domain::Generator::HumanEdit;
            }

            fsm = fsm.apply(DraftEvent::Approve).map_err(|_| ApiError::InvalidTransition)?;
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
}

#[derive(Debug, Clone)]
pub struct InMemoryStore {
    store: Store,
}

impl InMemoryStore {
    #[must_use]
    pub fn new_seeded() -> Self {
        let store = Store::new();
        Self { store }
    }

    #[must_use]
    pub fn store(&self) -> Store {
        self.store.clone()
    }
}

