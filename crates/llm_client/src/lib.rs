//! LLM client abstraction for Agent-review.
//!
//! Provides a trait for LLM inference and an in-memory fake for testing.

use domain::Platform;
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum LlmError {
    #[error("LLM request timed out after {0}ms")]
    Timeout(u64),

    #[error("LLM returned error status {status}: {message}")]
    ApiError { status: u16, message: String },

    #[error("monthly cost cap exceeded")]
    CostCapExceeded,

    #[error("{0}")]
    Other(String),
}

/// Which model tier to use for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Standard,
    Escalation,
}

/// A request to generate a reply draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub review_text: Option<String>,
    pub review_rating: u8,
    pub review_language: Option<String>,
    pub platform: Platform,
    pub restaurant_name: String,
    pub restaurant_context: String,
    pub model_tier: ModelTier,
    pub max_chars: u32,
    pub hint: Option<String>,
}

/// Response from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub reply_text: String,
    pub model_name: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
}

/// Trait abstracting LLM inference.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse, LlmError>;
}

/// In-memory fake that returns scripted responses for testing.
#[derive(Debug, Clone)]
pub struct InMemoryLlm {
    pub default_reply: String,
}

impl Default for InMemoryLlm {
    fn default() -> Self {
        Self {
            default_reply: "Thank you for your feedback! We appreciate it and hope to see you again soon.".into(),
        }
    }
}

impl LlmClient for InMemoryLlm {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let reply = if let Some(hint) = &request.hint {
            format!("{} (hint: {hint})", self.default_reply)
        } else {
            self.default_reply.clone()
        };

        Ok(GenerateResponse {
            reply_text: reply,
            model_name: match request.model_tier {
                ModelTier::Standard => "in-memory-standard".into(),
                ModelTier::Escalation => "in-memory-escalation".into(),
            },
            prompt_tokens: 100,
            completion_tokens: 50,
            latency_ms: 10,
        })
    }
}

/// Configuration for the Anthropic API client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_base_url: String,
    pub standard_model: String,
    pub escalation_model: String,
    pub max_retries: u32,
    pub timeout_ms: u64,
    pub monthly_cost_cap_usd: f64,
    pub api_key: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_base_url: "https://api.anthropic.com".into(),
            standard_model: "claude-sonnet-4-6".into(),
            escalation_model: "claude-opus-4-6".into(),
            max_retries: 3,
            timeout_ms: 30_000,
            monthly_cost_cap_usd: 50.0,
            api_key: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    http: reqwest::Client,
    config: LlmConfig,
}

impl AnthropicClient {
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
        }
    }

    fn model_for_tier(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Standard => &self.config.standard_model,
            ModelTier::Escalation => &self.config.escalation_model,
        }
    }

    fn key(&self) -> Result<&str, LlmError> {
        self.config
            .api_key
            .as_deref()
            .ok_or_else(|| LlmError::Other("ANTHROPIC_API_KEY not configured".into()))
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    model: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    ty: String,
    text: Option<String>,
}

impl LlmClient for AnthropicClient {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let start = std::time::Instant::now();
        let max_retries = self.config.max_retries.max(1);

        let prompt = format!(
            "You are replying as the restaurant owner.\n\
             Platform: {}\n\
             Rating: {}\n\
             Language: {}\n\
             Restaurant: {}\n\
             Context: {}\n\
             Review: {}\n\
             Hint: {}\n\
             Write a reply under {} characters.",
            request.platform,
            request.review_rating,
            request.review_language.clone().unwrap_or_else(|| "unknown".into()),
            request.restaurant_name,
            request.restaurant_context,
            request.review_text.clone().unwrap_or_else(|| "(rating-only review)".into()),
            request.hint.clone().unwrap_or_else(|| "(none)".into()),
            request.max_chars
        );

        let url = format!("{}/v1/messages", self.config.api_base_url.trim_end_matches('/'));

        for attempt in 0..max_retries {
            let req = self
                .http
                .post(&url)
                .header("x-api-key", self.key()?)
                .header("anthropic-version", "2023-06-01")
                .json(&serde_json::json!({
                    "model": self.model_for_tier(request.model_tier),
                    "max_tokens": 512,
                    "messages": [
                        { "role": "user", "content": prompt }
                    ]
                }));

            let resp = tokio::time::timeout(
                std::time::Duration::from_millis(self.config.timeout_ms),
                req.send(),
            )
            .await
            .map_err(|_| LlmError::Timeout(self.config.timeout_ms))?
            .map_err(|e| LlmError::Other(e.to_string()))?;

            let status = resp.status().as_u16();
            let text = resp.text().await.map_err(|e| LlmError::Other(e.to_string()))?;

            if status >= 500 || status == 429 {
                if attempt + 1 == max_retries {
                    return Err(LlmError::ApiError {
                        status,
                        message: text,
                    });
                }
                let backoff_ms = 500_u64.saturating_mul(2_u64.saturating_pow(attempt));
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                continue;
            }

            if status >= 400 {
                return Err(LlmError::ApiError {
                    status,
                    message: text,
                });
            }

            let parsed: AnthropicResponse =
                serde_json::from_str(&text).map_err(|e| LlmError::Other(e.to_string()))?;
            let reply_text = parsed
                .content
                .into_iter()
                .find(|c| c.ty == "text")
                .and_then(|c| c.text)
                .unwrap_or_default();

            let latency_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            return Ok(GenerateResponse {
                reply_text,
                model_name: parsed.model,
                prompt_tokens: 0,
                completion_tokens: 0,
                latency_ms,
            });
        }

        Err(LlmError::Other("unreachable".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_llm_returns_default() {
        let llm = InMemoryLlm::default();
        let req = GenerateRequest {
            review_text: Some("Great food!".into()),
            review_rating: 5,
            review_language: Some("en".into()),
            platform: Platform::Google,
            restaurant_name: "Chez Luca".into(),
            restaurant_context: "Italian trattoria".into(),
            model_tier: ModelTier::Standard,
            max_chars: 1000,
            hint: None,
        };
        let resp = llm.generate(req).await.unwrap();
        assert!(!resp.reply_text.is_empty());
        assert_eq!(resp.model_name, "in-memory-standard");
    }

    #[tokio::test]
    async fn in_memory_llm_includes_hint() {
        let llm = InMemoryLlm::default();
        let req = GenerateRequest {
            review_text: None,
            review_rating: 1,
            review_language: None,
            platform: Platform::Ubereats,
            restaurant_name: "Chez Luca".into(),
            restaurant_context: String::new(),
            model_tier: ModelTier::Escalation,
            max_chars: 500,
            hint: Some("be more empathetic".into()),
        };
        let resp = llm.generate(req).await.unwrap();
        assert!(resp.reply_text.contains("be more empathetic"));
        assert_eq!(resp.model_name, "in-memory-escalation");
    }

    #[test]
    fn llm_config_defaults() {
        let config = LlmConfig::default();
        assert!(config.api_base_url.contains("anthropic"));
        assert_eq!(config.standard_model, "claude-sonnet-4-6");
        assert_eq!(config.escalation_model, "claude-opus-4-6");
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.timeout_ms, 30_000);
        assert!(config.monthly_cost_cap_usd > 0.0);
    }

    #[test]
    fn model_tier_serde_round_trip() {
        let tier = ModelTier::Escalation;
        let json = serde_json::to_string(&tier).unwrap();
        let decoded: ModelTier = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, ModelTier::Escalation);
    }

    #[test]
    fn generate_request_serde() {
        let req = GenerateRequest {
            review_text: Some("Nice!".into()),
            review_rating: 5,
            review_language: Some("en".into()),
            platform: Platform::Google,
            restaurant_name: "Chez Luca".into(),
            restaurant_context: "Italian".into(),
            model_tier: ModelTier::Standard,
            max_chars: 1000,
            hint: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: GenerateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.review_rating, 5);
    }

    #[test]
    fn llm_error_display() {
        assert!(LlmError::Timeout(5000).to_string().contains("5000"));
        assert!(LlmError::CostCapExceeded.to_string().contains("cost cap"));
        assert!(LlmError::Other("oops".into()).to_string().contains("oops"));
    }

    #[tokio::test]
    async fn in_memory_llm_custom_reply() {
        let llm = InMemoryLlm {
            default_reply: "Custom reply from our team.".into(),
        };
        let req = GenerateRequest {
            review_text: Some("Food was great".into()),
            review_rating: 5,
            review_language: Some("en".into()),
            platform: Platform::Google,
            restaurant_name: "Test".into(),
            restaurant_context: String::new(),
            model_tier: ModelTier::Standard,
            max_chars: 1000,
            hint: None,
        };
        let resp = llm.generate(req).await.unwrap();
        assert_eq!(resp.reply_text, "Custom reply from our team.");
    }
}
