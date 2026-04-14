//! Notification system — sends alerts to the owner via email, push, and SMS.

use std::future::Future;

use domain::NotificationType;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;
use lettre::AsyncTransport as _;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
}

impl SmtpConfig {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("SMTP_HOST").ok()?;
        let port = std::env::var("SMTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(587);
        let from = std::env::var("SMTP_FROM").ok()?;
        let username = std::env::var("SMTP_USERNAME").ok();
        let password = std::env::var("SMTP_PASSWORD").ok();
        Some(Self {
            host,
            port,
            username,
            password,
            from,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SmtpSender {
    cfg: SmtpConfig,
}

impl SmtpSender {
    #[must_use]
    pub fn new(cfg: SmtpConfig) -> Self {
        Self { cfg }
    }
}

impl NotificationSender for SmtpSender {
    async fn send(&self, notification: &Notification) -> Result<(), NotifierError> {
        if notification.channel != Channel::Email {
            return Ok(());
        }

        let from: lettre::message::Mailbox = self
            .cfg
            .from
            .parse::<lettre::message::Mailbox>()
            .map_err(|e| NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: e.to_string(),
            })?;
        let to: lettre::message::Mailbox = notification
            .recipient
            .parse::<lettre::message::Mailbox>()
            .map_err(|e| NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: e.to_string(),
            })?;

        let email = lettre::Message::builder()
            .from(from)
            .to(to)
            .subject(notification.subject.clone())
            .body(notification.body.clone())
            .map_err(|e| NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: e.to_string(),
            })?;

        let creds = match (&self.cfg.username, &self.cfg.password) {
            (Some(u), Some(p)) => Some(lettre::transport::smtp::authentication::Credentials::new(
                u.clone(),
                p.clone(),
            )),
            _ => None,
        };

        let mut builder = lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&self.cfg.host)
            .map_err(|e| NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: e.to_string(),
            })?;
        builder = builder.port(self.cfg.port);
        if let Some(creds) = creds {
            builder = builder.credentials(creds);
        }
        let transport = builder.build();

        transport
            .send(email)
            .await
            .map(|_| ())
            .map_err(|e| NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: e.to_string(),
            })?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwilioConfig {
    pub account_sid: String,
    pub auth_token: String,
    pub from_number: String,
}

impl TwilioConfig {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Some(Self {
            account_sid: std::env::var("TWILIO_ACCOUNT_SID").ok()?,
            auth_token: std::env::var("TWILIO_AUTH_TOKEN").ok()?,
            from_number: std::env::var("TWILIO_FROM_NUMBER").ok()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TwilioSmsSender {
    cfg: TwilioConfig,
    http: reqwest::Client,
}

impl TwilioSmsSender {
    #[must_use]
    pub fn new(cfg: TwilioConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
        }
    }
}

impl NotificationSender for TwilioSmsSender {
    async fn send(&self, notification: &Notification) -> Result<(), NotifierError> {
        if notification.channel != Channel::Sms {
            return Ok(());
        }

        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            self.cfg.account_sid
        );

        let res = self
            .http
            .post(url)
            .basic_auth(&self.cfg.account_sid, Some(&self.cfg.auth_token))
            .form(&[
                ("From", self.cfg.from_number.as_str()),
                ("To", notification.recipient.as_str()),
                ("Body", notification.body.as_str()),
            ])
            .send()
            .await
            .map_err(|e| NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: e.to_string(),
            })?;

        if !res.status().is_success() {
            let status = res.status().as_u16();
            let body = res.text().await.unwrap_or_default();
            return Err(NotifierError::DeliveryFailed {
                channel: notification.channel.to_string(),
                reason: format!("twilio status {status}: {body}"),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebPushConfig {
    pub vapid_public_key: String,
    pub vapid_private_key: String,
    pub subject: String,
}

impl WebPushConfig {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Some(Self {
            vapid_public_key: std::env::var("VAPID_PUBLIC_KEY").ok()?,
            vapid_private_key: std::env::var("VAPID_PRIVATE_KEY").ok()?,
            subject: std::env::var("VAPID_SUBJECT").unwrap_or_else(|_| "mailto:owner@example.com".into()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct WebPushSender {
    cfg: WebPushConfig,
}

impl WebPushSender {
    #[must_use]
    pub fn new(cfg: WebPushConfig) -> Self {
        Self { cfg }
    }
}

impl NotificationSender for WebPushSender {
    async fn send(&self, notification: &Notification) -> Result<(), NotifierError> {
        if notification.channel != Channel::Push {
            return Ok(());
        }

        // NOTE: Web push transport is intentionally stubbed in this build to avoid
        // introducing a dependency that cannot be unpacked in the sandboxed build
        // environment. The sender remains behind the `NotificationSender` trait.
        let _ = (&self.cfg, notification);
        Err(NotifierError::DeliveryFailed {
            channel: notification.channel.to_string(),
            reason: "web push sender not available in this build".into(),
        })
    }
}

/// Parameters for dispatching a notification.
#[derive(Debug, Clone)]
pub struct DispatchParams<'a> {
    pub notification_type: NotificationType,
    pub recipient: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    pub entity_id: Option<Uuid>,
    pub quiet_hours: Option<&'a QuietHours>,
    pub current_hour: u8,
}

/// Dispatch a notification across all appropriate channels, respecting quiet
/// hours and channel suppression rules.
pub async fn dispatch_notification<S: NotificationSender>(
    sender: &S,
    params: &DispatchParams<'_>,
) -> Vec<Result<(), NotifierError>> {
    let channels = channels_for_type(params.notification_type);
    let mut results = Vec::with_capacity(channels.len());

    for channel in channels {
        if let Some(qh) = params.quiet_hours {
            if qh.should_suppress(params.notification_type, channel, params.current_hour) {
                continue;
            }
        }

        let mut notif = Notification::new(
            params.notification_type,
            channel,
            params.recipient,
            params.subject,
            params.body,
        );
        notif.entity_id = params.entity_id;

        results.push(sender.send(&notif).await);
    }

    results
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

    #[test]
    fn notification_new_sets_fields() {
        let notif = Notification::new(
            NotificationType::PostFailed,
            Channel::Push,
            "owner@example.com",
            "Post failed",
            "Reply could not be posted.",
        );
        assert_eq!(notif.notification_type, NotificationType::PostFailed);
        assert_eq!(notif.channel, Channel::Push);
        assert_eq!(notif.recipient, "owner@example.com");
        assert!(notif.entity_id.is_none());
    }

    #[test]
    fn sla_escalation_uses_sms() {
        let channels = channels_for_type(NotificationType::SlaEscalation);
        assert!(channels.contains(&Channel::Sms));
    }

    #[test]
    fn post_failed_no_sms() {
        let channels = channels_for_type(NotificationType::PostFailed);
        assert!(!channels.contains(&Channel::Sms));
    }

    #[test]
    fn drift_detected_email_only() {
        let channels = channels_for_type(NotificationType::DriftDetected);
        assert_eq!(channels, vec![Channel::Email]);
    }

    #[test]
    fn sla_breach_channels() {
        let channels = channels_for_type(NotificationType::SlaBreach);
        assert!(channels.contains(&Channel::Email));
        assert!(channels.contains(&Channel::Push));
        assert!(!channels.contains(&Channel::Sms));
    }

    #[test]
    fn push_suppressed_in_quiet() {
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        assert!(qh.should_suppress(NotificationType::DraftReady, Channel::Push, 23));
    }

    #[test]
    fn sms_for_sla_escalation_not_suppressed() {
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        assert!(!qh.should_suppress(NotificationType::SlaEscalation, Channel::Sms, 1));
    }

    #[test]
    fn notifier_error_display() {
        let err = NotifierError::DeliveryFailed {
            channel: "email".into(),
            reason: "timeout".into(),
        };
        assert!(err.to_string().contains("email"));
        assert!(err.to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn dispatch_notification_sends_to_all_channels() {
        let sender = InMemoryNotificationSender::default();
        let params = DispatchParams {
            notification_type: NotificationType::SensitiveReview,
            recipient: "owner@example.com",
            subject: "Urgent review",
            body: "1-star review received",
            entity_id: None,
            quiet_hours: None,
            current_hour: 12,
        };
        let results = dispatch_notification(&sender, &params).await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(sender.sent.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn dispatch_notification_respects_quiet_hours() {
        let sender = InMemoryNotificationSender::default();
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        let params = DispatchParams {
            notification_type: NotificationType::DraftReady,
            recipient: "owner@example.com",
            subject: "Draft ready",
            body: "New draft",
            entity_id: None,
            quiet_hours: Some(&qh),
            current_hour: 23,
        };
        let results = dispatch_notification(&sender, &params).await;
        assert!(results.is_empty());
        assert!(sender.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dispatch_sensitive_sms_during_quiet_hours() {
        let sender = InMemoryNotificationSender::default();
        let qh = QuietHours {
            start_hour: 22,
            end_hour: 8,
        };
        let params = DispatchParams {
            notification_type: NotificationType::SensitiveReview,
            recipient: "owner@example.com",
            subject: "Urgent",
            body: "1-star",
            entity_id: None,
            quiet_hours: Some(&qh),
            current_hour: 23,
        };
        let results = dispatch_notification(&sender, &params).await;
        assert_eq!(results.len(), 1);
        let sent = sender.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].channel, Channel::Sms);
    }
}
