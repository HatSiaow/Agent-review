# 08 — Data Storage

## Database

PostgreSQL 15+. Chosen because:

- Strong transactional guarantees for state machines.
- `jsonb` columns for `raw_payload` and `context_json`.
- Mature Rust ecosystem (`sqlx`, `refinery`, `sea-query`).

A single database is sufficient for the expected scale of one restaurant
(dozens of reviews per week).

## Schema Overview

```sql
-- users & auth
create table users (
    id              uuid primary key,
    email           citext unique not null,
    display_name    text not null,
    role            text not null check (role in ('owner','manager','viewer')),
    password_hash   text,
    created_at      timestamptz not null default now()
);

-- reviews
create table reviews (
    id                         uuid primary key,
    platform                   text not null check (platform in ('google','ubereats')),
    source_review_id           text not null,
    source_location_id         text not null,
    author_display_name        text not null,
    author_avatar_url          text,
    rating                     smallint not null check (rating between 1 and 5),
    body_text                  text,
    body_language              text,
    created_at                 timestamptz not null,
    updated_at                 timestamptz not null,
    ingested_at                timestamptz not null default now(),
    existing_reply_text        text,
    existing_reply_updated_at  timestamptz,
    status                     text not null,
    context_json               jsonb not null default '{}'::jsonb,
    raw_payload                jsonb not null,
    model_version              int not null default 1,
    unique (platform, source_review_id)
);
create index reviews_status_created_idx on reviews (status, created_at desc);

-- reply drafts
create table reply_drafts (
    id                    uuid primary key,
    review_id             uuid not null references reviews(id) on delete cascade,
    generated_by          text not null,
    model_name            text,
    prompt_fingerprint    text,
    text                  text not null,
    language              text not null,
    char_count            int not null,
    state                 text not null,
    guardrail_warnings    jsonb not null default '[]'::jsonb,
    flags                 jsonb not null default '[]'::jsonb,
    created_at            timestamptz not null default now(),
    reviewed_by           uuid references users(id),
    reviewed_at           timestamptz,
    rejection_reason      text,
    posted_at             timestamptz,
    platform_post_error   text
);
create index reply_drafts_state_idx on reply_drafts (state, created_at);
create index reply_drafts_review_idx on reply_drafts (review_id);

-- agent runs
create table agent_runs (
    id                    uuid primary key,
    review_id             uuid not null references reviews(id),
    draft_id              uuid references reply_drafts(id),
    model_name            text not null,
    prompt_fingerprint    text not null,
    prompt_tokens         int,
    completion_tokens     int,
    latency_ms            int,
    tool_calls            jsonb not null default '[]'::jsonb,
    guardrail_verdict     jsonb,
    error                 text,
    created_at            timestamptz not null default now()
);

-- sync state per adapter
create table reviews_sync_state (
    platform               text primary key,
    last_seen_update_time  timestamptz,
    last_run_at            timestamptz,
    last_error             text
);

-- audit log
create table audit_events (
    id            bigserial primary key,
    occurred_at   timestamptz not null default now(),
    actor_type    text not null,       -- 'user','system','agent'
    actor_id      text,
    entity_type   text not null,
    entity_id     uuid not null,
    event_type    text not null,
    details_json  jsonb not null default '{}'::jsonb
);
create index audit_events_entity_idx on audit_events (entity_type, entity_id, occurred_at desc);

-- notifications outbox
create table notifications_outbox (
    id              uuid primary key,
    event_type      text not null,
    channel         text not null,
    recipient       text not null,
    payload         jsonb not null,
    state           text not null,    -- 'pending','sent','failed'
    attempts        int not null default 0,
    scheduled_for   timestamptz not null default now(),
    sent_at         timestamptz,
    last_error      text
);
create index notifications_outbox_state_idx on notifications_outbox (state, scheduled_for);

-- push subscriptions
create table push_subscriptions (
    id          uuid primary key,
    user_id     uuid not null references users(id) on delete cascade,
    endpoint    text not null,
    p256dh      text not null,
    auth        text not null,
    created_at  timestamptz not null default now()
);
```

## Migrations

- Managed with `refinery` (embedded SQL migrations in
  `crates/storage/migrations/`).
- All migrations are forward-only. Schema changes that drop columns must go
  through a two-release deprecation cycle.
- Migration commands: `cargo run -p cli -- migrate up` /
  `migrate status`.

## Retention

| Table | Retention |
|---|---|
| `reviews` | Indefinite (owner may want to look up historic reviews) |
| `reply_drafts` | Indefinite (audit trail) |
| `agent_runs` | 180 days (costs / privacy), prompts truncated after 30 days |
| `audit_events` | 2 years |
| `notifications_outbox` | 30 days post-send |
| `raw_payload` on reviews | 90 days, then redacted (kept as `{}`::jsonb) |

A nightly `storage_gc` job enforces retention.

## Backups

- Daily logical backups (`pg_dump`) to encrypted object storage, 30-day
  retention.
- Weekly full + continuous WAL archiving for point-in-time recovery.

## Connection Pooling

Single `sqlx::PgPool` shared across services, configured via
`DATABASE_URL`. Default pool size 10.
