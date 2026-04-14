## Neighbourhood Restaurant Review Agent — Fix Plan

Bullet list of **spec gaps not yet implemented**, sorted by priority (P0 highest). File paths/specs are referenced so each item is actionable.

- **P0 — Secure, correct human-in-the-loop posting (must-have before “real” use)**
  - **Implement real auth + sessions + CSRF + RBAC (owner/manager/viewer)** per `specs/coder/09-auth-and-secrets.md`, `specs/coder/06-human-in-the-loop-workflow.md`, `specs/coder/11-api-design.md`, `specs/coder/16-frontend-web-ui.md`.
    - Current gaps: no login/session middleware, no CSRF, no RBAC checks; mutation endpoints accept optional `user_id` in body.
    - Code: `crates/api/src/lib.rs`, `crates/api/src/v1.rs`, `crates/storage/src/{pg.rs,memory.rs}`, `crates/storage/migrations/0001_init.sql`.
  - **Idempotency for mutating endpoints** (`Idempotency-Key` caching 24h) + webhook replay protection per `specs/coder/11-api-design.md` and `specs/coder/15-security-privacy-compliance.md`.
    - Code: `crates/api/src/v1.rs`, (new storage table or reuse outbox/idempotency store).
  - **Posting must be gated on explicit human approval, with correct state transitions and auditing** per `specs/coder/06-human-in-the-loop-workflow.md`.
    - Add missing transitions/guards in `domain` FSMs and enforce server-side (not just UI).
    - Code: `crates/domain/src/{fsm.rs,review_fsm.rs,audit.rs}`, `crates/api/src/v1.rs`, `crates/poster/src/lib.rs`, `crates/server/src/main.rs`.
  - **Bulk approve 10-second undo window** (backend-represented; posting must not begin until window closes) per `specs/coder/06-human-in-the-loop-workflow.md` and `specs/coder/16-frontend-web-ui.md`.
    - Current: `server` posts immediately for `Approved` drafts.
    - Code: `crates/server/src/main.rs`, `crates/domain/src/fsm.rs`, `crates/api/src/v1.rs`, `crates/storage/src/{pg.rs,memory.rs}`.
  - **Fix Problem Details to be RFC 9457-aligned** (content-type `application/problem+json`, proper `instance`, consistent mapping of validation errors) per `specs/coder/11-api-design.md`.
    - Code: `crates/api/src/problem.rs`, `crates/api/src/v1.rs`.

- **P0 — Data/storage correctness (otherwise the inbox will lose/duplicate work)**
  - **Make storage schema + repo behavior match the specs** (`model_version`, defaults, indexes, constraints, proper tables) per `specs/coder/08-data-storage.md` and `specs/coder/04-unified-review-data-model.md`.
    - Biggest mismatches: `agent_runs`, `notifications_outbox`, `reviews_sync_state`, `users` columns/constraints, missing defaults/indexes.
    - Code: `crates/storage/migrations/0001_init.sql`, `crates/storage/src/pg.rs`.
  - **Implement real migrations runner** (spec says `refinery`; current `PgRepository::migrate()` is stubbed) per `specs/coder/08-data-storage.md`, `specs/coder/13-deployment-infra.md`.
    - Code: `crates/storage/src/pg.rs`, `crates/cli/src/main.rs`.
  - **Implement spec-compliant upsert/dedup on `(platform, source_review_id)` with update semantics** (refresh raw payload, reprocess on newer `updated_at`, emit drift events when content changes) per `specs/coder/04-unified-review-data-model.md`.
    - Current: both memory+pg paths effectively “do nothing on conflict”.
    - Code: `crates/storage/src/{pg.rs,memory.rs}`, `crates/ingestion/src/lib.rs`.
  - **Retention / `storage_gc` job** (raw_payload redaction after 90d; agent_runs 180d; etc.) per `specs/coder/08-data-storage.md` and `specs/coder/15-security-privacy-compliance.md`.
    - Code: new worker/binary path (likely `crates/server` role + `crates/storage` SQL helpers).

- **P0 — Web UI (the “single inbox” is not usable without it)**
  - **Implement server-rendered UI (Askama + htmx) routes and templates** per `specs/coder/16-frontend-web-ui.md` and `specs/coder/11-api-design.md`.
    - Missing entirely: no `web_ui` crate, no templates, no HTML routes (`GET /`, `GET /reviews/{id}`, `GET /settings`, `GET /users`, auth pages).
    - Likely code: add `crates/web_ui` (per `specs/coder/10-rust-tech-stack.md`) or implement inside `crates/api`, plus templates.
  - **Queue semantics in UI** (Needs You Now / Ready To Send / History tabs, filters, sorting, card constraints) per `specs/coder/06-human-in-the-loop-workflow.md` and `specs/coder/16-frontend-web-ui.md`.
    - Current: JSON list endpoints exist but do not implement filters/sorting/tab semantics.
    - Code: `crates/api/src/v1.rs`, storage query methods.

- **P1 — Reliable workflow orchestration (ingestion → agent → approve → poster)**
  - **Replace demo “scan loops” with durable job/outbox processing** per `specs/coder/01-architecture.md`, `specs/coder/07-notification-system.md`, `specs/coder/13-deployment-infra.md`.
    - Current `crates/server/src/main.rs` polls `list_reviews/list_drafts` every 2s and uses in-memory de-dupe; no durable queues/outbox claims.
    - Code: `crates/server/src/main.rs`, `crates/storage`, plus new queue/outbox abstractions.
  - **Ingestion orchestration** (Google polling with stop-at-watermark; UberEats webhook enqueues + polling fallback “since last created_at”; per-location sync state) per `specs/coder/02-google-reviews-integration.md`, `specs/coder/03-ubereats-reviews-integration.md`.
    - Current `crates/ingestion` is mostly in-memory logic; adapters exist but watermark + per-location sync are missing.
    - Code: `crates/ingestion/src/lib.rs`, `crates/adapters/{google,ubereats}/src/lib.rs`, `crates/api/src/webhooks.rs`, `crates/storage/src/{pg.rs,repo.rs}`.
  - **Poster worker correctness** (idempotent posting, drift detection, retries persisted, surface platform rejections) per `specs/coder/01-architecture.md` and platform specs (`02`, `03`).
    - Current: retries exist but no durable job records; drift detection is largely missing.
    - Code: `crates/poster/src/lib.rs`, `crates/server/src/main.rs`, adapters + storage.

- **P1 — Notifications (owner should actually get notified, without noise)**
  - **Implement notifier outbox pipeline with idempotency store** per `specs/coder/07-notification-system.md`.
    - Current: server sends “DraftReady” once per draft id via in-memory sender; outbox schema doesn’t match spec and is unused.
    - Code: `crates/notifier/src/lib.rs`, `crates/storage/migrations/0001_init.sql`, `crates/server/src/main.rs`.
  - **Digest batching + quiet hours** per `specs/coder/07-notification-system.md`.
  - **SLA timers** (2h breach, 24h escalation, 72h auto-skip) per `specs/coder/06-human-in-the-loop-workflow.md`.

- **P2 — Agent quality + traceability**
  - **Implement `agent_runs` persistence** with the spec fields (tokens, latency, tool calls JSON, guardrail verdict, error) per `specs/coder/05-ai-response-agent.md` and `specs/coder/08-data-storage.md`.
    - Current: `agent_runs` table shape diverges from spec and is not written by code.
    - Code: `crates/agent/src/lib.rs`, `crates/storage/migrations/0001_init.sql`, `crates/storage/src/pg.rs`.
  - **Compute and store `prompt_fingerprint`** (and version prompts) per `specs/coder/05-ai-response-agent.md`.
    - Current: column exists but not populated by the agent.
    - Code: `crates/agent/src/lib.rs`, `crates/domain/src/model.rs`, `crates/storage/src/pg.rs`.
  - **Tooling model: real tool-use protocol (JSON args/results + result hashes)** per `specs/coder/05-ai-response-agent.md`.
    - Current: “tools” are deterministic string context builders, not LLM-invoked tool calls.
    - Code: `crates/agent/src/lib.rs`, plus new tool interface crate/module.
  - **Guardrail rule alignment** (refund promise allowlist/policy patterns, supported language config, restaurant-name self-reference alternative) per `specs/coder/05-ai-response-agent.md`.
    - Code: `crates/domain/src/guardrails.rs`.

- **P2 — Spec-complete platform behaviors**
  - **Google adapter watermark + stop paging** + configurable host/path, jittered backoff up to 5 minutes, drift detection/withdrawn handling per `specs/coder/02-google-reviews-integration.md`.
    - Code: `crates/adapters/google/src/lib.rs`, `crates/adapters/google/src/normalize.rs`.
  - **UberEats polling “since”** watermark + DLQ behavior + drift detection (don’t double-reply) per `specs/coder/03-ubereats-reviews-integration.md`.
    - Code: `crates/adapters/ubereats/src/lib.rs`, `crates/adapters/ubereats/src/normalize.rs`, `crates/api/src/webhooks.rs`.

- **P3 — Observability, operations, and test coverage**
  - **Telemetry bootstrap** (JSON logs in prod, OTLP tracing, redaction, standard span fields) per `specs/coder/12-observability.md`.
    - Current: minimal fmt subscriber only.
    - Code: `crates/common/src/lib.rs`, all binaries’ `main.rs`.
  - **Metrics emission for core flows** (ingestion/agent/posting/notifications/http/db) per `specs/coder/12-observability.md`.
  - **Readiness checks** (DB + secrets reachable) per `specs/coder/12-observability.md` and `specs/coder/11-api-design.md`.
    - Current `readyz` is unconditional “ok”.
    - Code: `crates/api/src/v1.rs`.
  - **Deployment scaffolding** (Dockerfile, config files, roles flags, CI steps) per `specs/coder/13-deployment-infra.md`.
  - **Testing layers called out in spec** (storage IT with testcontainers, e2e crate, eval binary, undo-window tests) per `specs/coder/14-testing-strategy.md`.

