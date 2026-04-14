# 04 — Unified Review Data Model

## Purpose

Define a single canonical representation of a customer review that abstracts
over Google and UberEats (and future platforms). All downstream services — the
agent, the UI, storage, notifications — only know about the unified model.

## Core Entities

### `Review`

| Field | Type | Notes |
|---|---|---|
| `id` | `Uuid` | Internal primary key |
| `platform` | `Platform` enum | `Google` or `UberEats` |
| `source_review_id` | `String` | Unique id on the source platform |
| `source_location_id` | `String` | Restaurant location on source |
| `author_display_name` | `String` | May be anonymised on some platforms |
| `author_avatar_url` | `Option<Url>` | |
| `rating` | `u8` | 1..=5 |
| `body_text` | `Option<String>` | `None` for rating-only reviews |
| `body_language` | `Option<String>` | BCP-47 tag, e.g. `en`, `fr`, `pt-BR` |
| `created_at` | `DateTime<Utc>` | When the review was created on source |
| `updated_at` | `DateTime<Utc>` | Last edit timestamp on source |
| `ingested_at` | `DateTime<Utc>` | When our system first saw it |
| `existing_reply_text` | `Option<String>` | Present if the source already has a reply |
| `existing_reply_updated_at` | `Option<DateTime<Utc>>` | |
| `status` | `ReviewStatus` enum | `new`, `drafting`, `awaiting_human`, `replied`, `withdrawn`, `skipped` |
| `context_json` | `JsonValue` | Platform-specific extras (e.g. ordered items) |
| `raw_payload` | `JsonValue` | Verbatim source payload |

### `ReplyDraft`

| Field | Type | Notes |
|---|---|---|
| `id` | `Uuid` | |
| `review_id` | `Uuid` | FK to `Review` |
| `generated_by` | `Generator` enum | `agent_llm`, `human_edit`, `template` |
| `model_name` | `Option<String>` | e.g. `claude-sonnet-4-6` |
| `prompt_fingerprint` | `Option<String>` | sha256 of prompt for reproducibility |
| `text` | `String` | The proposed reply |
| `language` | `String` | BCP-47 |
| `char_count` | `u32` | Cached |
| `state` | `DraftState` enum | `pending_review`, `approved`, `edited`, `rejected`, `posted`, `failed` |
| `created_at` | `DateTime<Utc>` | |
| `reviewed_by` | `Option<UserId>` | Who approved/rejected |
| `reviewed_at` | `Option<DateTime<Utc>>` | |
| `rejection_reason` | `Option<String>` | |
| `posted_at` | `Option<DateTime<Utc>>` | |
| `platform_post_error` | `Option<String>` | |

### `AuditEvent`

An append-only log of every state transition and external call:

```
id | occurred_at | actor | entity_type | entity_id | event_type | details_json
```

Used for the owner's review history and for debugging.

## Enumerations

```rust
pub enum Platform { Google, UberEats }

pub enum ReviewStatus {
    New,
    Drafting,
    AwaitingHuman,
    Replied,
    Withdrawn,
    Skipped,
}

pub enum DraftState {
    PendingReview,
    Approved,
    Edited,
    Rejected,
    Posted,
    Failed,
}

pub enum Generator { AgentLlm, HumanEdit, Template }
```

## Invariants

- A `Review` has at most one *active* `ReplyDraft` at a time (i.e. a draft in
  state `pending_review`, `approved`, or `edited`). Rejected drafts are kept
  as history.
- `source_review_id` is unique per `platform`. The pair
  `(platform, source_review_id)` is the natural key used for dedup on ingest.
- A draft in state `posted` is immutable.
- Transitions are enforced in Rust via a typed state machine (`ReviewFsm`) to
  prevent invalid transitions at compile time where possible.

## Identity & Dedup

Ingestion upserts on `(platform, source_review_id)`. If a row already exists
and `updated_at` is newer than the stored value, the review is re-processed:
the `raw_payload` is refreshed and, if the `body_text` changed, a drift event
is emitted.

## Versioning

The unified model is versioned via a `model_version` column on each row
(default `1`). Migration strategy is documented in `08-data-storage.md`.
