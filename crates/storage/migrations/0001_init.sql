-- Initial schema (v0.1), based on SPECS.

create table if not exists reviews (
  id uuid primary key,
  platform text not null,
  source_review_id text not null,
  source_location_id text not null,
  author_display_name text not null,
  author_avatar_url text null,
  rating smallint not null,
  body_text text null,
  body_language text null,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  ingested_at timestamptz not null,
  existing_reply_text text null,
  existing_reply_updated_at timestamptz null,
  status text not null,
  context_json jsonb not null,
  raw_payload jsonb not null
);

create unique index if not exists reviews_platform_source_review_id_uq
  on reviews (platform, source_review_id);

create index if not exists reviews_status_idx on reviews (status);
create index if not exists reviews_updated_at_idx on reviews (updated_at desc);

create table if not exists reply_drafts (
  id uuid primary key,
  review_id uuid not null references reviews(id) on delete cascade,
  generated_by text not null,
  model_name text null,
  prompt_fingerprint text null,
  text text not null,
  language text not null,
  char_count integer not null,
  state text not null,
  guardrail_warnings jsonb not null,
  flags jsonb not null,
  created_at timestamptz not null,
  reviewed_by uuid null,
  reviewed_at timestamptz null,
  rejection_reason text null,
  posted_at timestamptz null,
  platform_post_error text null
);

create index if not exists reply_drafts_review_id_idx on reply_drafts (review_id);
create index if not exists reply_drafts_state_idx on reply_drafts (state);

create table if not exists audit_events (
  id uuid primary key,
  occurred_at timestamptz not null,
  actor_type text not null,
  actor_id uuid null,
  entity_type text not null,
  entity_id uuid not null,
  event_type text not null,
  details_json jsonb not null
);

create index if not exists audit_events_entity_idx on audit_events (entity_type, entity_id);
create index if not exists audit_events_occurred_at_idx on audit_events (occurred_at desc);

create table if not exists reviews_sync_state (
  platform text primary key,
  last_seen_update_time timestamptz null
);

create table if not exists notifications_outbox (
  id uuid primary key,
  occurred_at timestamptz not null,
  notification_type text not null,
  review_id uuid null,
  draft_id uuid null,
  payload_json jsonb not null,
  sent_at timestamptz null
);

create index if not exists notifications_outbox_sent_at_idx on notifications_outbox (sent_at);

create table if not exists push_subscriptions (
  id uuid primary key,
  user_id uuid not null,
  endpoint text not null,
  p256dh text not null,
  auth text not null,
  created_at timestamptz not null
);

create unique index if not exists push_subscriptions_user_endpoint_uq
  on push_subscriptions(user_id, endpoint);

create table if not exists users (
  id uuid primary key,
  email text not null unique,
  password_hash text not null,
  role text not null,
  totp_secret text null,
  created_at timestamptz not null
);

create table if not exists sessions (
  id uuid primary key,
  user_id uuid not null references users(id) on delete cascade,
  created_at timestamptz not null,
  expires_at timestamptz not null
);

create index if not exists sessions_user_id_idx on sessions (user_id);
create index if not exists sessions_expires_at_idx on sessions (expires_at);

create table if not exists agent_runs (
  id uuid primary key,
  review_id uuid not null references reviews(id) on delete cascade,
  started_at timestamptz not null,
  finished_at timestamptz null,
  model_name text null,
  prompt_fingerprint text null,
  tool_calls integer not null,
  status text not null,
  error_text text null
);

create index if not exists agent_runs_review_id_idx on agent_runs (review_id);
