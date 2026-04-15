# notifier

Purpose: Owner notifications across email, SMS, and push stubs: channel selection per `NotificationType`, quiet-hours rules, dispatch helpers, and concrete senders (SMTP, Twilio SMS, in-memory test double).

Key entry points: `dispatch_notification`, `channels_for_type`, `QuietHours`, `Notification`, `NotificationSender`, `SmtpSender`, `TwilioSmsSender`, `InMemoryNotificationSender` in `src/lib.rs`.

Why tests matter: Wrong routing or suppression can hide urgent bad reviews or spam owners overnight. Channel matrix and quiet-hour edge cases must stay aligned with SLA and safety expectations for human-in-the-loop review.

Local tests:

```
cargo test -p notifier
```
