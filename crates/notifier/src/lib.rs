//! Notification system — sends alerts to the owner via email, push, and SMS.

use std::future::Future;

use domain::NotificationType;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum NotifierError {
    #[error("channel {channel} delivery failed: {reason}")]
    DeliveryFailed { channel: String, reason: String },

    #[error("all retries exhausted for notification {0}")]
    RetriesExhausted(Uuid),
}

/// Delivery channel for a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Email,
    Push,
    Sms,
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Email => f.write_str("email"),
            Self::Push => f.write_str("push"),
            Self::Sms => f.write_str("sms"),
        }
    }
}

/// A notification ready for delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub notification_type: NotificationType,
    pub channel: Channel,
    pub recipient: String,
    pub subject: String,
    pub body: String,
    pub entity_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Notification {
    #[must_use]
    pub fn new(
        notification_type: NotificationType,
        channel: Channel,
        recipient: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            notification_type,
            channel,
            recipient: recipient.into(),
            subject: subject.into(),
            body: body.into(),
            entity_id: None,
            created_at: OffsetDateTime::now_utc(),
        }
    }
}

/// Determine which channels to use for a given notification type.
///
/// Each type is listed individually because the channel set is specified per-type
/// in the notification spec and may diverge independently.
#[must_use]
#[allow(clippy::match_same_arms)]
pub fn channels_for_type(notification_type: NotificationType) -> Vec<Channel> {
    match notification_type {
        NotificationType::DraftReady => vec![Channel::Email, Channel::Push],
        NotificationType::SensitiveReview => vec![Channel::Email, Channel::Push, Channel::Sms],
        NotificationType::SlaBreach => vec![Channel::Email, Channel::Push],
        NotificationType::SlaEscalation => vec![Channel::Email, Channel::Push, Channel::Sms],
        NotificationType::IngestionFailure => vec![Channel::Email],
        NotificationType::PostFailed => vec![Channel::Email, Channel::Push],
        NotificationType::DriftDetected => vec![Channel::Email],
    }
}

/// Quiet hours configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuietHours {
    pub start_hour: u8,
    pub end_hour: u8,
}

impl QuietHours {
    /// Check if a given hour falls within quiet hours.
    #[must_use]
    pub fn is_quiet(&self, hour: u8) -> bool {
        if self.start_hour <= self.end_hour {
            hour >= self.start_hour && hour < self.end_hour
        } else {
            hour >= self.start_hour || hour < self.end_hour
        }
    }

    /// Some notification types bypass quiet hours.
    #[must_use]
    pub fn should_suppress(
        &self,
        notification_type: NotificationType,
        channel: Channel,
        hour: u8,
    ) -> bool {
        if !self.is_quiet(hour) {
            return false;
        }

        // SMS for urgent items is never suppressed
        if channel == Channel::Sms
            && matches!(
                notification_type,
                NotificationType::SensitiveReview | NotificationType::SlaEscalation
            )
        {
            return false;
        }

        true
    }
}

/// Trait abstracting notification delivery for testability.
pub trait NotificationSender: Send + Sync {
    fn send(
        &self,
        notification: &Notification,
    ) -> impl Future<Output = Result<(), NotifierError>> + Send;
}

/// In-memory sender for testing.
#[derive(Debug, Default)]
pub struct InMemoryNotificationSender {
    pub sent: std::sync::Mutex<Vec<Notification>>,
}

impl NotificationSender for InMemoryNotificationSender {
    async fn send(&self, notification: &Notification) -> Result<(), NotifierError> {
        self.sent
            .lock()
            .expect("lock")
            .push(notification.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_review_uses_all_channels() {
        let channels = channels_for_type(NotificationType::SensitiveReview);
        assert!(channels.contains(&Channel::Email));
        assert!(channels.contains(&Channel::Push));
        assert!(channels.contains(&Channel::Sms));
    }

    #[test]
    fn draft_ready_no_sms() {
        let channels = channels_for_type(NotificationType::DraftReady);
        assert!(!channels.contains(&Channel::Sms));
    }

    #[test]
    fn ingestion_failure_email_only() {
        let channels = channels_for_type(NotificationType::IngestionFailure);
        assert_eq!(channels, vec![Channel::Email]);
    }

    #[test]
    fn quiet_hours_wrapping_midnight() {
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        assert!(qh.is_quiet(22));
        assert!(qh.is_quiet(0));
        assert!(qh.is_quiet(3));
        assert!(qh.is_quiet(7));
        assert!(!qh.is_quiet(8));
        assert!(!qh.is_quiet(12));
        assert!(!qh.is_quiet(21));
    }

    #[test]
    fn quiet_hours_same_day() {
        let qh = QuietHours {
            start_hour: 13,
            end_hour: 15,
        };
        assert!(!qh.is_quiet(12));
        assert!(qh.is_quiet(13));
        assert!(qh.is_quiet(14));
        assert!(!qh.is_quiet(15));
    }

    #[test]
    fn sms_for_sensitive_not_suppressed_in_quiet() {
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        assert!(!qh.should_suppress(NotificationType::SensitiveReview, Channel::Sms, 23));
        assert!(!qh.should_suppress(NotificationType::SlaEscalation, Channel::Sms, 2));
    }

    #[test]
    fn email_suppressed_in_quiet() {
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        assert!(qh.should_suppress(NotificationType::DraftReady, Channel::Email, 23));
    }

    #[test]
    fn nothing_suppressed_outside_quiet() {
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        assert!(!qh.should_suppress(NotificationType::DraftReady, Channel::Email, 12));
    }

    #[test]
    fn channel_display() {
        assert_eq!(Channel::Email.to_string(), "email");
        assert_eq!(Channel::Push.to_string(), "push");
        assert_eq!(Channel::Sms.to_string(), "sms");
    }

    #[tokio::test]
    async fn in_memory_sender_records() {
        let sender = InMemoryNotificationSender::default();
        let notif = Notification::new(
            NotificationType::DraftReady,
            Channel::Email,
            "owner@example.com",
            "New draft ready",
            "A new 5-star review has been drafted.",
        );
        sender.send(&notif).await.unwrap();
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
    }
}
