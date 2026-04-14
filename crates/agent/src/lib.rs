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

/// Run the agent for a single review, producing a draft.
pub async fn run_agent<C: LlmClient>(
    llm: &C,
    config: &AgentConfig,
    review: &Review,
    hint: Option<String>,
) -> Result<AgentRunResult, AgentError> {
    let model_tier = select_model_tier(review);
    let char_limit = review.reply_char_limit();

    let request = GenerateRequest {
        review_text: review.body_text.clone(),
        review_rating: review.rating,
        review_language: review.body_language.clone(),
        platform: review.platform,
        restaurant_name: config.restaurant_name.clone(),
        restaurant_context: config.restaurant_context.clone(),
        model_tier,
        max_chars: char_limit,
        hint,
    };

    let response: GenerateResponse = llm.generate(request).await?;

    let guardrail_ctx = GuardrailContext {
        platform: review.platform,
        review_language: review.body_language.clone(),
        review_rating: review.rating,
        char_limit,
    };

    let checks = domain::guardrails::default_checks();
    let guardrail_result = domain::guardrails::evaluate_guardrails(
        &response.reply_text,
        &guardrail_ctx,
        &checks,
    );

    let flags = classify_flags(review);
    let warnings: Vec<String> = guardrail_result
        .warnings
        .iter()
        .map(|w| w.rule.clone())
        .collect();

    let mut draft = ReplyDraft::new_pending(
        review.id,
        response.reply_text,
        review.body_language.clone().unwrap_or_else(|| "en".into()),
    );
    draft.model_name = Some(response.model_name.clone());
    draft.generated_by = Generator::AgentLlm;
    draft.guardrail_warnings = warnings;
    draft.flags = flags;

    Ok(AgentRunResult {
        draft,
        model_name: response.model_name,
        prompt_tokens: response.prompt_tokens,
        completion_tokens: response.completion_tokens,
        latency_ms: response.latency_ms,
        tool_calls: 0,
        guardrail_result,
    })
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
}
