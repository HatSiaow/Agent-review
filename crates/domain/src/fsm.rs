use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    PendingReview,
    Approved,
    Edited,
    Rejected,
    Posted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DraftEvent {
    Approve,
    Edit,
    Reject,
    MarkPosted,
    MarkFailed,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid transition: {from:?} -> {event:?}")]
pub struct InvalidTransition {
    pub from: DraftState,
    pub event: DraftEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftFsm {
    state: DraftState,
}

impl DraftFsm {
    #[must_use]
    pub fn new(state: DraftState) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn state(self) -> DraftState {
        self.state
    }

    pub fn apply(mut self, event: DraftEvent) -> Result<Self, InvalidTransition> {
        self.state = transition(self.state, event)?;
        Ok(self)
    }
}

fn transition(from: DraftState, event: DraftEvent) -> Result<DraftState, InvalidTransition> {
    let to = match (from, event) {
        (DraftState::PendingReview, DraftEvent::Approve) => DraftState::Approved,
        (DraftState::PendingReview, DraftEvent::Edit) => DraftState::Edited,
        (DraftState::PendingReview, DraftEvent::Reject) => DraftState::Rejected,

        (DraftState::PendingReview, DraftEvent::MarkPosted | DraftEvent::MarkFailed) => {
            return Err(InvalidTransition { from, event });
        }

        (DraftState::Approved, DraftEvent::Approve) => return Err(InvalidTransition { from, event }),
        (DraftState::Approved, DraftEvent::Edit) => DraftState::Edited,
        (DraftState::Approved, DraftEvent::Reject) => DraftState::Rejected,
        (DraftState::Approved, DraftEvent::MarkPosted) => DraftState::Posted,
        (DraftState::Approved, DraftEvent::MarkFailed) => DraftState::Failed,

        (DraftState::Edited, DraftEvent::Approve) => DraftState::Approved,
        (DraftState::Edited, DraftEvent::Reject) => DraftState::Rejected,
        (DraftState::Edited, DraftEvent::Edit | DraftEvent::MarkPosted | DraftEvent::MarkFailed) => {
            return Err(InvalidTransition { from, event });
        }

        // terminal-ish
        (DraftState::Rejected, _) => return Err(InvalidTransition { from, event }),
        (DraftState::Posted, _) => return Err(InvalidTransition { from, event }),
        (DraftState::Failed, _) => return Err(InvalidTransition { from, event }),
    };
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_can_be_approved() {
        let fsm = DraftFsm::new(DraftState::PendingReview).apply(DraftEvent::Approve);
        assert_eq!(fsm.unwrap().state(), DraftState::Approved);
    }

    #[test]
    fn pending_can_be_rejected() {
        let fsm = DraftFsm::new(DraftState::PendingReview).apply(DraftEvent::Reject);
        assert_eq!(fsm.unwrap().state(), DraftState::Rejected);
    }

    #[test]
    fn approved_can_be_posted() {
        let fsm = DraftFsm::new(DraftState::Approved).apply(DraftEvent::MarkPosted);
        assert_eq!(fsm.unwrap().state(), DraftState::Posted);
    }

    #[test]
    fn posted_is_immutable() {
        let err = DraftFsm::new(DraftState::Posted)
            .apply(DraftEvent::Reject)
            .unwrap_err();
        assert_eq!(
            err,
            InvalidTransition {
                from: DraftState::Posted,
                event: DraftEvent::Reject
            }
        );
    }
}

