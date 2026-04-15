-- Durable work queue for internal workers.
--
-- See: specs/coder/17-work-queues-and-outbox-processing.md

create table if not exists work_jobs (
  id uuid primary key,

  job_type text not null,
  dedupe_key text not null,
  payload_json jsonb not null,

  state text not null, -- pending|running|succeeded|failed|dead_letter
  attempts integer not null default 0,
  max_attempts integer not null default 5,

  run_after timestamptz not null default now(),

  locked_by text null,
  locked_at timestamptz null,

  last_error text null,

  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create unique index if not exists work_jobs_type_dedupe_uq
  on work_jobs (job_type, dedupe_key);

create index if not exists work_jobs_claim_idx
  on work_jobs (job_type, state, run_after);

create index if not exists work_jobs_run_after_idx
  on work_jobs (run_after);

