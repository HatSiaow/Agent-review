use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Who or what initiated an audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    User,
    System,
    Agent,
}

impl fmt::Display for ActorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::System => f.write_str("system"),
            Self::Agent => f.write_str("agent"),
        }
    }
}

/// Categories of auditable events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    ReviewIngested,
    ReviewWithdrawn,
    ReviewSkipped,
    ReviewUnskipped,
    DraftCreated,
    DraftApproved,
    DraftEdited,
    DraftRejected,
    DraftPosted,
    DraftPostFailed,
    DriftDetected,
    LoginSuccess,
    LoginFailed,
    PasswordReset,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReviewIngested => f.write_str("review_ingested"),
            Self::ReviewWithdrawn => f.write_str("review_withdrawn"),
            Self::ReviewSkipped => f.write_str("review_skipped"),
            Self::ReviewUnskipped => f.write_str("review_unskipped"),
            Self::DraftCreated => f.write_str("draft_created"),
            Self::DraftApproved => f.write_str("draft_approved"),
            Self::DraftEdited => f.write_str("draft_edited"),
            Self::DraftRejected => f.write_str("draft_rejected"),
            Self::DraftPosted => f.write_str("draft_posted"),
            Self::DraftPostFailed => f.write_str("draft_post_failed"),
            Self::DriftDetected => f.write_str("drift_detected"),
            Self::LoginSuccess => f.write_str("login_success"),
            Self::LoginFailed => f.write_str("login_failed"),
            Self::PasswordReset => f.write_str("password_reset"),
        }
    }
}

/// An append-only record of a state transition or external action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    pub actor_type: ActorType,
    pub actor_id: Option<Uuid>,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub event_type: EventType,
    pub details_json: serde_json::Value,
}

impl AuditEvent {
    #[must_use]
    pub fn new(
        actor_type: ActorType,
        actor_id: Option<Uuid>,
        entity_type: impl Into<String>,
        entity_id: Uuid,
        event_type: EventType,
        details: serde_json::Value,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            occurred_at: OffsetDateTime::now_utc(),
            actor_type,
            actor_id,
            entity_type: entity_type.into(),
            entity_id,
            event_type,
            details_json: details,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn audit_event_round_trips() {
        let event = AuditEvent::new(
            ActorType::User,
            Some(Uuid::new_v4()),
            "review",
            Uuid::new_v4(),
            EventType::DraftApproved,
            json!({"previous_state": "pending_review"}),
        );
        let json_str = serde_json::to_string(&event).expect("serialize");
        let decoded: AuditEvent = serde_json::from_str(&json_str).expect("deserialize");
        assert_eq!(event, decoded);
    }

    #[test]
    fn actor_type_display() {
        assert_eq!(ActorType::User.to_string(), "user");
        assert_eq!(ActorType::System.to_string(), "system");
        assert_eq!(ActorType::Agent.to_string(), "agent");
    }

    #[test]
    fn event_type_display() {
        assert_eq!(EventType::DraftPosted.to_string(), "draft_posted");
        assert_eq!(EventType::DriftDetected.to_string(), "drift_detected");
    }
}
