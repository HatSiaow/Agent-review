# 17 — Work Queues & Outbox Processing

## Purpose

The current codebase contains “demo” polling loops that periodically scan the
database/store for work. For production reliability (no missed work, safe
retries, crash recovery), the system needs **durable work queues** and a
standard way to claim, process, and retry jobs.

This spec defines:

- How ingestion enqueues drafting work for the agent.
- How approvals enqueue posting work for the poster.
- How events enqueue notification work in the notifier outbox.
- Idempotency and retry behavior for each worker.

This spec is **internal**: it does not define any public API.

## Design Principles

- **At-least-once processing with idempotency**: workers may retry; side effects
  (posting replies, sending notifications) must not duplicate.
- **Crash-safe**: no work is “lost” if a process restarts.
- **Simple ops**: PostgreSQL-backed queues/outboxes are sufficient for v1.
- **Observable**: every job transition is auditable and emits metrics.

## Work Types

### 1) Drafting jobs (agent)

Triggered when:

- A new review is ingested and needs a draft.
- A user clicks **Regenerate**.
- A draft is **Rejected** and is eligible for rerun (up to 2 times; see `06`).

### 2) Posting jobs (poster)

Triggered when:

- A draft is approved (or edited+approved) by an authorized user.
- Bulk approve completes **after** its undo window closes.

### 3) Notification outbox events (notifier)

Triggered on:

- Draft enters `pending_review` (digest vs immediate depends on type/rating).
- SLA timers (2h breach, 24h escalation).
- Ingestion failures, post failures, drift detection, alerts.

## Tables

This spec uses Postgres as the durable mechanism.

### `work_jobs`

Generic queue table that supports multiple worker roles.

Required columns:

- `id uuid primary key`
- `job_type text not null`:
  - `agent_draft_review`
  - `poster_post_reply`
  - `notifier_dispatch`
- `dedupe_key text not null` (idempotency for job creation; unique per type)
- `payload_json jsonb not null`
- `state text not null` in:
  - `pending`
  - `running`
  - `succeeded`
  - `failed`
  - `dead_letter`
- `attempts int not null default 0`
- `max_attempts int not null default 5`
- `run_after timestamptz not null default now()`
- `locked_by text` (worker id)
- `locked_at timestamptz`
- `last_error text`
- `created_at timestamptz not null default now()`
- `updated_at timestamptz not null default now()`

Indexes:

- `(job_type, state, run_after)`
- `(job_type, dedupe_key)` unique

Notes:

- For “digest” behavior, enqueue a single `notifier_dispatch` job keyed by
  time bucket, not per-review.

### Relationship to existing tables

- `agent_runs` remains the authoritative log of each agent execution.
- `notifications_outbox` is still used as the outbox persistence format
  described in `07` / `08`, but dispatch is driven by `work_jobs` claiming
  `notifier_dispatch` jobs that then read/claim outbox rows.

## Claiming Protocol (Workers)

Each worker loop:

1. Select up to \(N\) jobs:
   - `where job_type = $type and state = 'pending' and run_after <= now()`
   - order by `run_after asc`
   - `for update skip locked`
2. Transition to `running` and set `locked_by/locked_at`.
3. Perform work.
4. On success:
   - mark `succeeded`
5. On failure:
   - increment `attempts`
   - if `attempts < max_attempts`: set `state='pending'` and compute
     exponential backoff for `run_after`
   - else: set `state='dead_letter'`

Workers must use a **single transaction** for claim, and must refresh lock if
the job could run longer than a lock timeout (v1 can keep jobs short and rely
on quick retries).

## Idempotency Rules

### Job creation

- Each job type must have a deterministic `dedupe_key` so enqueue is safe:
  - Drafting: `agent_draft_review:{review_id}:{review_updated_at}`
  - Posting: `poster_post_reply:{draft_id}`
  - Notifier dispatch: `notifier_dispatch:{bucket}:{channel?}`

### Side effects

- Poster must be idempotent by `(platform, source_review_id)` + “reply already
  exists” drift detection. If a reply already exists (by us or outside), emit
  `drift_detected` and stop retrying.
- Notifier must be idempotent by `(event_id, channel)` as described in `07`.

## Bulk Approve Undo Window

Bulk approve must create a backend-represented delay:

- When bulk approving, drafts transition to a state that is **not eligible for
  posting yet** (e.g. `approved_pending_undo`), with `post_eligible_at=now()+10s`,
  or enqueue posting jobs with `run_after=now()+10s`.
- Undo removes/cancels those jobs or reverts the draft state before jobs run.

The poster worker must only post when the job is claimed and eligible.

## Metrics & Audit

Workers emit:

- `work_jobs_claimed_total{job_type}`
- `work_jobs_succeeded_total{job_type}`
- `work_jobs_failed_total{job_type,kind}`
- `work_job_latency_seconds{job_type}`

Each job state transition emits an `AuditEvent` on the relevant entity:

- Drafting jobs: entity `review` and `draft`
- Posting jobs: entity `draft`
- Notifier jobs: entity depends on notification type

## Open Questions (v1 choices)

- Whether to use `work_jobs` alone or keep separate per-domain outboxes (v1
  preference: one `work_jobs` table plus a `notifications_outbox` for templated
  message persistence).
- Lock timeout strategy: simplest is “keep job execution under a short bound”
  and rely on `attempts` + retries.

