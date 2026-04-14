//! Poster worker — publishes approved reply drafts back to the originating
//! platform and handles retries.

use std::future::Future;

use domain::{DraftEvent, DraftFsm, Platform, ReplyDraft};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PostError {
    #[error("platform rejected the reply: {0}")]
    PlatformRejected(String),

    #[error("network error posting to {platform}: {message}")]
    NetworkError { platform: Platform, message: String },

    #[error("draft {0} is not in approved state")]
    NotApproved(Uuid),

    #[error("max retries ({0}) exhausted")]
    RetriesExhausted(u32),
}

/// Configuration for the poster worker.
#[derive(Debug, Clone)]
pub struct PosterConfig {
    pub max_retries: u32,
    pub base_backoff_ms: u64,
}

impl Default for PosterConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_backoff_ms: 2000,
        }
    }
}

/// Trait abstracting the platform-specific posting operation.
pub trait PlatformPoster: Send + Sync {
    fn post_reply(
        &self,
        platform: Platform,
        source_review_id: &str,
        reply_text: &str,
    ) -> impl Future<Output = Result<(), PostError>> + Send;
}

/// In-memory poster for testing.
#[derive(Debug, Default)]
pub struct InMemoryPoster {
    pub posted: std::sync::Mutex<Vec<PostedReply>>,
    pub should_fail: std::sync::Mutex<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedReply {
    pub platform: Platform,
    pub source_review_id: String,
    pub reply_text: String,
}

impl PlatformPoster for InMemoryPoster {
    async fn post_reply(
        &self,
        platform: Platform,
        source_review_id: &str,
        reply_text: &str,
    ) -> Result<(), PostError> {
        if *self.should_fail.lock().expect("lock") {
            return Err(PostError::PlatformRejected("test failure".into()));
        }

        self.posted.lock().expect("lock").push(PostedReply {
            platform,
            source_review_id: source_review_id.into(),
            reply_text: reply_text.into(),
        });
        Ok(())
    }
}

/// Validate that a draft is eligible for posting.
pub fn validate_for_posting(draft: &ReplyDraft) -> Result<(), PostError> {
    let fsm = DraftFsm::new(draft.state);
    if fsm.apply(DraftEvent::MarkPosted).is_err() {
        return Err(PostError::NotApproved(draft.id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DraftState, Generator, ReplyDraft};
    use time::OffsetDateTime;

    fn draft_in_state(state: DraftState) -> ReplyDraft {
        ReplyDraft {
            id: Uuid::new_v4(),
            review_id: Uuid::new_v4(),
            generated_by: Generator::AgentLlm,
            model_name: None,
            prompt_fingerprint: None,
            text: "Thanks!".into(),
            language: "en".into(),
            char_count: 7,
            state,
            guardrail_warnings: Vec::new(),
            flags: Vec::new(),
            created_at: OffsetDateTime::now_utc(),
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            posted_at: None,
            platform_post_error: None,
        }
    }

    #[test]
    fn approved_draft_valid_for_posting() {
        assert!(validate_for_posting(&draft_in_state(DraftState::Approved)).is_ok());
    }

    #[test]
    fn pending_draft_not_valid() {
        assert!(validate_for_posting(&draft_in_state(DraftState::PendingReview)).is_err());
    }

    #[test]
    fn rejected_draft_not_valid() {
        assert!(validate_for_posting(&draft_in_state(DraftState::Rejected)).is_err());
    }

    #[test]
    fn posted_draft_not_valid() {
        assert!(validate_for_posting(&draft_in_state(DraftState::Posted)).is_err());
    }

    #[tokio::test]
    async fn in_memory_poster_records() {
        let fake = InMemoryPoster::default();
        fake.post_reply(Platform::Google, "rev-1", "Thanks!")
            .await
            .unwrap();
        let recorded = fake.posted.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].source_review_id, "rev-1");
    }

    #[tokio::test]
    async fn in_memory_poster_failure_mode() {
        let fake = InMemoryPoster::default();
        *fake.should_fail.lock().unwrap() = true;
        let err = fake
            .post_reply(Platform::Google, "rev-1", "Thanks!")
            .await
            .unwrap_err();
        assert!(matches!(err, PostError::PlatformRejected(_)));
    }
}
