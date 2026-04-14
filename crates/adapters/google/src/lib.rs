//! Google Business Profile adapter — normalizes Google reviews into the
//! unified domain model and posts approved replies.

mod normalize;

pub use normalize::normalize_google_review;

use domain::{Platform, Review};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GoogleAdapterError {
    #[error("failed to parse Google review payload: {0}")]
    ParseError(String),

    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("API error: {status} {body}")]
    ApiError { status: u16, body: String },

    #[error("authentication failed")]
    AuthError,
}

/// Configuration for the Google adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleConfig {
    pub account_id: String,
    pub location_id: String,
    pub poll_interval_secs: u64,
}

impl Default for GoogleConfig {
    fn default() -> Self {
        Self {
            account_id: String::new(),
            location_id: String::new(),
            poll_interval_secs: 600,
        }
    }
}

/// Trait abstracting the Google review operations for testability.
pub trait GoogleReviewClient: Send + Sync {
    fn list_reviews(
        &self,
        config: &GoogleConfig,
    ) -> Result<Vec<serde_json::Value>, GoogleAdapterError>;

    fn post_reply(
        &self,
        config: &GoogleConfig,
        review_id: &str,
        reply_text: &str,
    ) -> Result<(), GoogleAdapterError>;
}

/// In-memory fake for testing.
#[derive(Debug, Default)]
pub struct InMemoryGoogleClient {
    pub reviews: Vec<serde_json::Value>,
    pub posted_replies: std::sync::Mutex<Vec<(String, String)>>,
}

impl GoogleReviewClient for InMemoryGoogleClient {
    fn list_reviews(
        &self,
        _config: &GoogleConfig,
    ) -> Result<Vec<serde_json::Value>, GoogleAdapterError> {
        Ok(self.reviews.clone())
    }

    fn post_reply(
        &self,
        _config: &GoogleConfig,
        review_id: &str,
        reply_text: &str,
    ) -> Result<(), GoogleAdapterError> {
        self.posted_replies
            .lock()
            .expect("lock poisoned in test")
            .push((review_id.to_string(), reply_text.to_string()));
        Ok(())
    }
}

/// Dedup key for Google reviews.
#[must_use]
pub fn dedup_key(review: &Review) -> (Platform, String) {
    (review.platform, review.source_review_id.clone())
}

/// Map Google star-rating enum strings to numeric 1..=5.
#[must_use]
pub fn star_rating_to_u8(star_rating: &str) -> Option<u8> {
    match star_rating {
        "ONE" => Some(1),
        "TWO" => Some(2),
        "THREE" => Some(3),
        "FOUR" => Some(4),
        "FIVE" => Some(5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_rating_mapping() {
        assert_eq!(star_rating_to_u8("ONE"), Some(1));
        assert_eq!(star_rating_to_u8("FIVE"), Some(5));
        assert_eq!(star_rating_to_u8("UNKNOWN"), None);
        assert_eq!(star_rating_to_u8(""), None);
    }

    #[test]
    fn in_memory_client_records_replies() {
        let client = InMemoryGoogleClient::default();
        let config = GoogleConfig::default();
        client.post_reply(&config, "rev-1", "Thanks!").unwrap();
        let replies = client.posted_replies.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0], ("rev-1".into(), "Thanks!".into()));
    }
}
