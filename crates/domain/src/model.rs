use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

/// Source platform for a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Google,
    Ubereats,
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Google => f.write_str("google"),
            Self::Ubereats => f.write_str("ubereats"),
        }
    }
}

/// Lifecycle status of a review within our system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    New,
    Drafting,
    AwaitingHuman,
    Replied,
    Withdrawn,
    Skipped,
}

impl fmt::Display for ReviewStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New => f.write_str("new"),
            Self::Drafting => f.write_str("drafting"),
            Self::AwaitingHuman => f.write_str("awaiting_human"),
            Self::Replied => f.write_str("replied"),
            Self::Withdrawn => f.write_str("withdrawn"),
            Self::Skipped => f.write_str("skipped"),
        }
    }
}

/// How a reply draft was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Generator {
    AgentLlm,
    HumanEdit,
    Template,
}

impl fmt::Display for Generator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentLlm => f.write_str("agent_llm"),
            Self::HumanEdit => f.write_str("human_edit"),
            Self::Template => f.write_str("template"),
        }
    }
}

/// Fixed reasons for rejecting a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    TooGeneric,
    WrongTone,
    FactuallyIncorrect,
    OffPolicy,
    LanguageMismatch,
    Other,
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooGeneric => f.write_str("too_generic"),
            Self::WrongTone => f.write_str("wrong_tone"),
            Self::FactuallyIncorrect => f.write_str("factually_incorrect"),
            Self::OffPolicy => f.write_str("off_policy"),
            Self::LanguageMismatch => f.write_str("language_mismatch"),
            Self::Other => f.write_str("other"),
        }
    }
}

/// Role within the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    Owner,
    Manager,
    Viewer,
}

impl fmt::Display for UserRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner => f.write_str("owner"),
            Self::Manager => f.write_str("manager"),
            Self::Viewer => f.write_str("viewer"),
        }
    }
}

/// Notification event categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    DraftReady,
    SensitiveReview,
    SlaBreach,
    SlaEscalation,
    IngestionFailure,
    PostFailed,
    DriftDetected,
}

impl fmt::Display for NotificationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DraftReady => f.write_str("draft_ready"),
            Self::SensitiveReview => f.write_str("sensitive_review"),
            Self::SlaBreach => f.write_str("sla_breach"),
            Self::SlaEscalation => f.write_str("sla_escalation"),
            Self::IngestionFailure => f.write_str("ingestion_failure"),
            Self::PostFailed => f.write_str("post_failed"),
            Self::DriftDetected => f.write_str("drift_detected"),
        }
    }
}

/// Author details extracted from the review source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAuthor {
    pub display_name: String,
    pub avatar_url: Option<Url>,
}

/// A customer review normalised from any supported platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub id: Uuid,
    pub platform: Platform,
    pub source_review_id: String,
    pub source_location_id: String,
    pub author: ReviewAuthor,
    pub rating: u8,
    pub body_text: Option<String>,
    pub body_language: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ingested_at: OffsetDateTime,
    pub existing_reply_text: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub existing_reply_updated_at: Option<OffsetDateTime>,
    pub status: ReviewStatus,
    pub context_json: serde_json::Value,
    pub raw_payload: serde_json::Value,
}

impl Review {
    /// Returns `true` when the rating warrants sensitive handling (1-2 stars).
    #[must_use]
    pub fn is_sensitive_rating(&self) -> bool {
        self.rating <= 2
    }

    /// Returns `true` when the review has body text (not rating-only).
    #[must_use]
    pub fn has_body(&self) -> bool {
        self.body_text.as_ref().is_some_and(|t| !t.trim().is_empty())
    }

    /// Platform-specific character limit for replies.
    #[must_use]
    pub fn reply_char_limit(&self) -> u32 {
        match self.platform {
            Platform::Google => 1000,
            Platform::Ubereats => 500,
        }
    }
}

/// An AI-generated (or human-edited) draft reply to a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyDraft {
    pub id: Uuid,
    pub review_id: Uuid,
    pub generated_by: Generator,
    pub model_name: Option<String>,
    pub prompt_fingerprint: Option<String>,
    pub text: String,
    pub language: String,
    pub char_count: u32,
    pub state: crate::fsm::DraftState,
    pub guardrail_warnings: Vec<String>,
    pub flags: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub reviewed_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub reviewed_at: Option<OffsetDateTime>,
    pub rejection_reason: Option<String>,
    /// Earliest time the system may begin posting this draft to the platform.
    ///
    /// Used to enforce the 10-second undo window for bulk approvals.
    #[serde(with = "time::serde::rfc3339::option")]
    pub post_eligible_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub posted_at: Option<OffsetDateTime>,
    pub platform_post_error: Option<String>,
}

/// A single execution of the AI agent for a review.
///
/// This is the durable trace record that links:
/// - the input review
/// - the exact prompt fingerprint sent to the model
/// - the resulting draft (when successful)
/// - observability fields (tokens, latency, tool calls, guardrail verdict)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: Uuid,
    pub review_id: Uuid,
    pub draft_id: Option<Uuid>,
    pub model_name: Option<String>,
    pub prompt_fingerprint: Option<String>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub latency_ms: Option<u64>,
    /// JSON array of tool call records (name/args/result hashes).
    pub tool_calls_json: serde_json::Value,
    /// Guardrail verdict payload (serialized).
    pub guardrail_verdict_json: Option<serde_json::Value>,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl ReplyDraft {
    /// Create a new draft in `PendingReview` state.
    #[must_use]
    pub fn new_pending(review_id: Uuid, text: String, language: String) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: Uuid::new_v4(),
            review_id,
            generated_by: Generator::AgentLlm,
            model_name: None,
            prompt_fingerprint: None,
            char_count: u32::try_from(text.chars().count()).unwrap_or(u32::MAX),
            text,
            language,
            state: crate::fsm::DraftState::PendingReview,
            guardrail_warnings: Vec::new(),
            flags: Vec::new(),
            created_at: now,
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            post_eligible_at: None,
            posted_at: None,
            platform_post_error: None,
        }
    }

    /// Whether this draft has any guardrail warnings the human should see.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        !self.guardrail_warnings.is_empty()
    }

    /// Whether this draft is in an active (non-terminal) state.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            crate::fsm::DraftState::PendingReview
                | crate::fsm::DraftState::Approved
                | crate::fsm::DraftState::Edited
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn sample_review(rating: u8) -> Review {
        Review {
            id: Uuid::new_v4(),
            platform: Platform::Google,
            source_review_id: "abc123".into(),
            source_location_id: "loc1".into(),
            author: ReviewAuthor {
                display_name: "Test User".into(),
                avatar_url: None,
            },
            rating,
            body_text: Some("Great food!".into()),
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
    fn sensitive_rating_boundary() {
        assert!(sample_review(1).is_sensitive_rating());
        assert!(sample_review(2).is_sensitive_rating());
        assert!(!sample_review(3).is_sensitive_rating());
        assert!(!sample_review(5).is_sensitive_rating());
    }

    #[test]
    fn reply_char_limit_per_platform() {
        let mut r = sample_review(5);
        assert_eq!(r.reply_char_limit(), 1000);
        r.platform = Platform::Ubereats;
        assert_eq!(r.reply_char_limit(), 500);
    }

    #[test]
    fn has_body_excludes_empty_and_whitespace() {
        let mut r = sample_review(5);
        assert!(r.has_body());
        r.body_text = Some("   ".into());
        assert!(!r.has_body());
        r.body_text = None;
        assert!(!r.has_body());
    }

    #[test]
    fn draft_is_active_states() {
        let d = ReplyDraft::new_pending(Uuid::new_v4(), "hi".into(), "en".into());
        assert!(d.is_active());
        assert!(!d.has_warnings());
    }

    #[test]
    fn platform_display() {
        assert_eq!(Platform::Google.to_string(), "google");
        assert_eq!(Platform::Ubereats.to_string(), "ubereats");
    }

    #[test]
    fn review_status_display() {
        assert_eq!(ReviewStatus::AwaitingHuman.to_string(), "awaiting_human");
    }

    #[test]
    fn rejection_reason_display() {
        assert_eq!(RejectionReason::TooGeneric.to_string(), "too_generic");
        assert_eq!(
            RejectionReason::FactuallyIncorrect.to_string(),
            "factually_incorrect"
        );
    }

    #[test]
    fn user_role_display() {
        assert_eq!(UserRole::Owner.to_string(), "owner");
        assert_eq!(UserRole::Manager.to_string(), "manager");
        assert_eq!(UserRole::Viewer.to_string(), "viewer");
    }

    #[test]
    fn notification_type_display() {
        assert_eq!(
            NotificationType::SensitiveReview.to_string(),
            "sensitive_review"
        );
    }

    #[test]
    fn review_round_trips_through_json() {
        let r = sample_review(4);
        let json = serde_json::to_string(&r).expect("serialize");
        let r2: Review = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, r2);
    }

    #[test]
    fn draft_round_trips_through_json() {
        let d = ReplyDraft::new_pending(Uuid::new_v4(), "thanks!".into(), "en".into());
        let json = serde_json::to_string(&d).expect("serialize");
        let d2: ReplyDraft = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(d, d2);
    }
}
