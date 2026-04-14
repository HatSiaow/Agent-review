-- Align `agent_runs` towards the spec shape (without dropping existing columns).
--
-- Why: we persist per-run observability (prompt fingerprint, tokens, latency, tool calls, verdict)
-- so that drafts are reproducible and the human approval workflow is auditable.

alter table agent_runs
  add column if not exists draft_id uuid null references reply_drafts(id) on delete set null;

alter table agent_runs
  add column if not exists prompt_tokens integer null;

alter table agent_runs
  add column if not exists completion_tokens integer null;

alter table agent_runs
  add column if not exists latency_ms integer null;

alter table agent_runs
  add column if not exists tool_calls_json jsonb not null default '[]'::jsonb;

alter table agent_runs
  add column if not exists guardrail_verdict_json jsonb null;

alter table agent_runs
  add column if not exists error text null;

alter table agent_runs
  add column if not exists created_at timestamptz not null default now();

create index if not exists agent_runs_created_at_idx on agent_runs (created_at desc);
