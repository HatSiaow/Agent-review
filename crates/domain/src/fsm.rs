use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// States a reply draft can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    PendingReview,
    Approved,
    ApprovedPendingUndo,
    Edited,
    Rejected,
    Posted,
    Failed,
}

impl DraftState {
    /// Whether this state is terminal (no further transitions allowed).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Rejected | Self::Posted | Self::Failed)
    }
}

impl fmt::Display for DraftState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PendingReview => f.write_str("pending_review"),
            Self::Approved => f.write_str("approved"),
            Self::ApprovedPendingUndo => f.write_str("approved_pending_undo"),
            Self::Edited => f.write_str("edited"),
            Self::Rejected => f.write_str("rejected"),
            Self::Posted => f.write_str("posted"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// Events that drive draft state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DraftEvent {
    Approve,
    BulkApprove,
    UndoBulkApprove,
    Edit,
    Reject,
    MarkPosted,
    MarkFailed,
}

impl fmt::Display for DraftEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approve => f.write_str("approve"),
            Self::BulkApprove => f.write_str("bulk_approve"),
            Self::UndoBulkApprove => f.write_str("undo_bulk_approve"),
            Self::Edit => f.write_str("edit"),
            Self::Reject => f.write_str("reject"),
            Self::MarkPosted => f.write_str("mark_posted"),
            Self::MarkFailed => f.write_str("mark_failed"),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid transition: {from} -> {event}")]
pub struct InvalidTransition {
    pub from: DraftState,
    pub event: DraftEvent,
}

/// Typed state machine for `DraftState` transitions.
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

// Each (state, event) pair is listed individually for readability — the FSM
// is the single source of truth for allowed transitions and merging arms would
// obscure which source state produced which target state.
#[allow(clippy::match_same_arms)]
fn transition(from: DraftState, event: DraftEvent) -> Result<DraftState, InvalidTransition> {
    let to = match (from, event) {
        // PendingReview
        (DraftState::PendingReview, DraftEvent::Approve) => DraftState::Approved,
        (DraftState::PendingReview, DraftEvent::BulkApprove) => DraftState::ApprovedPendingUndo,
        (DraftState::PendingReview, DraftEvent::UndoBulkApprove) => {
            return Err(InvalidTransition { from, event });
        }
        (DraftState::PendingReview, DraftEvent::Edit) => DraftState::Edited,
        (DraftState::PendingReview, DraftEvent::Reject) => DraftState::Rejected,
        (DraftState::PendingReview, DraftEvent::MarkPosted | DraftEvent::MarkFailed) => {
            return Err(InvalidTransition { from, event });
        }

        // Approved
        (DraftState::Approved, DraftEvent::Approve) => {
            return Err(InvalidTransition { from, event });
        }
        (DraftState::Approved, DraftEvent::BulkApprove | DraftEvent::UndoBulkApprove) => {
            return Err(InvalidTransition { from, event });
        }
        (DraftState::Approved, DraftEvent::Edit) => DraftState::Edited,
        (DraftState::Approved, DraftEvent::Reject) => DraftState::Rejected,
        (DraftState::Approved, DraftEvent::MarkPosted) => DraftState::Posted,
        (DraftState::Approved, DraftEvent::MarkFailed) => DraftState::Failed,

        // ApprovedPendingUndo
        (DraftState::ApprovedPendingUndo, DraftEvent::UndoBulkApprove) => DraftState::PendingReview,
        (DraftState::ApprovedPendingUndo, DraftEvent::Edit) => DraftState::Edited,
        (DraftState::ApprovedPendingUndo, DraftEvent::Reject) => DraftState::Rejected,
        (DraftState::ApprovedPendingUndo, DraftEvent::MarkPosted) => DraftState::Posted,
        (DraftState::ApprovedPendingUndo, DraftEvent::MarkFailed) => DraftState::Failed,
        (DraftState::ApprovedPendingUndo, DraftEvent::Approve | DraftEvent::BulkApprove) => {
            return Err(InvalidTransition { from, event });
        }

        // Edited
        (DraftState::Edited, DraftEvent::Approve) => DraftState::Approved,
        (DraftState::Edited, DraftEvent::BulkApprove) => DraftState::ApprovedPendingUndo,
        (DraftState::Edited, DraftEvent::UndoBulkApprove) => {
            return Err(InvalidTransition { from, event });
        }
        (DraftState::Edited, DraftEvent::Reject) => DraftState::Rejected,
        (
            DraftState::Edited,
            DraftEvent::Edit | DraftEvent::MarkPosted | DraftEvent::MarkFailed,
        ) => {
            return Err(InvalidTransition { from, event });
        }

        // Terminal states
        (DraftState::Rejected | DraftState::Posted | DraftState::Failed, _) => {
            return Err(InvalidTransition { from, event });
        }
    };
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(from: DraftState, event: DraftEvent) -> Result<DraftState, InvalidTransition> {
        DraftFsm::new(from).apply(event).map(DraftFsm::state)
    }

    // --- PendingReview transitions ---

    #[test]
    fn pending_approve() {
        assert_eq!(
            apply(DraftState::PendingReview, DraftEvent::Approve),
            Ok(DraftState::Approved)
        );
    }

    #[test]
    fn pending_edit() {
        assert_eq!(
            apply(DraftState::PendingReview, DraftEvent::Edit),
            Ok(DraftState::Edited)
        );
    }

    #[test]
    fn pending_reject() {
        assert_eq!(
            apply(DraftState::PendingReview, DraftEvent::Reject),
            Ok(DraftState::Rejected)
        );
    }

    #[test]
    fn pending_mark_posted_invalid() {
        assert!(apply(DraftState::PendingReview, DraftEvent::MarkPosted).is_err());
    }

    #[test]
    fn pending_mark_failed_invalid() {
        assert!(apply(DraftState::PendingReview, DraftEvent::MarkFailed).is_err());
    }

    // --- Approved transitions ---

    #[test]
    fn approved_approve_invalid() {
        assert!(apply(DraftState::Approved, DraftEvent::Approve).is_err());
    }

    #[test]
    fn approved_edit() {
        assert_eq!(
            apply(DraftState::Approved, DraftEvent::Edit),
            Ok(DraftState::Edited)
        );
    }

    #[test]
    fn approved_reject() {
        assert_eq!(
            apply(DraftState::Approved, DraftEvent::Reject),
            Ok(DraftState::Rejected)
        );
    }

    #[test]
    fn approved_mark_posted() {
        assert_eq!(
            apply(DraftState::Approved, DraftEvent::MarkPosted),
            Ok(DraftState::Posted)
        );
    }

    #[test]
    fn approved_mark_failed() {
        assert_eq!(
            apply(DraftState::Approved, DraftEvent::MarkFailed),
            Ok(DraftState::Failed)
        );
    }

    // --- Edited transitions ---

    #[test]
    fn edited_approve() {
        assert_eq!(
            apply(DraftState::Edited, DraftEvent::Approve),
            Ok(DraftState::Approved)
        );
    }

    #[test]
    fn edited_edit_invalid() {
        assert!(apply(DraftState::Edited, DraftEvent::Edit).is_err());
    }

    #[test]
    fn edited_reject() {
        assert_eq!(
            apply(DraftState::Edited, DraftEvent::Reject),
            Ok(DraftState::Rejected)
        );
    }

    #[test]
    fn edited_mark_posted_invalid() {
        assert!(apply(DraftState::Edited, DraftEvent::MarkPosted).is_err());
    }

    #[test]
    fn edited_mark_failed_invalid() {
        assert!(apply(DraftState::Edited, DraftEvent::MarkFailed).is_err());
    }

    // --- Terminal states: every event is rejected ---

    #[test]
    fn rejected_is_terminal() {
        for event in [
            DraftEvent::Approve,
            DraftEvent::BulkApprove,
            DraftEvent::UndoBulkApprove,
            DraftEvent::Edit,
            DraftEvent::Reject,
            DraftEvent::MarkPosted,
            DraftEvent::MarkFailed,
        ] {
            assert!(apply(DraftState::Rejected, event).is_err());
        }
    }

    #[test]
    fn posted_is_immutable() {
        for event in [
            DraftEvent::Approve,
            DraftEvent::BulkApprove,
            DraftEvent::UndoBulkApprove,
            DraftEvent::Edit,
            DraftEvent::Reject,
            DraftEvent::MarkPosted,
            DraftEvent::MarkFailed,
        ] {
            assert!(apply(DraftState::Posted, event).is_err());
        }
    }

    #[test]
    fn failed_is_terminal() {
        for event in [
            DraftEvent::Approve,
            DraftEvent::BulkApprove,
            DraftEvent::UndoBulkApprove,
            DraftEvent::Edit,
            DraftEvent::Reject,
            DraftEvent::MarkPosted,
            DraftEvent::MarkFailed,
        ] {
            assert!(apply(DraftState::Failed, event).is_err());
        }
    }

    // --- Display & serialization ---

    #[test]
    fn draft_state_display() {
        assert_eq!(DraftState::PendingReview.to_string(), "pending_review");
        assert_eq!(DraftState::Approved.to_string(), "approved");
        assert_eq!(DraftState::Posted.to_string(), "posted");
    }

    #[test]
    fn draft_event_display() {
        assert_eq!(DraftEvent::MarkPosted.to_string(), "mark_posted");
    }

    #[test]
    fn is_terminal() {
        assert!(!DraftState::PendingReview.is_terminal());
        assert!(!DraftState::Approved.is_terminal());
        assert!(!DraftState::Edited.is_terminal());
        assert!(DraftState::Rejected.is_terminal());
        assert!(DraftState::Posted.is_terminal());
        assert!(DraftState::Failed.is_terminal());
    }

    #[test]
    fn invalid_transition_error_message() {
        let err = apply(DraftState::Posted, DraftEvent::Approve).unwrap_err();
        assert_eq!(err.to_string(), "invalid transition: posted -> approve");
    }

    // --- Multi-step paths ---

    #[test]
    fn happy_path_pending_to_posted() {
        let fsm = DraftFsm::new(DraftState::PendingReview)
            .apply(DraftEvent::Approve)
            .unwrap()
            .apply(DraftEvent::MarkPosted)
            .unwrap();
        assert_eq!(fsm.state(), DraftState::Posted);
    }

    #[test]
    fn edit_then_approve_then_post() {
        let fsm = DraftFsm::new(DraftState::PendingReview)
            .apply(DraftEvent::Edit)
            .unwrap()
            .apply(DraftEvent::Approve)
            .unwrap()
            .apply(DraftEvent::MarkPosted)
            .unwrap();
        assert_eq!(fsm.state(), DraftState::Posted);
    }

    #[test]
    fn approved_reverted_via_edit_then_re_approved() {
        let fsm = DraftFsm::new(DraftState::PendingReview)
            .apply(DraftEvent::Approve)
            .unwrap()
            .apply(DraftEvent::Edit)
            .unwrap()
            .apply(DraftEvent::Approve)
            .unwrap();
        assert_eq!(fsm.state(), DraftState::Approved);
    }
}
