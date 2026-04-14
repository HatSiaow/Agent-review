//! LLM client abstraction for Agent-review.
//!
//! Provides a trait for LLM inference and an in-memory fake for testing.

use std::future::Future;

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
pub trait LlmClient: Send + Sync {
    fn generate(
        &self,
        request: GenerateRequest,
    ) -> impl Future<Output = Result<GenerateResponse, LlmError>> + Send;
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
        }
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
}
