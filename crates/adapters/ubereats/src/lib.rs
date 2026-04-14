//! UberEats Merchant adapter — normalizes UberEats reviews into the
//! unified domain model, verifies webhook signatures, and posts replies.

mod hmac_verify;
mod normalize;

pub use hmac_verify::{verify_webhook_signature, HmacVerificationError};
pub use normalize::normalize_ubereats_review;

use domain::Review;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UberEatsAdapterError {
    #[error("failed to parse UberEats review payload: {0}")]
    ParseError(String),

    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("API error: {status} {body}")]
    ApiError { status: u16, body: String },

    #[error("authentication failed")]
    AuthError,

    #[error("review already has an external reply — drift detected")]
    DriftDetected,
}

/// Configuration for the UberEats adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UberEatsConfig {
    pub store_id: String,
    pub webhook_secret: String,
    pub poll_interval_secs: u64,
}

impl Default for UberEatsConfig {
    fn default() -> Self {
        Self {
            store_id: String::new(),
            webhook_secret: String::new(),
            poll_interval_secs: 1800,
        }
    }
}

/// Trait abstracting the UberEats review operations for testability.
pub trait UberEatsReviewClient: Send + Sync {
    fn list_reviews(
        &self,
        config: &UberEatsConfig,
    ) -> Result<Vec<serde_json::Value>, UberEatsAdapterError>;

    fn post_reply(
        &self,
        config: &UberEatsConfig,
        review_id: &str,
        reply_text: &str,
    ) -> Result<(), UberEatsAdapterError>;
}

/// In-memory fake for testing.
#[derive(Debug, Default)]
pub struct InMemoryUberEatsClient {
    pub reviews: Vec<serde_json::Value>,
    pub posted_replies: std::sync::Mutex<Vec<(String, String)>>,
}

impl UberEatsReviewClient for InMemoryUberEatsClient {
    fn list_reviews(
        &self,
        _config: &UberEatsConfig,
    ) -> Result<Vec<serde_json::Value>, UberEatsAdapterError> {
        Ok(self.reviews.clone())
    }

    fn post_reply(
        &self,
        _config: &UberEatsConfig,
        review_id: &str,
        reply_text: &str,
    ) -> Result<(), UberEatsAdapterError> {
        self.posted_replies
            .lock()
            .expect("lock poisoned in test")
            .push((review_id.to_string(), reply_text.to_string()));
        Ok(())
    }
}

/// Check if a review's body implies it needs personal handling
/// (low rating + no comment).
#[must_use]
pub fn needs_personal_handling(review: &Review) -> bool {
    review.rating <= 2 && !review.has_body()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Platform, ReviewAuthor, ReviewStatus};
    use serde_json::json;
    use time::macros::datetime;
    use uuid::Uuid;

    fn review_with(rating: u8, body: Option<&str>) -> Review {
        Review {
            id: Uuid::new_v4(),
            platform: Platform::Ubereats,
            source_review_id: "ue-1".into(),
            source_location_id: "store-1".into(),
            author: ReviewAuthor {
                display_name: "Test".into(),
                avatar_url: None,
            },
            rating,
            body_text: body.map(String::from),
            body_language: Some("en".into()),
            created_at: datetime!(2026-04-10 12:00:00 UTC),
            updated_at: datetime!(2026-04-10 12:00:00 UTC),
            ingested_at: datetime!(2026-04-10 12:01:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({}),
        }
    }

    #[test]
    fn low_rating_no_body_needs_personal() {
        assert!(needs_personal_handling(&review_with(1, None)));
        assert!(needs_personal_handling(&review_with(2, None)));
    }

    #[test]
    fn low_rating_with_body_does_not() {
        assert!(!needs_personal_handling(&review_with(1, Some("terrible"))));
    }

    #[test]
    fn high_rating_never_personal() {
        assert!(!needs_personal_handling(&review_with(5, None)));
    }
}
