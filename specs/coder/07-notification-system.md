# 07 — Notification System

## Purpose

Tell the owner (and, where configured, managers) when reviews need their
attention, without becoming a source of noise.

## Channels

| Channel | Use |
|---|---|
| **Email** | Default for all notifications. Uses SMTP via `lettre`. |
| **Web Push** | For users who keep the browser UI open. VAPID-signed, stored in `push_subscriptions`. |
| **SMS** | Escalation only (1–2★ unreviewed, or system outages). Uses a pluggable provider trait with a Twilio implementation. |

Channels are additive: the owner may enable any combination.

## Notification Types

| Type | Trigger | Default channels |
|---|---|---|
| `draft_ready` | A new draft enters `pending_review` | Email (digest), Push |
| `sensitive_review` | 1–2★ review or sensitive keyword | Email (immediate), Push, SMS |
| `sla_breach` | Draft > 2h in Needs You Now | Email, Push |
| `sla_escalation` | Draft > 24h in Needs You Now | Email, Push, SMS |
| `ingestion_failure` | Adapter error persists > 30 min | Email |
| `post_failed` | Posting to platform failed after retries | Email, Push |
| `drift_detected` | Reply was edited/added outside the app | Email |

## Batching

- `draft_ready` for 4–5★ reviews is batched into a single **hourly digest**
  email so the owner is not pinged on every thank-you.
- `sensitive_review` is **never** batched and is sent immediately.
- Push notifications collapse per-review (new drafts replace older pending
  notifications for the same review).

## Delivery Pipeline

```
event_bus -> notifier worker -> renderer -> channel adapters
                             \-> idempotency store (event_id + channel)
```

- Events are persisted in an outbox table before dispatch to guarantee
  at-least-once delivery.
- The idempotency store prevents duplicate sends if the worker crashes
  mid-batch.

## Templates

Templates are plain Tera templates under `templates/notifications/`:

- `draft_ready_digest.html` / `.txt`
- `sensitive_review.html` / `.txt`
- `sla_breach.html` / `.txt`
- `ingestion_failure.txt`

Each email has a deep link to the specific item in the web UI.

## Quiet Hours

The owner can configure quiet hours (e.g. 22:00–08:00 local time). During
quiet hours:

- Batched emails are held and delivered at the end of the window.
- Push is suppressed.
- SMS is **still** sent for `sla_escalation` and `sensitive_review` — these
  are explicitly urgent.

## Configuration

| Env var | Purpose |
|---|---|
| `SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASS` | SMTP credentials |
| `SMTP_FROM` | From address |
| `TWILIO_ACCOUNT_SID`, `TWILIO_AUTH_TOKEN`, `TWILIO_FROM` | SMS |
| `VAPID_PUBLIC_KEY`, `VAPID_PRIVATE_KEY` | Web push signing |
| `NOTIFIER_DIGEST_INTERVAL_SECONDS` | Digest cadence (default 3600) |
| `NOTIFIER_QUIET_HOURS` | e.g. `22:00-08:00` |

## Failure Handling

- SMTP / SMS failures are retried 5 times with exponential backoff.
- Sustained failures surface as an in-app banner: "We can't reach your
  email right now." so the owner is never left unaware of a silent channel.
