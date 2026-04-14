//! AI response generation agent — drafts replies, runs guardrails, and
//! manages the agentic loop with bounded tool calls.

use domain::{Generator, GuardrailContext, GuardrailResult, ReplyDraft, Review};
use llm_client::{GenerateRequest, GenerateResponse, LlmClient, ModelTier};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCall {
    name: String,
}

#[derive(Debug, Clone)]
struct ToolContext {
    extra_context: String,
    calls: Vec<ToolCall>,
}

impl ToolContext {
    fn empty() -> Self {
        Self {
            extra_context: String::new(),
            calls: Vec::new(),
        }
    }

    fn tool_calls_count(&self) -> u8 {
        u8::try_from(self.calls.len()).unwrap_or(u8::MAX)
    }
}

fn lookup_policy(review: &Review) -> ToolContext {
    let mut ctx = ToolContext::empty();
    ctx.calls.push(ToolCall {
        name: "lookup_policy".into(),
    });

    // Minimal policy hints consistent with SPECS: no refunds/liability, human escalation for sensitive.
    let mut notes = Vec::new();
    notes.push("Do not offer refunds or admit liability.".to_string());
    notes.push("Do not request personal info (phone/email) publicly.".to_string());
    if review.is_sensitive_rating() || contains_sensitive_keywords(review) {
        notes.push("Sensitive: be empathetic, invite offline follow-up, avoid specifics.".to_string());
    }
    ctx.extra_context = format!("Policy:\n- {}", notes.join("\n- "));
    ctx
}

fn lookup_menu_item(review: &Review) -> Option<ToolContext> {
    let body = review.body_text.as_ref()?;
    let lower = body.to_lowercase();
    let needles = ["pasta", "naan", "biryani", "pizza", "salad"];
    let mentioned: Vec<&str> = needles.iter().copied().filter(|n| lower.contains(n)).collect();
    if mentioned.is_empty() {
        return None;
    }
    let mut ctx = ToolContext::empty();
    ctx.calls.push(ToolCall {
        name: "lookup_menu_item".into(),
    });
    ctx.extra_context = format!(
        "Menu items mentioned: {} (acknowledge specifically if appropriate).",
        mentioned.join(", ")
    );
    Some(ctx)
}

fn get_past_replies(_review: &Review) -> ToolContext {
    let mut ctx = ToolContext::empty();
    ctx.calls.push(ToolCall {
        name: "get_past_replies".into(),
    });
    ctx.extra_context = "Past replies: (none available in this demo build)".into();
    ctx
}

fn translate_hint(review: &Review) -> Option<ToolContext> {
    let lang = review.body_language.as_deref()?;
    if lang.eq_ignore_ascii_case("en") {
        return None;
    }
    let mut ctx = ToolContext::empty();
    ctx.calls.push(ToolCall { name: "translate".into() });
    ctx.extra_context = format!(
        "Language: respond in {lang}. If unsure, keep it simple and polite."
    );
    Some(ctx)
}

fn check_banned_phrases(text: &str) -> Option<ToolContext> {
    // Minimal, deterministic banned phrase scan. This intentionally mirrors guardrail intent.
    let banned = ["refund", "lawsuit", "compensation", "DM me your phone", "email me"];
    let lower = text.to_lowercase();
    let found: Vec<&str> = banned.iter().copied().filter(|p| lower.contains(&p.to_lowercase())).collect();
    if found.is_empty() {
        return None;
    }
    let mut ctx = ToolContext::empty();
    ctx.calls.push(ToolCall {
        name: "check_banned_phrases".into(),
    });
    ctx.extra_context = format!("Banned phrases detected: {}", found.join(", "));
    Some(ctx)
}

fn build_tool_context(review: &Review) -> ToolContext {
    // Bounded tool calls (max 3) as per SPECS.
    let mut merged = ToolContext::empty();

    let candidates: Vec<Option<ToolContext>> = vec![
        Some(lookup_policy(review)),
        lookup_menu_item(review),
        translate_hint(review),
        Some(get_past_replies(review)),
    ];

    for c in candidates.into_iter().flatten() {
        if merged.calls.len() >= 3 {
            break;
        }
        if !merged.extra_context.is_empty() {
            merged.extra_context.push_str("\n\n");
        }
        merged.extra_context.push_str(&c.extra_context);
        merged.calls.extend(c.calls);
    }

    merged
}

/// Result of a single agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub draft: ReplyDraft,
    pub model_name: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub latency_ms: u64,
    pub tool_calls: u8,
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
        Self {
            restaurant_name: "Chez Luca".into(),
            restaurant_context: "A small family-run Italian trattoria in the neighbourhood.".into(),
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

const MAX_GUARDRAIL_RETRIES: u8 = 1;

/// Run the agent for a single review, producing a draft.
///
/// If guardrails fail on the first attempt, the agent retries once with a
/// corrective system note appended to the hint. If the retry also fails,
/// the draft is stored with guardrail warnings for human review.
pub async fn run_agent<C: LlmClient + ?Sized>(
    llm: &C,
    config: &AgentConfig,
    review: &Review,
    hint: Option<String>,
) -> Result<AgentRunResult, AgentError> {
    let model_tier = select_model_tier(review);
    let char_limit = review.reply_char_limit();
    let checks = domain::guardrails::default_checks();
    let tool_ctx = build_tool_context(review);

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
    let mut attempts = 0u8;
    let mut current_hint = hint;

    loop {
        let request = GenerateRequest {
            review_text: review.body_text.clone(),
            review_rating: review.rating,
            review_language: review.body_language.clone(),
            platform: review.platform,
            restaurant_name: config.restaurant_name.clone(),
            restaurant_context: if tool_ctx.extra_context.is_empty() {
                config.restaurant_context.clone()
            } else {
                format!("{}\n\n{}", config.restaurant_context, tool_ctx.extra_context)
            },
            model_tier,
            max_chars: char_limit,
            hint: current_hint.clone(),
        };

        let response: GenerateResponse = llm.generate(request).await?;

        total_prompt_tokens = total_prompt_tokens.saturating_add(response.prompt_tokens);
        total_completion_tokens = total_completion_tokens.saturating_add(response.completion_tokens);
        total_latency_ms = total_latency_ms.saturating_add(response.latency_ms);
        last_model_name.clone_from(&response.model_name);

        let guardrail_result = domain::guardrails::evaluate_guardrails(
            &response.reply_text,
            &guardrail_ctx,
            &checks,
        );

        let banned_phrase_ctx = check_banned_phrases(&response.reply_text);
        let extra_warnings: Vec<String> = banned_phrase_ctx
            .as_ref()
            .map(|c| vec![c.extra_context.clone()])
            .unwrap_or_default();

        let has_warnings = !guardrail_result.warnings.is_empty();
        let has_any_warnings = has_warnings || !extra_warnings.is_empty();

        if !has_any_warnings || attempts >= MAX_GUARDRAIL_RETRIES {
            let flags = classify_flags(review);

            let mut draft = ReplyDraft::new_pending(
                review.id,
                response.reply_text,
                review.body_language.clone().unwrap_or_else(|| "en".into()),
            );
            draft.model_name = Some(response.model_name);
            draft.generated_by = Generator::AgentLlm;
            let mut all_warnings: Vec<String> = guardrail_result
                .warnings
                .iter()
                .map(|w| w.rule.clone())
                .collect();
            all_warnings.extend(extra_warnings);
            draft.guardrail_warnings = all_warnings;
            draft.flags = flags;
            if has_any_warnings {
                draft.flags.push("guardrail_warning".into());
            }

            return Ok(AgentRunResult {
                draft,
                model_name: last_model_name,
                prompt_tokens: total_prompt_tokens,
                completion_tokens: total_completion_tokens,
                latency_ms: total_latency_ms,
                tool_calls: tool_ctx.tool_calls_count(),
                guardrail_result,
            });
        }

        let violation_rules: Vec<String> = guardrail_result
            .warnings
            .iter()
            .map(|w| format!("{}: {}", w.rule, w.message))
            .collect();
        let mut combined = violation_rules;
        if let Some(c) = banned_phrase_ctx {
            combined.push(c.extra_context);
        }
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
    async fn tool_calls_are_bounded_and_reported() {
        let llm = InMemoryLlm::default();
        let config = AgentConfig::default();
        let r = review(5, Some("Amazing pasta!"));
        let result = run_agent(&llm, &config, &r, None).await.unwrap();
        assert!(result.tool_calls <= 3);
        assert!(result.tool_calls > 0);
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
}
