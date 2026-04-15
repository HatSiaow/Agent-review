//! Domain types and state machines for Agent-review.
//!
//! This crate is intentionally pure: no I/O, no database, no HTTP.

mod audit;
mod fsm;
pub mod guardrails;
mod model;
mod review_fsm;
pub mod settings;
pub mod validation;

pub use crate::audit::{ActorType, AuditEvent, EventType};
pub use crate::fsm::{DraftEvent, DraftFsm, DraftState, InvalidTransition};
pub use crate::guardrails::{
    GuardrailCheck, GuardrailContext, GuardrailResult, GuardrailVerdict, GuardrailWarning,
};
pub use crate::model::{
    AgentRun, Generator, NotificationType, Platform, RejectionReason, ReplyDraft, Review, ReviewAuthor,
    ReviewStatus, Session, User, UserRole,
};
pub use crate::settings::{RestaurantSettings, RestaurantSettingsPatch};
pub use crate::review_fsm::{InvalidReviewTransition, ReviewEvent, ReviewFsm};
pub use crate::validation::ValidationError;
