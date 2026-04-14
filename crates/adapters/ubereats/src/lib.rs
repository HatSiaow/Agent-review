//! UberEats Merchant adapter — normalizes UberEats reviews into the
//! unified domain model, verifies webhook signatures, and posts replies.

mod hmac_verify;
mod normalize;

pub use hmac_verify::{verify_webhook_signature, HmacVerificationError};
pub use normalize::normalize_ubereats_review;

use domain::Review;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use url::Url;

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
    pub api_base_url: String,
    pub oauth_token_url: String,
    pub oauth_client_id: String,
    pub oauth_client_secret: String,
}

impl Default for UberEatsConfig {
    fn default() -> Self {
        Self {
            store_id: String::new(),
            webhook_secret: String::new(),
            poll_interval_secs: 1800,
            api_base_url: "https://api.uber.com".into(),
            oauth_token_url: "https://login.uber.com/oauth/v2/token".into(),
            oauth_client_id: String::new(),
            oauth_client_secret: String::new(),
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

#[derive(Debug)]
pub struct HttpUberEatsClient {
    http: reqwest::blocking::Client,
    token: std::sync::Mutex<Option<AccessToken>>,
    last_request_at: std::sync::Mutex<Option<OffsetDateTime>>,
}

#[derive(Debug, Clone)]
struct AccessToken {
    value: String,
    expires_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    token_type: String,
}

impl HttpUberEatsClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .user_agent("agent-review/0.1")
                .build()
                .expect("reqwest client build"),
            token: std::sync::Mutex::new(None),
            last_request_at: std::sync::Mutex::new(None),
        }
    }

    fn token_is_fresh(token: &AccessToken) -> bool {
        // Spec: refresh when within 1 day of expiry.
        token.expires_at - OffsetDateTime::now_utc() > time::Duration::days(1)
    }

    fn rate_limit(&self) -> Result<(), UberEatsAdapterError> {
        // Simple ~5 req/s limiter: ensure >=200ms between requests.
        let mut guard = self
            .last_request_at
            .lock()
            .map_err(|_| UberEatsAdapterError::ApiError {
                status: 0,
                body: "rate limiter poisoned".into(),
            })?;
        let now = OffsetDateTime::now_utc();
        if let Some(last) = *guard {
            let delta = now - last;
            let min = time::Duration::milliseconds(200);
            if delta < min {
                let ms = (min - delta).whole_milliseconds().max(0);
                let sleep_for = u64::try_from(ms).unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_millis(sleep_for));
            }
        }
        *guard = Some(OffsetDateTime::now_utc());
        Ok(())
    }

    fn get_access_token(&self, cfg: &UberEatsConfig) -> Result<String, UberEatsAdapterError> {
        {
            let guard = self.token.lock().map_err(|_| UberEatsAdapterError::AuthError)?;
            if let Some(t) = guard.as_ref() {
                if Self::token_is_fresh(t) {
                    return Ok(t.value.clone());
                }
            }
        }

        if cfg.oauth_client_id.is_empty() || cfg.oauth_client_secret.is_empty() {
            return Err(UberEatsAdapterError::AuthError);
        }

        let resp = self
            .http
            .post(&cfg.oauth_token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", cfg.oauth_client_id.as_str()),
                ("client_secret", cfg.oauth_client_secret.as_str()),
                (
                    "scope",
                    "eats.store eats.store.reviews.read eats.store.reviews.write",
                ),
            ])
            .send()
            .map_err(|_| UberEatsAdapterError::AuthError)?;

        if !resp.status().is_success() {
            return Err(UberEatsAdapterError::AuthError);
        }
        let tr: TokenResponse = resp
            .json()
            .map_err(|e| UberEatsAdapterError::ParseError(e.to_string()))?;
        if !tr.token_type.eq_ignore_ascii_case("bearer") {
            return Err(UberEatsAdapterError::AuthError);
        }
        let expires_at =
            OffsetDateTime::now_utc() + time::Duration::seconds(tr.expires_in.max(0));
        let token = AccessToken {
            value: tr.access_token.clone(),
            expires_at,
        };
        let mut guard = self.token.lock().map_err(|_| UberEatsAdapterError::AuthError)?;
        *guard = Some(token);
        Ok(tr.access_token)
    }

    fn with_retry<T>(
        mut f: impl FnMut() -> Result<T, UberEatsAdapterError>,
    ) -> Result<T, UberEatsAdapterError> {
        let mut delay = std::time::Duration::from_secs(1);
        let max_delay = std::time::Duration::from_secs(16);
        for attempt in 0..6 {
            match f() {
                Ok(v) => return Ok(v),
                Err(UberEatsAdapterError::ApiError { status, .. })
                    if status == 429 || status >= 500 =>
                {
                    if attempt == 5 {
                        return Err(UberEatsAdapterError::ApiError {
                            status,
                            body: "exhausted retries".into(),
                        });
                    }
                    std::thread::sleep(delay);
                    delay = std::cmp::min(max_delay, delay.saturating_mul(2));
                }
                Err(e) => return Err(e),
            }
        }
        Err(UberEatsAdapterError::ApiError {
            status: 500,
            body: "exhausted retries".into(),
        })
    }

    fn build_list_url(cfg: &UberEatsConfig) -> Result<Url, UberEatsAdapterError> {
        let base = format!(
            "{}/v1/eats/stores/{}/reviews",
            cfg.api_base_url.trim_end_matches('/'),
            cfg.store_id
        );
        Url::parse(&base).map_err(|e| UberEatsAdapterError::ParseError(e.to_string()))
    }
}

impl Default for HttpUberEatsClient {
    fn default() -> Self {
        Self::new()
    }
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

impl UberEatsReviewClient for HttpUberEatsClient {
    fn list_reviews(
        &self,
        config: &UberEatsConfig,
    ) -> Result<Vec<serde_json::Value>, UberEatsAdapterError> {
        if config.store_id.is_empty() {
            return Err(UberEatsAdapterError::MissingField("store_id"));
        }
        let url = Self::build_list_url(config)?;

        Self::with_retry(|| {
            self.rate_limit()?;
            let token = self.get_access_token(config)?;
            let resp = self
                .http
                .get(url.clone())
                .bearer_auth(token)
                .send()
                .map_err(|e| UberEatsAdapterError::ApiError {
                    status: 0,
                    body: e.to_string(),
                })?;
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                return Err(UberEatsAdapterError::ApiError { status, body });
            }
            let v: serde_json::Value = resp
                .json()
                .map_err(|e| UberEatsAdapterError::ParseError(e.to_string()))?;
            Ok(v["reviews"].as_array().cloned().unwrap_or_default())
        })
    }

    fn post_reply(
        &self,
        config: &UberEatsConfig,
        review_id: &str,
        reply_text: &str,
    ) -> Result<(), UberEatsAdapterError> {
        if reply_text.chars().count() > 500 {
            return Err(UberEatsAdapterError::ApiError {
                status: 400,
                body: "reply exceeds 500 chars".into(),
            });
        }
        let base = format!(
            "{}/v1/eats/stores/{}/reviews/{}/reply",
            config.api_base_url.trim_end_matches('/'),
            config.store_id,
            review_id
        );
        let url = Url::parse(&base).map_err(|e| UberEatsAdapterError::ParseError(e.to_string()))?;

        Self::with_retry(|| {
            self.rate_limit()?;
            let token = self.get_access_token(config)?;
            let resp = self
                .http
                .post(url.clone())
                .bearer_auth(token)
                .json(&serde_json::json!({ "text": reply_text }))
                .send()
                .map_err(|e| UberEatsAdapterError::ApiError {
                    status: 0,
                    body: e.to_string(),
                })?;
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                return Err(UberEatsAdapterError::ApiError { status, body });
            }
            Ok(())
        })
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
