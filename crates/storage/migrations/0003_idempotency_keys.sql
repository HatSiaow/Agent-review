-- Idempotency-Key store for mutating API requests.
--
-- Stores a canonical response payload for a key for 24h (GC job can prune older rows).

create table if not exists idempotency_responses (
  idempotency_key text primary key,
  status smallint not null,
  body_json jsonb not null,
  created_at timestamptz not null
);

create index if not exists idempotency_responses_created_at_idx
  on idempotency_responses (created_at desc);

