use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ReviewStatus;

/// Events that cause a review to change status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    StartDrafting,
    DraftReady,
    DraftingFailed,
    MarkReplied,
    MarkWithdrawn,
    Skip,
    Unskip,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid review transition: {from:?} -> {event:?}")]
pub struct InvalidReviewTransition {
    pub from: ReviewStatus,
    pub event: ReviewEvent,
}

/// State machine for `ReviewStatus` transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewFsm {
    state: ReviewStatus,
}

impl ReviewFsm {
    #[must_use]
    pub fn new(state: ReviewStatus) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn state(self) -> ReviewStatus {
        self.state
    }

    pub fn apply(mut self, event: ReviewEvent) -> Result<Self, InvalidReviewTransition> {
        self.state = transition(self.state, event)?;
        Ok(self)
    }
}

// Each (status, event) arm is listed individually for readability.
#[allow(clippy::match_same_arms)]
fn transition(
    from: ReviewStatus,
    event: ReviewEvent,
) -> Result<ReviewStatus, InvalidReviewTransition> {
    let to = match (from, event) {
        (ReviewStatus::New, ReviewEvent::StartDrafting) => ReviewStatus::Drafting,
        (ReviewStatus::New, ReviewEvent::Skip) => ReviewStatus::Skipped,
        (ReviewStatus::New, ReviewEvent::MarkWithdrawn) => ReviewStatus::Withdrawn,

        (ReviewStatus::Drafting, ReviewEvent::DraftReady) => ReviewStatus::AwaitingHuman,
        (ReviewStatus::Drafting, ReviewEvent::DraftingFailed) => ReviewStatus::New,
        (ReviewStatus::Drafting, ReviewEvent::MarkWithdrawn) => ReviewStatus::Withdrawn,

        (ReviewStatus::AwaitingHuman, ReviewEvent::MarkReplied) => ReviewStatus::Replied,
        (ReviewStatus::AwaitingHuman, ReviewEvent::Skip) => ReviewStatus::Skipped,
        (ReviewStatus::AwaitingHuman, ReviewEvent::StartDrafting) => ReviewStatus::Drafting,
        (ReviewStatus::AwaitingHuman, ReviewEvent::MarkWithdrawn) => ReviewStatus::Withdrawn,

        (ReviewStatus::Skipped, ReviewEvent::Unskip) => ReviewStatus::New,
        (ReviewStatus::Skipped, ReviewEvent::MarkWithdrawn) => ReviewStatus::Withdrawn,

        _ => return Err(InvalidReviewTransition { from, event }),
    };
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_can_start_drafting() {
        let fsm = ReviewFsm::new(ReviewStatus::New)
            .apply(ReviewEvent::StartDrafting)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Drafting);
    }

    #[test]
    fn new_can_skip() {
        let fsm = ReviewFsm::new(ReviewStatus::New)
            .apply(ReviewEvent::Skip)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Skipped);
    }

    #[test]
    fn drafting_to_awaiting_on_draft_ready() {
        let fsm = ReviewFsm::new(ReviewStatus::Drafting)
            .apply(ReviewEvent::DraftReady)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::AwaitingHuman);
    }

    #[test]
    fn drafting_failure_returns_to_new() {
        let fsm = ReviewFsm::new(ReviewStatus::Drafting)
            .apply(ReviewEvent::DraftingFailed)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::New);
    }

    #[test]
    fn awaiting_human_can_be_replied() {
        let fsm = ReviewFsm::new(ReviewStatus::AwaitingHuman)
            .apply(ReviewEvent::MarkReplied)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Replied);
    }

    #[test]
    fn awaiting_human_can_skip() {
        let fsm = ReviewFsm::new(ReviewStatus::AwaitingHuman)
            .apply(ReviewEvent::Skip)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Skipped);
    }

    #[test]
    fn awaiting_human_can_regenerate() {
        let fsm = ReviewFsm::new(ReviewStatus::AwaitingHuman)
            .apply(ReviewEvent::StartDrafting)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Drafting);
    }

    #[test]
    fn skipped_can_be_unskipped() {
        let fsm = ReviewFsm::new(ReviewStatus::Skipped)
            .apply(ReviewEvent::Unskip)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::New);
    }

    #[test]
    fn withdrawn_is_terminal() {
        let err = ReviewFsm::new(ReviewStatus::Withdrawn)
            .apply(ReviewEvent::StartDrafting)
            .unwrap_err();
        assert_eq!(err.from, ReviewStatus::Withdrawn);
    }

    #[test]
    fn replied_is_terminal() {
        let err = ReviewFsm::new(ReviewStatus::Replied)
            .apply(ReviewEvent::Skip)
            .unwrap_err();
        assert_eq!(err.from, ReviewStatus::Replied);
    }

    #[test]
    fn any_active_state_can_be_withdrawn() {
        for status in [
            ReviewStatus::New,
            ReviewStatus::Drafting,
            ReviewStatus::AwaitingHuman,
            ReviewStatus::Skipped,
        ] {
            let fsm = ReviewFsm::new(status)
                .apply(ReviewEvent::MarkWithdrawn)
                .unwrap();
            assert_eq!(fsm.state(), ReviewStatus::Withdrawn);
        }
    }

    #[test]
    fn full_happy_path() {
        let fsm = ReviewFsm::new(ReviewStatus::New)
            .apply(ReviewEvent::StartDrafting)
            .unwrap()
            .apply(ReviewEvent::DraftReady)
            .unwrap()
            .apply(ReviewEvent::MarkReplied)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Replied);
    }

    #[test]
    fn skip_then_unskip_then_draft() {
        let fsm = ReviewFsm::new(ReviewStatus::New)
            .apply(ReviewEvent::Skip)
            .unwrap()
            .apply(ReviewEvent::Unskip)
            .unwrap()
            .apply(ReviewEvent::StartDrafting)
            .unwrap();
        assert_eq!(fsm.state(), ReviewStatus::Drafting);
    }
}
