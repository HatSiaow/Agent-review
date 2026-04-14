# 11 — API Design

## Scope

The `api` crate exposes:

1. A server-rendered web UI (Askama + htmx) for the owner/manager.
2. A small JSON API consumed by the UI's interactive bits.
3. Public webhook endpoints for platform push events.
4. Health and metrics endpoints.

There is no public third-party API in v1.

## Conventions

- Base path: `/` for UI, `/api/v1/` for JSON, `/webhooks/` for inbound.
- JSON requests/responses are `application/json; charset=utf-8`.
- Timestamps are ISO-8601 UTC.
- Errors use [Problem Details for HTTP APIs (RFC 9457)](https://www.rfc-editor.org/rfc/rfc9457):

```json
{
  "type": "https://agent-review/errors/invalid-transition",
  "title": "Draft cannot be approved from state 'posted'",
  "status": 409,
  "code": "invalid_transition",
  "instance": "/api/v1/drafts/3f0d.../approve"
}
```

- All mutating endpoints require a `X-CSRF-Token` header (double-submit
  cookie pattern) and a valid session cookie.
- Idempotency: mutating endpoints accept an optional `Idempotency-Key`
  header; the server caches the result for 24h.

## JSON Endpoints

### Reviews

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/reviews` | List reviews; filters: `platform`, `status`, `rating`, `q` |
| GET | `/api/v1/reviews/{id}` | Fetch one review with its drafts |
| POST | `/api/v1/reviews/{id}/skip` | Mark review skipped |
| POST | `/api/v1/reviews/{id}/unskip` | Undo skip |
| POST | `/api/v1/reviews/{id}/regenerate` | Enqueue a new agent run; body: `{ "hint": "..." }` |

### Drafts

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/drafts` | List drafts with filters: `state`, `rating`, `flag` |
| POST | `/api/v1/drafts/{id}/approve` | Approve; body: `{ "text"?: string }` for edit-and-approve |
| POST | `/api/v1/drafts/{id}/reject` | Reject; body: `{ "reason": "too_generic"|..., "note"?: string }` |
| POST | `/api/v1/drafts/bulk-approve` | Body: `{ "ids": [uuid, ...] }`; only allowed for 5★ no-warning drafts |

### Admin / Settings

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/settings` | Get restaurant profile, voice, hours, quiet hours |
| PUT | `/api/v1/settings` | Update |
| GET | `/api/v1/users` | List users |
| POST | `/api/v1/users/invite` | Invite a user |
| DELETE | `/api/v1/users/{id}` | Remove |

### System

| Method | Path | Purpose |
|---|---|---|
| GET | `/healthz` | Liveness |
| GET | `/readyz` | Readiness (DB + secrets reachable) |
| GET | `/metrics` | Prometheus metrics |

## Webhooks

| Method | Path | Purpose |
|---|---|---|
| POST | `/webhooks/ubereats` | UberEats review events (HMAC verified) |

The handler:

1. Reads the raw body (via axum's `Bytes` extractor).
2. Verifies the HMAC signature header.
3. Parses the event.
4. Writes to the ingestion queue.
5. Returns `200` quickly (< 200ms target).

## Request/Response Shapes

### `Review` JSON

```json
{
  "id": "3f0d...",
  "platform": "google",
  "source_review_id": "AbCd...",
  "author": { "display_name": "Maria L.", "avatar_url": null },
  "rating": 4,
  "body_text": "Great pasta but the wait was long.",
  "body_language": "en",
  "created_at": "2026-04-10T18:22:11Z",
  "status": "awaiting_human",
  "context": { "ordered_items": ["Tagliatelle al ragù"] },
  "active_draft": {
    "id": "a12c...",
    "text": "Thanks Maria! We're so glad you enjoyed the tagliatelle...",
    "language": "en",
    "char_count": 142,
    "state": "pending_review",
    "flags": []
  },
  "history": [ /* past drafts, audit entries */ ]
}
```

### Approve request

```http
POST /api/v1/drafts/a12c.../approve
Content-Type: application/json
X-CSRF-Token: ...
Idempotency-Key: 8f3e...

{ "text": "Thanks Maria! ..." }
```

Response:

```json
{ "id": "a12c...", "state": "approved", "posted_at": null }
```

## Rate Limiting

- UI endpoints: 120 req/min per session.
- Webhook endpoint: 60 req/min per source IP (platform IPs are whitelisted
  to a higher tier in config).
- Login: 5 attempts / 15 min (see `09-auth-and-secrets.md`).

## Versioning

- JSON API is `/api/v1/`. Breaking changes go to `/api/v2/`.
- UI has no versioning; it's coupled to the server it ships with.

## OpenAPI

An OpenAPI 3.1 document is generated from Rust types via `utoipa` and served
at `/api/v1/openapi.json` (authenticated).
