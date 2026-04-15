//! LLM client abstraction for Agent-review.
//!
//! Provides a trait for LLM inference and an in-memory fake for testing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use thiserror::Error;

use domain::Platform;

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

/// A tool available to the LLM, described using JSON Schema for its inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// A single message in a multi-turn conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
}

/// Content of a message — either a plain string or a list of typed content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// A typed content block within an assistant or user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
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
    /// Tools to make available to the model. Empty means no tool-use.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
}

/// Response from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub reply_text: String,
    pub model_name: String,
    /// sha256 hex of the exact prompt / message history sent to the model.
    ///
    /// Stored alongside drafts/runs so responses can be reproduced and audited.
    pub prompt_fingerprint: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
    /// Raw content blocks from the assistant response.
    ///
    /// Callers building a multi-turn tool-use loop use these to construct the
    /// assistant message they append to conversation history.
    #[serde(default)]
    pub content_blocks: Vec<ContentBlock>,
    /// Tool calls the model wants to make (non-empty when `needs_tool_execution` is true).
    #[serde(default)]
    pub tool_calls: Vec<ToolUseCall>,
    /// True when the model stopped with `stop_reason == "tool_use"`.
    #[serde(default)]
    pub needs_tool_execution: bool,
}

/// Trait abstracting LLM inference.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Generate a reply from a single-turn request.
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse, LlmError>;

    /// Generate using a pre-built conversation history (for multi-turn tool-use loops).
    ///
    /// `history` is the full message array to send; the `request` supplies metadata
    /// such as model tier, tools list, and max_chars.
    async fn generate_with_history(
        &self,
        request: GenerateRequest,
        history: Vec<Message>,
    ) -> Result<GenerateResponse, LlmError>;
}

/// In-memory fake that returns scripted responses for testing.
///
/// `generate_with_history` ignores the history and delegates to `generate`,
/// so the tool-use loop terminates immediately (no tool calls are simulated).
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

#[async_trait::async_trait]
impl LlmClient for InMemoryLlm {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let prompt = build_prompt(&request);
        let prompt_fingerprint = sha256_hex(prompt.as_bytes());
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
            prompt_fingerprint,
            prompt_tokens: 100,
            completion_tokens: 50,
            latency_ms: 10,
            content_blocks: Vec::new(),
            tool_calls: Vec::new(),
            needs_tool_execution: false,
        })
    }

    async fn generate_with_history(
        &self,
        request: GenerateRequest,
        _history: Vec<Message>,
    ) -> Result<GenerateResponse, LlmError> {
        self.generate(request).await
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

/// Cost in microdollars (1 USD = 1,000,000 µUSD) for an LLM response.
///
/// Uses hardcoded Anthropic pricing tiers:
/// - claude-opus-*: $15/$75 per 1M input/output tokens
/// - all others (sonnet, etc.): $3/$15 per 1M input/output tokens
fn compute_cost_microdollars(model: &str, input_tokens: u32, output_tokens: u32) -> u64 {
    // Rates in µUSD per 1M tokens. Dividing by 1_000_000 gives cost per token in µUSD.
    let (input_rate, output_rate) = if model.contains("opus") {
        (15_000_000_u64, 75_000_000_u64)
    } else {
        (3_000_000_u64, 15_000_000_u64)
    };
    let input_cost = u64::from(input_tokens) * input_rate / 1_000_000;
    let output_cost = u64::from(output_tokens) * output_rate / 1_000_000;
    input_cost + output_cost
}

/// Returns the current month as `YYYYMM` (e.g. `202604` for April 2026).
fn compute_current_month() -> u32 {
    let now = time::OffsetDateTime::now_utc();
    let year = u32::try_from(now.year()).unwrap_or(0);
    let month = u32::from(u8::from(now.month()));
    year * 100 + month
}

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    http: reqwest::Client,
    config: LlmConfig,
    /// Accumulated cost this month in microdollars (µUSD). Reset when the calendar month changes.
    accumulated_cost_microdollars: Arc<AtomicU64>,
    /// The month (YYYYMM) the accumulator was last reset for.
    cost_month: Arc<std::sync::Mutex<u32>>,
}

impl AnthropicClient {
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
            accumulated_cost_microdollars: Arc::new(AtomicU64::new(0)),
            cost_month: Arc::new(std::sync::Mutex::new(compute_current_month())),
        }
    }

    /// Returns the accumulated LLM spend this month in USD.
    #[must_use]
    pub fn current_cost_usd(&self) -> f64 {
        let microdollars = self.accumulated_cost_microdollars.load(Ordering::Relaxed);
        microdollars as f64 / 1_000_000.0
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
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    ty: String,
    // text block
    text: Option<String>,
    // tool_use block
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

/// Build the initial user prompt from a `GenerateRequest`.
pub fn build_prompt(request: &GenerateRequest) -> String {
    format!(
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
        request
            .review_language
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        request.restaurant_name,
        request.restaurant_context,
        request
            .review_text
            .clone()
            .unwrap_or_else(|| "(rating-only review)".into()),
        request.hint.clone().unwrap_or_else(|| "(none)".into()),
        request.max_chars
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[async_trait::async_trait]
impl LlmClient for AnthropicClient {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let prompt = build_prompt(&request);
        let initial_message = Message {
            role: "user".into(),
            content: MessageContent::Text(prompt),
        };
        self.generate_with_history(request, vec![initial_message])
            .await
    }

    async fn generate_with_history(
        &self,
        request: GenerateRequest,
        history: Vec<Message>,
    ) -> Result<GenerateResponse, LlmError> {
        let start = std::time::Instant::now();
        let max_retries = self.config.max_retries.max(1);

        let history_bytes = serde_json::to_vec(&history).unwrap_or_default();
        let prompt_fingerprint = sha256_hex(&history_bytes);

        // Pre-flight cost cap check.
        let cap_microdollars = (self.config.monthly_cost_cap_usd * 1_000_000.0) as u64;
        let current = self.accumulated_cost_microdollars.load(Ordering::Relaxed);
        if current >= cap_microdollars {
            return Err(LlmError::CostCapExceeded);
        }

        let url = format!("{}/v1/messages", self.config.api_base_url.trim_end_matches('/'));

        let tools_value = if request.tools.is_empty() {
            None
        } else {
            Some(
                serde_json::to_value(&request.tools)
                    .map_err(|e| LlmError::Other(e.to_string()))?,
            )
        };

        for attempt in 0..max_retries {
            let mut body = serde_json::json!({
                "model": self.model_for_tier(request.model_tier),
                "max_tokens": 512,
                "messages": &history,
            });
            if let Some(ref tools) = tools_value {
                body["tools"] = tools.clone();
            }

            let req = self
                .http
                .post(&url)
                .header("x-api-key", self.key()?)
                .header("anthropic-version", "2023-06-01")
                .json(&body);

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

            let needs_tool_execution =
                parsed.stop_reason.as_deref() == Some("tool_use");

            let mut reply_text = String::new();
            let mut tool_calls = Vec::new();
            let mut content_blocks = Vec::new();

            for block in &parsed.content {
                match block.ty.as_str() {
                    "text" => {
                        if let Some(ref t) = block.text {
                            reply_text.push_str(t);
                            content_blocks.push(ContentBlock::Text { text: t.clone() });
                        }
                    }
                    "tool_use" => {
                        if let (Some(id), Some(name), Some(input)) =
                            (&block.id, &block.name, &block.input)
                        {
                            tool_calls.push(ToolUseCall {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            });
                            content_blocks.push(ContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }

            let input_tokens = parsed.usage.as_ref().map_or(0, |u| u.input_tokens);
            let output_tokens = parsed.usage.as_ref().map_or(0, |u| u.output_tokens);

            // Accumulate cost; reset if the calendar month has rolled over.
            let cost = compute_cost_microdollars(&parsed.model, input_tokens, output_tokens);
            let current_month = compute_current_month();
            {
                let mut month = self.cost_month.lock().unwrap();
                if *month != current_month {
                    *month = current_month;
                    self.accumulated_cost_microdollars.store(0, Ordering::Relaxed);
                }
            }
            self.accumulated_cost_microdollars.fetch_add(cost, Ordering::Relaxed);

            let latency_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            return Ok(GenerateResponse {
                reply_text,
                model_name: parsed.model,
                prompt_fingerprint,
                prompt_tokens: input_tokens,
                completion_tokens: output_tokens,
                latency_ms,
                content_blocks,
                tool_calls,
                needs_tool_execution,
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
            tools: Vec::new(),
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
            tools: Vec::new(),
        };
        let resp = llm.generate(req).await.unwrap();
        assert!(resp.reply_text.contains("be more empathetic"));
        assert_eq!(resp.model_name, "in-memory-escalation");
    }

    #[tokio::test]
    async fn in_memory_llm_generate_with_history_delegates_to_generate() {
        let llm = InMemoryLlm::default();
        let req = GenerateRequest {
            review_text: Some("Nice place".into()),
            review_rating: 4,
            review_language: Some("en".into()),
            platform: Platform::Google,
            restaurant_name: "Chez Luca".into(),
            restaurant_context: String::new(),
            model_tier: ModelTier::Standard,
            max_chars: 500,
            hint: None,
            tools: Vec::new(),
        };
        let history = vec![Message {
            role: "user".into(),
            content: MessageContent::Text("some prior context".into()),
        }];
        let resp = llm.generate_with_history(req, history).await.unwrap();
        assert!(!resp.reply_text.is_empty());
        assert!(!resp.needs_tool_execution);
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
            tools: Vec::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: GenerateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.review_rating, 5);
    }

    #[test]
    fn generate_request_serde_tools_default() {
        // Requests serialized without `tools` should still deserialize cleanly.
        let json = r#"{"review_text":"Nice!","review_rating":5,"review_language":"en","platform":"google","restaurant_name":"X","restaurant_context":"Y","model_tier":"standard","max_chars":1000,"hint":null}"#;
        let decoded: GenerateRequest = serde_json::from_str(json).unwrap();
        assert!(decoded.tools.is_empty());
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
            tools: Vec::new(),
        };
        let resp = llm.generate(req).await.unwrap();
        assert_eq!(resp.reply_text, "Custom reply from our team.");
    }

    #[test]
    fn tool_definition_serializes_to_anthropic_shape() {
        let tool = ToolDefinition {
            name: "lookup_menu_item".into(),
            description: "Look up a menu item".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        };
        let v = serde_json::to_value(&tool).unwrap();
        assert_eq!(v["name"], "lookup_menu_item");
        assert!(v["input_schema"].is_object());
    }

    #[test]
    fn content_block_serde_round_trips() {
        let blocks = vec![
            ContentBlock::Text { text: "hello".into() },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "lookup_policy".into(),
                input: serde_json::json!({"topic": "refunds"}),
            },
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: serde_json::json!({"policy": "no refunds"}),
            },
        ];
        let json = serde_json::to_string(&blocks).unwrap();
        let decoded: Vec<ContentBlock> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.len(), 3);
    }

    #[test]
    fn compute_cost_microdollars_sonnet() {
        // 1M input + 1M output at $3/$15 per 1M = $18 = 18_000_000 µUSD
        let cost = compute_cost_microdollars("claude-sonnet-4-6", 1_000_000, 1_000_000);
        assert_eq!(cost, 18_000_000);
    }

    #[test]
    fn compute_cost_microdollars_opus() {
        // 1M input + 1M output at $15/$75 per 1M = $90 = 90_000_000 µUSD
        let cost = compute_cost_microdollars("claude-opus-4-6", 1_000_000, 1_000_000);
        assert_eq!(cost, 90_000_000);
    }

    #[test]
    fn compute_cost_microdollars_small_tokens() {
        // input: 1000 * 3_000_000 / 1_000_000 = 3 µUSD
        // output: 500 * 15_000_000 / 1_000_000 = 7 µUSD (integer division)
        let cost = compute_cost_microdollars("claude-sonnet-4-6", 1_000, 500);
        assert_eq!(cost, 10_500);
    }

    #[tokio::test]
    async fn anthropic_client_rejects_when_cap_exceeded() {
        use std::sync::atomic::Ordering;
        let config = LlmConfig {
            monthly_cost_cap_usd: 0.000_001, // 1 µUSD cap
            api_key: Some("dummy".into()),
            ..LlmConfig::default()
        };
        let client = AnthropicClient::new(config);
        // Push accumulated cost over the cap.
        client.accumulated_cost_microdollars.store(2, Ordering::Relaxed);

        let req = GenerateRequest {
            review_text: Some("Nice".into()),
            review_rating: 5,
            review_language: Some("en".into()),
            platform: Platform::Google,
            restaurant_name: "Test".into(),
            restaurant_context: String::new(),
            model_tier: ModelTier::Standard,
            max_chars: 500,
            hint: None,
            tools: Vec::new(),
        };
        let err = client.generate(req).await.unwrap_err();
        assert!(matches!(err, LlmError::CostCapExceeded));
    }

    #[test]
    fn anthropic_client_current_cost_usd_reflects_accumulator() {
        use std::sync::atomic::Ordering;
        let config = LlmConfig::default();
        let client = AnthropicClient::new(config);
        assert_eq!(client.current_cost_usd(), 0.0);
        client
            .accumulated_cost_microdollars
            .store(5_000_000, Ordering::Relaxed);
        assert!((client.current_cost_usd() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn compute_current_month_returns_valid_yyyymm() {
        let month = compute_current_month();
        assert!(month >= 202_600, "expected month >= 202600, got {month}");
        assert_eq!(month % 100, month % 100, "month component check");
        let month_component = month % 100;
        assert!((1..=12).contains(&month_component), "month must be 1-12");
    }
}
