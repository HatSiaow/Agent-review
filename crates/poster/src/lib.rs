//! Poster worker — publishes approved reply drafts back to the originating
//! platform and handles retries.

use std::future::Future;

use adapter_google::{GoogleConfig, GoogleReviewClient, HttpGoogleClient};
use adapter_ubereats::{HttpUberEatsClient, UberEatsConfig, UberEatsReviewClient};
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

#[derive(Debug, Clone)]
pub struct HttpPlatformPoster {
    google: Option<GoogleConfig>,
    ubereats: Option<UberEatsConfig>,
}

impl HttpPlatformPoster {
    #[must_use]
    pub fn new(
        google: Option<GoogleConfig>,
        ubereats: Option<UberEatsConfig>,
    ) -> Self {
        Self { google, ubereats }
    }
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

impl PlatformPoster for HttpPlatformPoster {
    async fn post_reply(
        &self,
        platform: Platform,
        source_review_id: &str,
        reply_text: &str,
    ) -> Result<(), PostError> {
        match platform {
            Platform::Google => {
                let Some(cfg) = self.google.clone() else {
                    return Err(PostError::NetworkError {
                        platform,
                        message: "google poster not configured".into(),
                    });
                };
                let review_id = source_review_id.to_string();
                let reply = reply_text.to_string();
                tokio::task::spawn_blocking(move || {
                    let client = HttpGoogleClient::new();
                    client.post_reply(&cfg, &review_id, &reply)
                })
                    .await
                    .map_err(|e| PostError::NetworkError {
                        platform,
                        message: e.to_string(),
                    })?
                    .map_err(|e| match e {
                        adapter_google::GoogleAdapterError::ApiError { status: _, body } => {
                            PostError::PlatformRejected(body)
                        }
                        adapter_google::GoogleAdapterError::AuthError => PostError::NetworkError {
                            platform,
                            message: "auth error".into(),
                        },
                        other => PostError::NetworkError {
                            platform,
                            message: other.to_string(),
                        },
                    })
            }
            Platform::Ubereats => {
                let Some(cfg) = self.ubereats.clone() else {
                    return Err(PostError::NetworkError {
                        platform,
                        message: "ubereats poster not configured".into(),
                    });
                };
                let review_id = source_review_id.to_string();
                let reply = reply_text.to_string();
                tokio::task::spawn_blocking(move || {
                    let client = HttpUberEatsClient::new();
                    client.post_reply(&cfg, &review_id, &reply)
                })
                    .await
                    .map_err(|e| PostError::NetworkError {
                        platform,
                        message: e.to_string(),
                    })?
                    .map_err(|e| match e {
                        adapter_ubereats::UberEatsAdapterError::ApiError { status: _, body } => {
                            PostError::PlatformRejected(body)
                        }
                        adapter_ubereats::UberEatsAdapterError::AuthError => PostError::NetworkError {
                            platform,
                            message: "auth error".into(),
                        },
                        adapter_ubereats::UberEatsAdapterError::DriftDetected => {
                            PostError::PlatformRejected("drift_detected".into())
                        }
                        other => PostError::NetworkError {
                            platform,
                            message: other.to_string(),
                        },
                    })
            }
        }
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

/// Attempt to post a draft with exponential backoff retries.
///
/// Returns `Ok(())` on success or the final error after all retries are
/// exhausted. The caller is responsible for updating the draft state
/// (e.g. to `Posted` or `Failed`).
pub async fn post_with_retries<P: PlatformPoster>(
    poster: &P,
    config: &PosterConfig,
    platform: Platform,
    source_review_id: &str,
    reply_text: &str,
) -> Result<(), PostError> {
    let mut last_error = None;

    for attempt in 0..=config.max_retries {
        match poster
            .post_reply(platform, source_review_id, reply_text)
            .await
        {
            Ok(()) => return Ok(()),
            Err(PostError::PlatformRejected(msg)) => {
                return Err(PostError::PlatformRejected(msg));
            }
            Err(e) => {
                last_error = Some(e);
                if attempt < config.max_retries {
                    let backoff_ms = config.base_backoff_ms * 2u64.saturating_pow(attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => Err(PostError::RetriesExhausted(config.max_retries)),
    }
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

    #[test]
    fn edited_draft_not_valid_for_posting() {
        assert!(validate_for_posting(&draft_in_state(DraftState::Edited)).is_err());
    }

    #[test]
    fn failed_draft_not_valid() {
        assert!(validate_for_posting(&draft_in_state(DraftState::Failed)).is_err());
    }

    #[test]
    fn poster_config_defaults() {
        let config = PosterConfig::default();
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_backoff_ms, 2000);
    }

    #[test]
    fn post_error_display() {
        let err = PostError::NotApproved(Uuid::new_v4());
        assert!(err.to_string().contains("not in approved state"));

        let err = PostError::RetriesExhausted(3);
        assert!(err.to_string().contains('3'));

        let err = PostError::PlatformRejected("content policy".into());
        assert!(err.to_string().contains("content policy"));
    }

    #[tokio::test]
    async fn post_with_retries_succeeds_immediately() {
        let fake = InMemoryPoster::default();
        let config = PosterConfig {
            max_retries: 3,
            base_backoff_ms: 1,
        };
        let result =
            post_with_retries(&fake, &config, Platform::Google, "rev-1", "Thanks!").await;
        assert!(result.is_ok());
        assert_eq!(fake.posted.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn post_with_retries_platform_rejected_no_retry() {
        let fake = InMemoryPoster::default();
        *fake.should_fail.lock().unwrap() = true;
        let config = PosterConfig {
            max_retries: 3,
            base_backoff_ms: 1,
        };
        let result =
            post_with_retries(&fake, &config, Platform::Google, "rev-1", "Thanks!").await;
        assert!(matches!(result, Err(PostError::PlatformRejected(_))));
    }
}
