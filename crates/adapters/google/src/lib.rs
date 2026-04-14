//! Google Business Profile adapter — normalizes Google reviews into the
//! unified domain model and posts approved replies.

mod normalize;

pub use normalize::normalize_google_review;

use domain::{Platform, Review};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use url::Url;

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
    pub api_base_url: String,
    pub oauth_token_url: String,
    pub oauth_client_id: String,
    pub oauth_client_secret: String,
    pub oauth_refresh_token: String,
}

impl Default for GoogleConfig {
    fn default() -> Self {
        Self {
            account_id: String::new(),
            location_id: String::new(),
            poll_interval_secs: 600,
            api_base_url: "https://mybusiness.googleapis.com".into(),
            oauth_token_url: "https://oauth2.googleapis.com/token".into(),
            oauth_client_id: String::new(),
            oauth_client_secret: String::new(),
            oauth_refresh_token: String::new(),
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

#[derive(Debug, Clone)]
pub struct HttpGoogleClient {
    http: reqwest::blocking::Client,
    token: std::sync::Mutex<Option<AccessToken>>,
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
}

impl HttpGoogleClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .user_agent("agent-review/0.1")
                .build()
                .expect("reqwest client build"),
            token: std::sync::Mutex::new(None),
        }
    }

    fn token_is_fresh(token: &AccessToken) -> bool {
        // Refresh within 5 minutes of expiry (spec).
        token.expires_at - OffsetDateTime::now_utc() > time::Duration::minutes(5)
    }

    fn get_access_token(&self, cfg: &GoogleConfig) -> Result<String, GoogleAdapterError> {
        {
            let guard = self.token.lock().map_err(|_| GoogleAdapterError::AuthError)?;
            if let Some(t) = guard.as_ref() {
                if Self::token_is_fresh(t) {
                    return Ok(t.value.clone());
                }
            }
        }

        if cfg.oauth_client_id.is_empty()
            || cfg.oauth_client_secret.is_empty()
            || cfg.oauth_refresh_token.is_empty()
        {
            return Err(GoogleAdapterError::AuthError);
        }

        let resp = self
            .http
            .post(&cfg.oauth_token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", cfg.oauth_client_id.as_str()),
                ("client_secret", cfg.oauth_client_secret.as_str()),
                ("refresh_token", cfg.oauth_refresh_token.as_str()),
            ])
            .send()
            .map_err(|_| GoogleAdapterError::AuthError)?;

        if !resp.status().is_success() {
            return Err(GoogleAdapterError::AuthError);
        }

        let tr: TokenResponse = resp.json().map_err(|e| GoogleAdapterError::ParseError(e.to_string()))?;
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::seconds(tr.expires_in.max(0));
        let token = AccessToken {
            value: tr.access_token.clone(),
            expires_at,
        };

        let mut guard = self.token.lock().map_err(|_| GoogleAdapterError::AuthError)?;
        *guard = Some(token);
        Ok(tr.access_token)
    }

    fn build_reviews_list_url(
        cfg: &GoogleConfig,
        page_token: Option<&str>,
    ) -> Result<Url, GoogleAdapterError> {
        let base = format!(
            "{}/v4/accounts/{}/locations/{}/reviews",
            cfg.api_base_url.trim_end_matches('/'),
            cfg.account_id,
            cfg.location_id
        );
        let mut url = Url::parse(&base).map_err(|e| GoogleAdapterError::ParseError(e.to_string()))?;
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("orderBy", "updateTime desc");
            qp.append_pair("pageSize", "50");
            if let Some(tok) = page_token {
                qp.append_pair("pageToken", tok);
            }
        }
        Ok(url)
    }

    fn with_retry<T>(
        mut f: impl FnMut() -> Result<T, GoogleAdapterError>,
    ) -> Result<T, GoogleAdapterError> {
        let mut delay = std::time::Duration::from_secs(2);
        let max_delay = std::time::Duration::from_secs(32);
        for attempt in 0..6 {
            match f() {
                Ok(v) => return Ok(v),
                Err(GoogleAdapterError::ApiError { status, .. }) if status == 429 || status >= 500 => {
                    if attempt == 5 {
                        return Err(GoogleAdapterError::ApiError {
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
        Err(GoogleAdapterError::ApiError {
            status: 500,
            body: "exhausted retries".into(),
        })
    }

    fn authed_get_json(
        &self,
        cfg: &GoogleConfig,
        url: Url,
    ) -> Result<serde_json::Value, GoogleAdapterError> {
        Self::with_retry(|| {
            let token = self.get_access_token(cfg)?;
            let resp = self
                .http
                .get(url.clone())
                .bearer_auth(token)
                .send()
                .map_err(|e| GoogleAdapterError::ApiError {
                    status: 0,
                    body: e.to_string(),
                })?;

            if resp.status().as_u16() == 401 {
                // Lazy refresh on 401.
                let mut guard = self.token.lock().map_err(|_| GoogleAdapterError::AuthError)?;
                *guard = None;
                let token = self.get_access_token(cfg)?;
                let resp2 = self
                    .http
                    .get(url.clone())
                    .bearer_auth(token)
                    .send()
                    .map_err(|e| GoogleAdapterError::ApiError {
                        status: 0,
                        body: e.to_string(),
                    })?;
                if !resp2.status().is_success() {
                    let status = resp2.status().as_u16();
                    let body = resp2.text().unwrap_or_default();
                    return Err(GoogleAdapterError::ApiError { status, body });
                }
                return resp2
                    .json()
                    .map_err(|e| GoogleAdapterError::ParseError(e.to_string()));
            }

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                return Err(GoogleAdapterError::ApiError { status, body });
            }

            resp.json()
                .map_err(|e| GoogleAdapterError::ParseError(e.to_string()))
        })
    }
}

impl Default for HttpGoogleClient {
    fn default() -> Self {
        Self::new()
    }
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

impl GoogleReviewClient for HttpGoogleClient {
    fn list_reviews(&self, config: &GoogleConfig) -> Result<Vec<serde_json::Value>, GoogleAdapterError> {
        if config.account_id.is_empty() || config.location_id.is_empty() {
            return Err(GoogleAdapterError::MissingField("account_id/location_id"));
        }

        let mut out = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = Self::build_reviews_list_url(config, page_token.as_deref())?;
            let v = self.authed_get_json(config, url)?;

            let reviews = v["reviews"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            out.extend(reviews);

            page_token = v["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }

        Ok(out)
    }

    fn post_reply(
        &self,
        config: &GoogleConfig,
        review_id: &str,
        reply_text: &str,
    ) -> Result<(), GoogleAdapterError> {
        if reply_text.chars().count() > 1000 {
            return Err(GoogleAdapterError::ApiError {
                status: 400,
                body: "reply exceeds 1000 chars".into(),
            });
        }

        let base = format!(
            "{}/v4/accounts/{}/locations/{}/reviews/{}/reply",
            config.api_base_url.trim_end_matches('/'),
            config.account_id,
            config.location_id,
            review_id
        );
        let url = Url::parse(&base).map_err(|e| GoogleAdapterError::ParseError(e.to_string()))?;

        Self::with_retry(|| {
            let token = self.get_access_token(config)?;
            let resp = self
                .http
                .put(url.clone())
                .bearer_auth(token)
                .json(&serde_json::json!({ "comment": reply_text }))
                .send()
                .map_err(|e| GoogleAdapterError::ApiError {
                    status: 0,
                    body: e.to_string(),
                })?;

            if resp.status().as_u16() == 401 {
                let mut guard = self.token.lock().map_err(|_| GoogleAdapterError::AuthError)?;
                *guard = None;
                let token = self.get_access_token(config)?;
                let resp2 = self
                    .http
                    .put(url.clone())
                    .bearer_auth(token)
                    .json(&serde_json::json!({ "comment": reply_text }))
                    .send()
                    .map_err(|e| GoogleAdapterError::ApiError {
                        status: 0,
                        body: e.to_string(),
                    })?;
                if !resp2.status().is_success() {
                    let status = resp2.status().as_u16();
                    let body = resp2.text().unwrap_or_default();
                    return Err(GoogleAdapterError::ApiError { status, body });
                }
                return Ok(());
            }

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                return Err(GoogleAdapterError::ApiError { status, body });
            }
            Ok(())
        })
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
