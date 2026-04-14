use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Google,
    Ubereats,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Generator {
    AgentLlm,
    HumanEdit,
    Template,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAuthor {
    pub display_name: String,
    pub avatar_url: Option<Url>,
}

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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub reviewed_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub reviewed_at: Option<OffsetDateTime>,
    pub rejection_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub posted_at: Option<OffsetDateTime>,
    pub platform_post_error: Option<String>,
}

impl ReplyDraft {
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
            created_at: now,
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            posted_at: None,
            platform_post_error: None,
        }
    }
}

