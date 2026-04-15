//! AI response generation agent — drafts replies, runs guardrails, and
//! manages the agentic loop with bounded tool calls.

use domain::{Generator, GuardrailContext, GuardrailResult, ReplyDraft, Review};
use llm_client::{
    build_prompt, ContentBlock, GenerateRequest, GenerateResponse, LlmClient, Message,
    MessageContent, ModelTier, ToolDefinition,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SENSITIVE_KEYWORDS: &[&str] = &[
    "allergy",
    "allergic",
    "food poisoning",
    "illness",
    "sick",
    "lawsuit",
    "discrimination",
    "harassment",
    "hospital",
    "ambulance",
];

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("LLM error: {0}")]
    Llm(#[from] llm_client::LlmError),

    #[error("budget exhausted after {0} tool calls")]
    BudgetExhausted(u8),
}

/// A logged tool invocation for auditability.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCall {
    name: String,
}

/// Result of a single agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub draft: ReplyDraft,
    pub model_name: String,
    pub prompt_fingerprint: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
    pub tool_calls: u8,
    pub tool_calls_json: serde_json::Value,
    pub guardrail_result: GuardrailResult,
}

/// Configuration for the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub restaurant_name: String,
    pub restaurant_context: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::from_restaurant_settings(&domain::RestaurantSettings::default())
    }
}

/// Build agent configuration from persisted [`domain::RestaurantSettings`].
///
/// This keeps the LLM prompt aligned with the single source of truth in storage (no duplicate
/// hard-coded demo strings in the server binary).
#[must_use]
pub fn agent_config_from_settings(s: &domain::RestaurantSettings) -> AgentConfig {
    AgentConfig::from_restaurant_settings(s)
}

impl AgentConfig {
    fn from_restaurant_settings(s: &domain::RestaurantSettings) -> Self {
        let mut ctx = String::new();
        ctx.push_str(&format!("Cuisine: {}\n\n", s.cuisine_style));
        ctx.push_str(&s.context_line);
        if let Some(ref tone) = s.voice_tone {
            ctx.push_str("\n\n");
            ctx.push_str("Voice/tone: ");
            ctx.push_str(tone);
        }
        if !s.signature_dishes.is_empty() {
            ctx.push_str("\n\nSignature dishes: ");
            ctx.push_str(&s.signature_dishes.join(", "));
        }
        if let Some(ref hours) = s.opening_hours_text {
            ctx.push_str("\n\nHours: ");
            ctx.push_str(hours);
        }
        Self {
            restaurant_name: s.restaurant_name.clone(),
            restaurant_context: ctx,
        }
    }
}

/// Determine which model tier to use based on rating and content.
#[must_use]
pub fn select_model_tier(review: &Review) -> ModelTier {
    if review.is_sensitive_rating() || contains_sensitive_keywords(review) {
        ModelTier::Escalation
    } else {
        ModelTier::Standard
    }
}

/// Check if a review contains sensitive keywords that trigger escalation.
#[must_use]
pub fn contains_sensitive_keywords(review: &Review) -> bool {
    let Some(ref body) = review.body_text else {
        return false;
    };
    let lower = body.to_lowercase();
    SENSITIVE_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Classify flags for a review based on rating and content.
#[must_use]
pub fn classify_flags(review: &Review) -> Vec<String> {
    let mut flags = Vec::new();

    if review.rating <= 2 || contains_sensitive_keywords(review) {
        flags.push("sensitive".into());
    }
    if review.rating == 3 {
        flags.push("needs_attention".into());
    }

    flags
}

// ---------------------------------------------------------------------------
// Tool definitions (sent to the LLM so it can request tool calls)
// ---------------------------------------------------------------------------

fn define_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "lookup_menu_item".into(),
            description: "Look up a menu item by name to get canonical name, ingredients, and allergen info.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The menu item name to look up"}
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "lookup_policy".into(),
            description: "Look up restaurant policy on a topic (refunds, allergens, reservations, etc.).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "topic": {"type": "string", "description": "Policy topic to look up"}
                },
                "required": ["topic"]
            }),
        },
        ToolDefinition {
            name: "get_past_replies".into(),
            description: "Get recent approved replies for tone and style priming.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "rating": {"type": "integer", "description": "Filter by star rating (1–5)"},
                    "limit": {"type": "integer", "description": "Max number of replies to return", "default": 3}
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "check_banned_phrases".into(),
            description: "Check whether a draft reply contains banned phrases before sending.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Draft text to scan for banned phrases"}
                },
                "required": ["text"]
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// Tool execution (deterministic Rust implementations of each tool)
// ---------------------------------------------------------------------------

fn execute_tool(
    name: &str,
    input: &serde_json::Value,
    _review: &Review,
    _config: &AgentConfig,
) -> serde_json::Value {
    match name {
        "lookup_menu_item" => {
            let item_name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let lower = item_name.to_lowercase();
            let known: &[(&str, &str)] = &[
                ("pasta", "Fresh pasta — ingredients: flour, eggs. Contains gluten."),
                ("naan", "Tandoor-baked naan bread. Contains gluten."),
                ("biryani", "Aromatic rice dish with spices. No common allergens by default."),
                ("pizza", "Wood-fired pizza. Contains gluten."),
                ("salad", "House salad with seasonal vegetables. No allergens."),
            ];
            for (key, info) in known {
                if lower.contains(key) {
                    return serde_json::json!({ "found": true, "item": item_name, "info": info });
                }
            }
            serde_json::json!({ "found": false, "message": "Item not found in menu database" })
        }

        "lookup_policy" => {
            let topic = input.get("topic").and_then(|v| v.as_str()).unwrap_or("general");
            let lower = topic.to_lowercase();
            let policy = if lower.contains("refund") {
                "We do not offer refunds for delivered food. For quality issues on dine-in, speak with a manager during your visit."
            } else if lower.contains("allerg") {
                "We take allergies seriously. Please inform staff before ordering. We cannot guarantee a completely allergen-free environment."
            } else if lower.contains("reserv") {
                "Reservations accepted for groups of 4+. Walk-ins welcome."
            } else {
                "Please contact us directly for specific policy questions."
            };
            serde_json::json!({ "policy": policy })
        }

        "get_past_replies" => {
            serde_json::json!({
                "replies": [
                    "Thank you so much for the kind words! We're delighted you enjoyed your experience and look forward to welcoming you back.",
                    "We appreciate your feedback and are sorry to hear about your experience. Please reach out to us directly so we can make it right."
                ]
            })
        }

        "check_banned_phrases" => {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let banned = ["refund", "lawsuit", "compensation", "call me", "email me"];
            let lower = text.to_lowercase();
            let found: Vec<&str> = banned
                .iter()
                .copied()
                .filter(|p| lower.contains(p))
                .collect();
            serde_json::json!({ "banned_phrases": found, "clean": found.is_empty() })
        }

        _ => serde_json::json!({ "error": format!("Unknown tool: {name}") }),
    }
}

// ---------------------------------------------------------------------------
// Guardrail helpers
// ---------------------------------------------------------------------------

/// Scan the final response text for banned phrases (always-on guardrail).
fn scan_banned_phrases(text: &str) -> Vec<String> {
    let banned = ["refund", "lawsuit", "compensation", "DM me your phone", "email me"];
    let lower = text.to_lowercase();
    banned
        .iter()
        .copied()
        .filter(|p| lower.contains(&p.to_lowercase()))
        .map(|p| format!("Banned phrases detected: {p}"))
        .collect()
}

const MAX_GUARDRAIL_RETRIES: u8 = 1;
const MAX_TOOL_ITERATIONS: u8 = 3;

// ---------------------------------------------------------------------------
// Agent entry point
// ---------------------------------------------------------------------------

/// Run the agent for a single review, producing a draft.
///
/// The agent uses Anthropic's tool-use protocol. On each turn the LLM may
/// request tool calls (`lookup_menu_item`, `lookup_policy`, `get_past_replies`,
/// `check_banned_phrases`); the agent executes them deterministically and feeds
/// results back until the model produces a final text reply or
/// `MAX_TOOL_ITERATIONS` is reached.
///
/// If guardrails fail on the first attempt the agent retries once with a
/// corrective note appended to the hint. If the retry also fails, the draft is
/// stored with guardrail warnings for human review.
pub async fn run_agent<C: LlmClient + ?Sized>(
    llm: &C,
    config: &AgentConfig,
    review: &Review,
    hint: Option<String>,
) -> Result<AgentRunResult, AgentError> {
    let model_tier = select_model_tier(review);
    let char_limit = review.reply_char_limit();
    let checks = domain::guardrails::default_checks();
    let tools = define_tools();

    let guardrail_ctx = GuardrailContext {
        platform: review.platform,
        review_language: review.body_language.clone(),
        review_rating: review.rating,
        char_limit,
    };

    let mut total_prompt_tokens = 0u32;
    let mut total_completion_tokens = 0u32;
    let mut total_latency_ms = 0u64;
    let mut last_model_name = String::new();
    let mut last_fingerprint = String::new();
    let mut all_tool_calls: Vec<ToolCall> = Vec::new();
    let mut attempts = 0u8;
    let mut current_hint = hint;

    loop {
        // Build the request (metadata + tool definitions).
        let request = GenerateRequest {
            review_text: review.body_text.clone(),
            review_rating: review.rating,
            review_language: review.body_language.clone(),
            platform: review.platform,
            restaurant_name: config.restaurant_name.clone(),
            restaurant_context: config.restaurant_context.clone(),
            model_tier,
            max_chars: char_limit,
            hint: current_hint.clone(),
            tools: tools.clone(),
        };

        // Seed the conversation with the initial user prompt.
        let initial_prompt = build_prompt(&request);
        let mut history: Vec<Message> = vec![Message {
            role: "user".into(),
            content: MessageContent::Text(initial_prompt),
        }];

        // Inner tool-use loop.
        let mut iteration = 0u8;
        let final_response: GenerateResponse;

        loop {
            let response = llm
                .generate_with_history(request.clone(), history.clone())
                .await?;

            total_prompt_tokens =
                total_prompt_tokens.saturating_add(response.prompt_tokens);
            total_completion_tokens =
                total_completion_tokens.saturating_add(response.completion_tokens);
            total_latency_ms = total_latency_ms.saturating_add(response.latency_ms);
            last_model_name.clone_from(&response.model_name);
            last_fingerprint.clone_from(&response.prompt_fingerprint);

            if !response.needs_tool_execution || iteration >= MAX_TOOL_ITERATIONS {
                final_response = response;
                break;
            }

            // Append the assistant turn (may contain text + tool_use blocks).
            history.push(Message {
                role: "assistant".into(),
                content: MessageContent::Blocks(response.content_blocks.clone()),
            });

            // Execute each requested tool and collect results.
            let mut result_blocks: Vec<ContentBlock> = Vec::new();
            for tc in &response.tool_calls {
                all_tool_calls.push(ToolCall {
                    name: tc.name.clone(),
                });
                let result = execute_tool(&tc.name, &tc.input, review, config);
                result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: result,
                });
            }

            // Feed tool results back as a user turn.
            history.push(Message {
                role: "user".into(),
                content: MessageContent::Blocks(result_blocks),
            });

            iteration += 1;
        }

        // Run guardrails on the final reply.
        let guardrail_result = domain::guardrails::evaluate_guardrails(
            &final_response.reply_text,
            &guardrail_ctx,
            &checks,
        );

        let banned_warnings = scan_banned_phrases(&final_response.reply_text);
        let has_guardrail_warnings = !guardrail_result.warnings.is_empty();
        let has_any_warnings = has_guardrail_warnings || !banned_warnings.is_empty();

        if !has_any_warnings || attempts >= MAX_GUARDRAIL_RETRIES {
            let flags = classify_flags(review);
            let tool_calls_count = u8::try_from(all_tool_calls.len()).unwrap_or(u8::MAX);
            let tool_calls_json = serde_json::to_value(&all_tool_calls)
                .unwrap_or_else(|_| serde_json::json!([]));

            let mut draft = ReplyDraft::new_pending(
                review.id,
                final_response.reply_text,
                review.body_language.clone().unwrap_or_else(|| "en".into()),
            );
            draft.model_name = Some(final_response.model_name);
            draft.prompt_fingerprint = Some(last_fingerprint.clone());
            draft.generated_by = Generator::AgentLlm;

            let mut all_warnings: Vec<String> = guardrail_result
                .warnings
                .iter()
                .map(|w| w.rule.clone())
                .collect();
            all_warnings.extend(banned_warnings);
            draft.guardrail_warnings = all_warnings;
            draft.flags = flags;
            if has_any_warnings {
                draft.flags.push("guardrail_warning".into());
            }

            return Ok(AgentRunResult {
                draft,
                model_name: last_model_name,
                prompt_fingerprint: last_fingerprint,
                prompt_tokens: total_prompt_tokens,
                completion_tokens: total_completion_tokens,
                latency_ms: total_latency_ms,
                tool_calls: tool_calls_count,
                tool_calls_json,
                guardrail_result,
            });
        }

        // Build corrective hint and retry.
        let violation_rules: Vec<String> = guardrail_result
            .warnings
            .iter()
            .map(|w| format!("{}: {}", w.rule, w.message))
            .collect();
        let mut combined = violation_rules;
        combined.extend(banned_warnings);
        let corrective_note = format!(
            "Your previous reply violated these guardrails: {}. Please fix these issues.",
            combined.join("; ")
        );
        current_hint = Some(match current_hint {
            Some(h) => format!("{h}. {corrective_note}"),
            None => corrective_note,
        });
        attempts += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DraftState, Platform, ReviewAuthor, ReviewStatus};
    use llm_client::InMemoryLlm;
    use serde_json::json;
    use time::macros::datetime;
    use uuid::Uuid;

    fn review(rating: u8, body: Option<&str>) -> Review {
        Review {
            id: Uuid::new_v4(),
            platform: Platform::Google,
            source_review_id: "rev-1".into(),
            source_location_id: "loc-1".into(),
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
    fn model_tier_standard_for_high_rating() {
        assert_eq!(select_model_tier(&review(5, Some("Great!"))), ModelTier::Standard);
        assert_eq!(select_model_tier(&review(4, Some("Good"))), ModelTier::Standard);
        assert_eq!(select_model_tier(&review(3, Some("OK"))), ModelTier::Standard);
    }

    #[test]
    fn model_tier_escalation_for_low_rating() {
        assert_eq!(select_model_tier(&review(1, Some("Bad"))), ModelTier::Escalation);
        assert_eq!(select_model_tier(&review(2, Some("Meh"))), ModelTier::Escalation);
    }

    #[test]
    fn model_tier_escalation_for_sensitive_keywords() {
        assert_eq!(
            select_model_tier(&review(5, Some("I had food poisoning"))),
            ModelTier::Escalation
        );
        assert_eq!(
            select_model_tier(&review(4, Some("My allergy was ignored"))),
            ModelTier::Escalation
        );
    }

    #[test]
    fn classify_flags_for_ratings() {
        assert!(classify_flags(&review(1, Some("Bad"))).contains(&"sensitive".into()));
        assert!(classify_flags(&review(3, Some("OK"))).contains(&"needs_attention".into()));
        assert!(classify_flags(&review(5, Some("Great!"))).is_empty());
    }

    #[tokio::test]
    async fn run_agent_produces_draft() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(5, Some("Amazing food!"));
        let result = run_agent(&llm, &config, &r, None).await.unwrap();

        assert_eq!(result.draft.review_id, r.id);
        assert_eq!(result.draft.state, DraftState::PendingReview);
        assert!(!result.draft.text.is_empty());
        assert!(result.draft.model_name.is_some());
    }

    #[tokio::test]
    async fn run_agent_with_hint() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(3, Some("Mediocre"));
        let result = run_agent(&llm, &config, &r, Some("be warmer".into()))
            .await
            .unwrap();
        assert!(result.draft.text.contains("be warmer"));
    }

    #[test]
    fn no_sensitive_keywords_in_clean_review() {
        assert!(!contains_sensitive_keywords(&review(5, Some("Lovely pasta!"))));
    }

    #[test]
    fn sensitive_keywords_detected() {
        assert!(contains_sensitive_keywords(&review(4, Some("I got food poisoning here"))));
        assert!(contains_sensitive_keywords(&review(3, Some("My allergy was ignored"))));
    }

    #[test]
    fn no_sensitive_keywords_when_no_body() {
        assert!(!contains_sensitive_keywords(&review(5, None)));
    }

    #[test]
    fn classify_flags_sensitive_keyword_high_rating() {
        let flags = classify_flags(&review(5, Some("I had an allergic reaction")));
        assert!(flags.contains(&"sensitive".into()));
    }

    #[tokio::test]
    async fn run_agent_for_rating_only_review() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(5, None);
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert_eq!(result.draft.review_id, r.id);
        assert_eq!(result.draft.language, "en");
    }

    #[tokio::test]
    async fn run_agent_low_rating_gets_sensitive_flag() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(1, Some("Terrible service"));
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert!(result.draft.flags.contains(&"sensitive".into()));
        assert_eq!(result.model_name, "in-memory-escalation");
    }

    #[tokio::test]
    async fn tool_calls_are_bounded() {
        // With InMemoryLlm the model never requests tool calls, so tool_calls == 0.
        // The important invariant is that it never exceeds MAX_TOOL_ITERATIONS.
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(5, Some("Amazing pasta!"));
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert!(result.tool_calls <= 3);
    }

    #[tokio::test]
    async fn banned_phrase_adds_guardrail_warning_flag() {
        let llm = InMemoryLlm {
            default_reply: "We can offer a full refund. Please email me.".into(),
        };
        let config = AgentConfig::default();
        let r = review(5, Some("ok"));
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert!(result.draft.flags.contains(&"guardrail_warning".into()));
        assert!(!result.draft.guardrail_warnings.is_empty());
    }

    #[tokio::test]
    async fn run_agent_three_star_gets_needs_attention() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(3, Some("Average experience"));
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert!(result.draft.flags.contains(&"needs_attention".into()));
    }

    #[tokio::test]
    async fn run_agent_ubereats_platform() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let mut r = review(5, Some("Great food!"));
        r.platform = Platform::Ubereats;
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert_eq!(result.draft.review_id, r.id);
    }

    #[test]
    fn agent_config_defaults() {
        let config = AgentConfig::default();
        assert_eq!(config.restaurant_name, "Chez Luca");
        assert!(!config.restaurant_context.is_empty());
    }

    #[test]
    fn define_tools_returns_four_tools() {
        let tools = define_tools();
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"lookup_menu_item"));
        assert!(names.contains(&"lookup_policy"));
        assert!(names.contains(&"get_past_replies"));
        assert!(names.contains(&"check_banned_phrases"));
    }

    #[test]
    fn execute_tool_lookup_policy_refunds() {
        let r = review(5, None);
        let config = AgentConfig::default();
        let result = execute_tool(
            "lookup_policy",
            &serde_json::json!({"topic": "refund policy"}),
            &r,
            &config,
        );
        assert!(result["policy"].as_str().unwrap().contains("refund"));
    }

    #[test]
    fn execute_tool_lookup_menu_item_found() {
        let r = review(5, None);
        let config = AgentConfig::default();
        let result = execute_tool(
            "lookup_menu_item",
            &serde_json::json!({"name": "pasta bolognese"}),
            &r,
            &config,
        );
        assert_eq!(result["found"], true);
    }

    #[test]
    fn execute_tool_check_banned_phrases_clean() {
        let r = review(5, None);
        let config = AgentConfig::default();
        let result = execute_tool(
            "check_banned_phrases",
            &serde_json::json!({"text": "Thank you for visiting us!"}),
            &r,
            &config,
        );
        assert_eq!(result["clean"], true);
    }

    #[test]
    fn execute_tool_check_banned_phrases_dirty() {
        let r = review(5, None);
        let config = AgentConfig::default();
        let result = execute_tool(
            "check_banned_phrases",
            &serde_json::json!({"text": "We will offer a full refund."}),
            &r,
            &config,
        );
        assert_eq!(result["clean"], false);
    }
}
