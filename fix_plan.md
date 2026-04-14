## Neighbourhood Restaurant Review Agent — Fix Plan

Bullet list of **spec gaps not yet implemented**, sorted by priority (P0 highest). File paths/specs are referenced so each item is actionable.

- **P0 — Secure, correct human-in-the-loop posting (must-have before “real” use)**
  - **Implement real auth + sessions (owner/manager/viewer)** per `specs/coder/09-auth-and-secrets.md`, `specs/coder/06-human-in-the-loop-workflow.md`, `specs/coder/11-api-design.md`, `specs/coder/16-frontend-web-ui.md`.
    - Remaining: login/session middleware + user identity derivation (stop accepting `user_id` in request bodies), role assignment UX/admin path, and end-to-end coverage.
    - Code: `crates/api/src/lib.rs`, `crates/api/src/v1.rs`, `crates/storage/src/{pg.rs,memory.rs}`.
  - **Implement secrets backend + encryption-at-rest plumbing** (the `Secrets` trait + `EnvFileSecrets`(age) + cloud backends + envelope encryption helper) per `specs/coder/09-auth-and-secrets.md` and `specs/coder/15-security-privacy-compliance.md`.
    - Current gaps: no `Secrets` trait/implementations found in code; sensitive columns (e.g. `users.totp_secret`) are plaintext; adapters/LLM client read raw strings from env/config.
    - Code: new crate/module (likely `crates/common` or a new `crates/secrets` + `crates/encryption`), plus storage migrations updates.
  - **Posting must be gated on explicit human approval, with correct state transitions and auditing** per `specs/coder/06-human-in-the-loop-workflow.md`.
    - Add missing transitions/guards in `domain` FSMs and enforce server-side (not just UI).
    - Code: `crates/domain/src/{fsm.rs,review_fsm.rs,audit.rs}`, `crates/api/src/v1.rs`, `crates/poster/src/lib.rs`, `crates/server/src/main.rs`.
  - **Fix Problem Details to be RFC 9457-aligned** (content-type `application/problem+json`, proper `instance`, consistent mapping of validation errors) per `specs/coder/11-api-design.md`.
    - Code: `crates/api/src/problem.rs`, `crates/api/src/v1.rs`.

- **P0 — Data/storage correctness (otherwise the inbox will lose/duplicate work)**
  - **Make storage schema + repo behavior match the specs** (`model_version`, defaults, indexes, constraints, proper tables) per `specs/coder/08-data-storage.md` and `specs/coder/04-unified-review-data-model.md`.
    - Biggest mismatches: `agent_runs`, `notifications_outbox`, `reviews_sync_state`, `users` columns/constraints, missing defaults/indexes.
    - Code: `crates/storage/migrations/0001_init.sql`, `crates/storage/src/pg.rs`.
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
  - **Replace demo “scan loops” with durable job/outbox processing** per `specs/coder/01-architecture.md`, `specs/coder/07-notification-system.md`, `specs/coder/13-deployment-infra.md` and the new `specs/coder/17-work-queues-and-outbox-processing.md`.
    - Current `crates/server/src/main.rs` polls `list_reviews/list_drafts` every 2s and uses in-memory de-dupe; no durable queues/outbox claims.
    - Code: `crates/server/src/main.rs`, `crates/storage`, plus new queue/outbox abstractions.
  - **Ingestion orchestration** (Google polling with stop-at-watermark; UberEats webhook enqueues + polling fallback “since last created_at”; per-location sync state) per `specs/coder/02-google-reviews-integration.md`, `specs/coder/03-ubereats-reviews-integration.md`.
    - Remaining: “enqueue + polling fallback” orchestration, DLQ behaviors, and per-location fanout/locking (watermark persistence is now in place).
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
  - **Fix `llm_client` usage accounting and cost cap enforcement** per `specs/coder/05-ai-response-agent.md` and `specs/coder/13-deployment-infra.md`.
    - Current gaps: prompt/completion tokens are always `0`; “usage logs” and monthly cost-cap enforcement are not implemented.
    - Code: `crates/llm_client/src/lib.rs`.
  - **Tooling model: real tool-use protocol (JSON args/results + result hashes)** per `specs/coder/05-ai-response-agent.md`.
    - Current: “tools” are deterministic string context builders, not LLM-invoked tool calls.
    - Code: `crates/agent/src/lib.rs`, plus new tool interface crate/module.
  - **Guardrail rule alignment** (refund promise allowlist/policy patterns, supported language config, restaurant-name self-reference alternative) per `specs/coder/05-ai-response-agent.md`.
    - Code: `crates/domain/src/guardrails.rs`.
  - **Replace hard-coded “Chez Luca” defaults with persisted restaurant settings** (name, cuisine, hours, signature dishes, voice) per `specs/coder/16-frontend-web-ui.md` and `specs/coder/11-api-design.md` (`/api/v1/settings`).
    - Current: agent config and prompts embed demo defaults; no settings endpoints/storage exist.
    - Code: `crates/agent/src/lib.rs`, `crates/llm_client/src/lib.rs`, `crates/api/src/v1.rs`, `crates/storage` (new table), UI templates.

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
  - **Implement config loading via `figment` + `APP_ENV` + `APP_` env prefix** per `specs/coder/13-deployment-infra.md` and `specs/coder/10-rust-tech-stack.md`.
    - Current: ad-hoc env reads, inconsistent naming (`APP_BIND_ADDR` vs `DATABASE_URL` vs `ANTHROPIC_API_KEY`), and no `config/` directory.
    - Code: `crates/common`, all binaries, `crates/storage/src/pg.rs`.
  - **Server role selection flag (`--roles`) and SIGTERM shutdown** per `specs/coder/10-rust-tech-stack.md` and `specs/coder/13-deployment-infra.md`.
    - Current: server always starts all workers; shutdown listens to Ctrl+C only.
    - Code: `crates/server/src/main.rs`.
  - **Deployment scaffolding** (Dockerfile, config files, roles flags, CI steps) per `specs/coder/13-deployment-infra.md`.
  - **Testing layers called out in spec** (storage IT with testcontainers, e2e crate, eval binary, undo-window tests) per `specs/coder/14-testing-strategy.md`.
  - **Finish CLI operational commands** (`google-auth` flow, token rotation, migrate status, replay tools) per `specs/coder/09-auth-and-secrets.md`, `specs/coder/08-data-storage.md`, `specs/coder/10-rust-tech-stack.md`.
    - Current: `google-auth` and `migrate status` are placeholders.
    - Code: `crates/cli/src/main.rs`.

- **Completed (this session)**
  - **Storage upsert semantics for reviews (pg + memory)**: switched to “update on conflict” semantics (no more `DO NOTHING`); refreshes stored review data when a duplicate `(platform, source_review_id)` arrives.
    - Code: `crates/storage/src/{pg.rs,memory.rs,repo.rs}`
  - **Google polling stop-at-watermark persisted via `reviews_sync_state`**: persist/read watermark so polling stops correctly and resumes without re-scanning.
    - Code: `crates/storage/src/{pg.rs,repo.rs}`, `crates/api/src/store.rs`
  - **UberEats webhook replay protection (event_id or sha256 fallback) + tests**: dedupe webhook deliveries using provider `event_id` when present, else a sha256 fallback; added coverage around replay behavior.
    - Code: `crates/api/src/webhooks.rs`, `crates/storage/src/{pg.rs,memory.rs,repo.rs}`
  - **Enforce bulk approve 10s undo window gating (backend + repo guard)**: prevent posting/processing until the undo window has elapsed; added repo-level guard so workers can’t bypass API/UI timing.
    - Code: `crates/api/src/v1.rs`, `crates/storage/src/{pg.rs,memory.rs,repo.rs}`, `crates/server/src/main.rs`
  - **Audit events writing (pg + memory) + ability to list audit events**: write audit rows for key mutations and expose read/list capability.
    - Code: `crates/domain/src/audit.rs`, `crates/storage/src/{pg.rs,memory.rs,repo.rs}`, `crates/api/src/v1.rs`
  - **Minimal RBAC (viewer read-only) + CSRF double-submit checks for mutating endpoints**: added “viewer cannot mutate” enforcement and CSRF double-submit verification on write routes.
    - Code: `crates/api/src/v1.rs`
  - **Idempotency-Key caching for approve/reject/bulk/undo (repo-backed) + migrations**: persist idempotency keys to prevent duplicate mutations; added schema changes.
    - Migrations: `crates/storage/migrations/0003_idempotency_keys.sql`
    - Code: `crates/api/src/v1.rs`, `crates/storage/src/{pg.rs,memory.rs,repo.rs}`
  - **Implement migrations runner (embedded migrations) and update CLI to await**: embedded migrations and ensured CLI blocks until migrations complete.
    - Migrations: `crates/storage/migrations/0002_webhook_events.sql`, `crates/storage/migrations/0003_idempotency_keys.sql`
    - Code: `crates/storage/src/pg.rs`, `crates/server/src/main.rs`

