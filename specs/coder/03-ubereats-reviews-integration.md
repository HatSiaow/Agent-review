# 03 — UberEats Reviews Integration

## Purpose

Ingest customer reviews from UberEats (sometimes called "order feedback" or
"eater feedback") for the restaurant's store, and post approved replies back.

## API Used

- **Uber Eats Merchant APIs** (`api.uber.com/v1/eats/...`).
  - `GET /v1/eats/stores/{store_id}/reviews` — list reviews.
  - `POST /v1/eats/stores/{store_id}/reviews/{review_id}/reply` — post a reply.
  - Store webhooks (`event_type=store.review_created`) for push notifications
    where available.

> UberEats' review/reply endpoints evolve. The adapter must isolate the exact
> endpoint paths behind a thin client trait so they can be updated without
> rippling through the codebase.

## Authentication

- OAuth 2.0 client-credentials flow for the Merchant API with scopes:
  - `eats.store` (store metadata)
  - `eats.store.reviews.read`
  - `eats.store.reviews.write`
- Access tokens are short-lived (≈30 days); refreshed automatically by the
  adapter when within 1 day of expiry.

## Ingestion Strategy

Hybrid:

1. **Webhook** — primary path. The `api` service exposes
   `POST /webhooks/ubereats` which UberEats calls on new reviews. The handler
   verifies the HMAC signature (`x-uber-signature`), enqueues an ingestion job,
   and returns `200` quickly.
2. **Polling fallback** — every 30 minutes the adapter lists reviews since
   the last known `created_at`. This catches any webhooks that were dropped.

## Rate Limiting & Backoff

- UberEats publishes per-endpoint limits; the adapter uses a conservative
  shared token bucket of 5 requests/second.
- Retries on `429` and `5xx` follow exponential backoff (1s, 2s, 4s, 8s, 16s).
- After 5 failed attempts the job is parked in a dead-letter queue.

## Normalization

| UberEats field                 | Unified field           |
|--------------------------------|-------------------------|
| `review_uuid`                  | `source_review_id`      |
| `eater.first_name` + initial   | `author_display_name`   |
| `rating.overall` (1..5)        | `rating`                |
| `comment.text`                 | `body_text`             |
| `comment.language`             | `body_language`         |
| `created_at`                   | `created_at`            |
| `order_uuid`                   | `context.order_id`      |
| `items[].name`                 | `context.ordered_items` |
| `store_uuid`                   | `source_location_id`    |

`platform` is hard-coded to `ubereats`. The `context` object is stashed in
the unified model's `context_json` column so the agent can personalise
replies (e.g. "thanks for ordering the lamb biryani").

## Posting Replies

- Approved replies are sent via the reply endpoint.
- UberEats imposes a 500-character limit; the agent must produce shorter
  drafts for this platform.
- If the review has already been replied to by a staff member directly on
  UberEats, the adapter treats it as a `drift_detected` event and will not
  post a second reply.

## Edge Cases

- **Non-English reviews.** The `body_language` field feeds the agent's
  language selection so replies are returned in the same language.
- **Anonymous reviews.** If `eater.first_name` is missing, fall back to
  `"there"` as a salutation (agent handles this).
- **Very low ratings (1–2 stars) with no comment.** Route directly to the
  owner without auto-drafting, as these usually need personal handling.

## Configuration

| Env var | Purpose |
|---|---|
| `UBEREATS_CLIENT_ID` | OAuth client id |
| `UBEREATS_CLIENT_SECRET` | OAuth client secret |
| `UBEREATS_STORE_ID` | Store (location) uuid |
| `UBEREATS_WEBHOOK_SECRET` | HMAC secret for signature validation |
| `UBEREATS_POLL_SECONDS` | Fallback poll interval (default 1800) |
