//! Domain types and state machines for Agent-review.
//!
//! This crate is intentionally pure: no I/O, no database, no HTTP.

mod fsm;
mod model;

pub use crate::fsm::{DraftEvent, DraftFsm, DraftState, InvalidTransition};
pub use crate::model::{Generator, Platform, ReplyDraft, Review, ReviewAuthor, ReviewStatus};

