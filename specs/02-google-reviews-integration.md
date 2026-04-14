# 02 — Google Reviews Integration

## Purpose

Ingest new customer reviews from the restaurant's Google Business Profile
location and post approved replies back.

## API Used

- **Google Business Profile API** (successor to Google My Business API).
  - `accounts.locations.reviews.list` — list reviews for a location.
  - `accounts.locations.reviews.get` — fetch a single review.
  - `accounts.locations.reviews.updateReply` — create or update a reply.
  - `accounts.locations.reviews.deleteReply` — delete a reply.

> Note: Google has been migrating parts of the old GMB API to the new
> Business Profile Performance API. The review endpoints still live under the
> legacy `mybusiness.googleapis.com` host at the time of writing. The adapter
> must be written so the host and path are configurable via env vars.

## Authentication

- OAuth 2.0 with the `https://www.googleapis.com/auth/business.manage` scope.
- Single restaurant owner account. The refresh token is stored encrypted in
  the secrets store (see `09-auth-and-secrets.md`).
- Access tokens are refreshed lazily on 401 responses and proactively when
  the cached token is within 5 minutes of expiry.

## Ingestion Strategy

Google does not offer a stable push/webhook for reviews, so ingestion uses
polling:

- Poll interval: **every 10 minutes** by default, configurable.
- Each poll calls `reviews.list` with `pageSize=50`, ordered by `updateTime desc`.
- The adapter stops paging once it encounters a review whose `updateTime` is
  older than the last successfully processed `updateTime` for this location.
- A `reviews_sync_state` row tracks `last_seen_update_time` per location.

## Rate Limiting & Backoff

- Default quota: ~10 QPS per project. Polling every 10 minutes is well under.
- On `429` or `5xx`, the adapter uses exponential backoff (2s, 4s, 8s, 16s,
  32s, max 5 minutes) with jitter.
- Permanent failures are surfaced as an `IngestionError` event in the audit
  log and an alert to the notifier.

## Normalization

Map Google review fields to the unified model (see `04-unified-review-data-model.md`):

| Google field                     | Unified field               |
|----------------------------------|-----------------------------|
| `reviewId`                       | `source_review_id`          |
| `reviewer.displayName`           | `author_display_name`       |
| `reviewer.profilePhotoUrl`       | `author_avatar_url`         |
| `starRating` (enum ONE..FIVE)    | `rating` (1..5 integer)     |
| `comment`                        | `body_text`                 |
| `createTime`                     | `created_at`                |
| `updateTime`                     | `updated_at`                |
| `reviewReply.comment`            | `existing_reply_text`       |
| `reviewReply.updateTime`         | `existing_reply_updated_at` |

`platform` is hard-coded to `google`. The raw JSON payload is also persisted
in `raw_payload` for debugging and future schema evolution.

## Posting Replies

- The `poster` worker calls `reviews.updateReply` with the approved text.
- On success, the `ReplyDraft` transitions to `posted` and the post timestamp
  is recorded.
- On `400` (e.g. content policy) the draft is flagged `rejected_by_platform`
  and surfaced in the UI for the owner to rewrite.
- Google imposes a ~4096 character limit on replies; the agent enforces
  1000 characters max as a safer internal limit.

## Edge Cases

- **Review deleted by user.** On next poll, if a tracked review no longer
  appears, mark it `withdrawn` and cancel any pending draft.
- **Reply edited on Google side.** If `existing_reply_updated_at` advances
  without our posting worker touching it, record a `drift_detected` event
  and notify the owner.
- **Rating-only reviews.** Reviews with no `comment` still have a `starRating`.
  Draft a rating-only reply template (see agent spec).

## Configuration

| Env var | Purpose |
|---|---|
| `GOOGLE_OAUTH_CLIENT_ID` | OAuth client id |
| `GOOGLE_OAUTH_CLIENT_SECRET` | OAuth client secret |
| `GOOGLE_OAUTH_REFRESH_TOKEN` | Long-lived refresh token for the owner account |
| `GOOGLE_ACCOUNT_ID` | Google account identifier |
| `GOOGLE_LOCATION_ID` | Location identifier for the restaurant |
| `GOOGLE_REVIEWS_POLL_SECONDS` | Poll interval (default 600) |
