# Acceptance Criteria (Reviewer-Only)

This document consolidates acceptance criteria extracted from the canonical specs
in `specs/coder/`. It’s intended to be used directly while reviewing diffs.

## 01 — Architecture & System Overview

- **Human approval gate**: no reply is posted without an explicit human approval action.
- **Service separation**: adapter failures must not block ingestion from other platforms.
- **Single vs multi-process**: services can run as one binary or separate roles against one Postgres.
- **Auditability**: end-to-end flow is traceable per review (ingest → draft → approve → post).

## 02 — Google Reviews Integration

- **Polling**: default poll is every 10 minutes; configurable.
- **Stop-at-cursor**: pagination stops once encountering a review older than the last processed `updateTime`.
- **Dedup**: upsert/dedup on `(platform, source_review_id)` via unified model rules.
- **Backoff**: retries on `429`/`5xx` use exponential backoff with jitter; permanent failures emit audit + notifier alert.
- **Reply posting**: approved text is posted; `400` flags `rejected_by_platform` and surfaces to UI.
- **Edge cases**:
  - Deleted review → status `withdrawn` and pending draft cancelled.
  - Reply edited externally → `drift_detected` recorded and owner notified.
  - Rating-only review → supported draft path.

## 03 — UberEats Reviews Integration

- **Webhook primary**: `/webhooks/ubereats` verifies HMAC signature, enqueues ingestion, returns `200` quickly.
- **Polling fallback**: runs every 30 minutes since last known `created_at`.
- **Rate limiting**: shared conservative limit (token bucket ~5 rps) and exponential backoff on `429`/`5xx`.
- **DLQ**: after 5 failed attempts, job is parked in a dead-letter queue.
- **Reply posting**: do not post if a reply already exists externally (`drift_detected`).
- **Edge cases**:
  - Non-English reviews → reply language matches `body_language`.
  - Anonymous → safe salutation fallback.
  - 1–2★ with no comment → routed to owner without auto-drafting.

## 04 — Unified Review Data Model

- **Natural key**: `(platform, source_review_id)` is unique and used for ingest dedup.
- **Single active draft**: at most one active draft per review; rejected drafts retained as history.
- **Posted immutability**: posted drafts are immutable.
- **FSM enforcement**: invalid transitions are rejected (compile-time where possible; runtime otherwise).
- **Drift behavior**: newer `updated_at` triggers refresh + drift event if body changes.

## 05 — AI Response Generation Agent

- **Tool boundary**: agent cannot invoke arbitrary HTTP; only deterministic tool interface calls.
- **Guardrails**: enforce refund/liability/PII/profanity/length/language-match/self-reference rules.
- **Retry once on guardrail fail**; on repeated fail, persist with warning visible to human reviewer.
- **Rating behavior**: 5★/4★ auto-draft; 3★ flagged `needs_attention`; 1–2★ escalated and `sensitive`.
- **Sensitive keywords** always escalate regardless of rating.
- **Observability**: every run records model, tool calls, token usage, latency, verdict.

## 06 — Human-in-the-Loop Workflow

- **Queue tabs**: Needs You Now / Ready To Send / History with sorting rules.
- **Actions**: Approve, Edit & Approve (audit diff), Reject (reason captured, regeneration loop), Skip (reversible), Regenerate, Escalate.
- **Bulk approve**: only for 5★ with no warnings; 10-second undo window before posting starts.
- **SLA escalation**: 2h reminder, 24h escalation to SMS, 72h auto-skip with audit entry.
- **RBAC**: owner all actions; manager limited; viewer read-only.

## 07 — Notification System

- **Channel policy**: email default; push optional; SMS escalation only.
- **Batching**: 4–5★ `draft_ready` is hourly digest; sensitive is immediate (never batched).
- **Outbox**: at-least-once delivery with idempotency per (event_id, channel).
- **Quiet hours**: hold batch + suppress push; SMS still allowed for urgent types.
- **Failure handling**: retries with backoff; sustained failures surface in-app banner.

## 08 — Data Storage

- **Postgres**: schema supports unified model + drafts + agent runs + audit + outbox.
- **Migrations**: forward-only; deprecations follow two-release cycle.
- **Retention**: raw payload redacted after 90 days; agent runs retained 180 days; other retention as specified.
- **GC job**: nightly storage GC enforces retention.

## 09 — Authentication & Secrets Management

- **Auth**: Argon2id; cookie sessions server-side with sliding expiry; login rate limit.
- **2FA**: TOTP recommended; required for owner by default (configurable).
- **Secrets**: abstracted `Secrets` trait; never logged; `SecretString` redaction.
- **Webhook signatures**: HMAC validated constant-time.
- **Encryption**: envelope encryption for sensitive DB columns; master key rotation supported.

## 10 — Rust Technical Stack

- **Workspace discipline**: deny `unsafe_code`, clippy/rustfmt standards applied.
- **Observability spans**: every external call is instrumented with relevant fields.
- **No unwraps** outside tests/startup.

## 11 — API Design

- **Base paths**: `/api/v1/` JSON; `/webhooks/` inbound; UI at `/`.
- **Mutations**: CSRF + session required; optional `Idempotency-Key` cached 24h.
- **Errors**: RFC 9457 problem details.
- **Webhook handling**: raw body verify signature, enqueue, respond quickly (<200ms target).

## 12 — Observability & Logging

- **Standard fields**: service, trace ids, review/draft ids, platform, user id.
- **Metrics**: key counters/histograms exposed on `/metrics`.
- **Alerts**: ingestion stalled, error spikes, backlog, SLA breaches route into notifier.
- **Debug view**: per-review trace view shows raw payload, runs, drafts, audit events, posting attempts.

## 13 — Deployment & Infrastructure

- **Default deploy**: single small VM/host, Postgres, reverse proxy (Caddy).
- **CI**: fmt/clippy/tests/deny/sqlx prepare checks run.
- **Migrations**: run as pre-start/init hook; no rollbacks (forward-only).

## 14 — Testing Strategy

See `specs/reviewer/14-testing-strategy.md`.

## 15 — Security, Privacy & Compliance

- **Posting safety**: no automated unattended replies; posting requires human approval.
- **Webhook replay protection**: dedupe events within 24h.
- **Input safety**: review bodies treated as untrusted; UI escapes by default; logs/traces escape.
- **PII minimisation**: raw payload retention & redaction; LLM requests omit unnecessary customer data.
- **Security checks**: `cargo deny`/advisories in CI; security review for sensitive areas.
