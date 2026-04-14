# 13 — Deployment & Infrastructure

## Deployment Model

The target customer is a single small restaurant, so the default deployment
is deliberately modest:

- **One small Linux VM** (1–2 vCPU, 2 GB RAM) or a single container host.
- **PostgreSQL** — either on the same VM or a managed small instance.
- **Reverse proxy / TLS** — Caddy (automatic Let's Encrypt) in front of the
  Rust binary.

A more ambitious deployment (e.g. separate workers on Kubernetes) is
supported by the same code but not the default.

## Environments

| Env | Purpose | Data |
|---|---|---|
| `dev` | Local laptop | SQLite? No — Postgres via Docker. Fake adapters. |
| `staging` | Pre-prod on a tiny VM | Real Postgres. Sandbox OAuth creds. Reviews from fixture data. |
| `prod` | The restaurant's live site | Real creds, real reviews |

Environment is selected by `APP_ENV` which maps to config files under
`config/` (e.g. `config/prod.toml`). Secrets come only from the secrets
backend, never from TOML files.

## Container Image

- `Dockerfile` builds a static-ish binary with `rustls` (no system OpenSSL).
- Multi-stage: `rust:1.XX` builder → `gcr.io/distroless/cc-debian12` runtime.
- Image size target: < 50 MB.
- Non-root user (`uid 10001`).
- `HEALTHCHECK` hits `/healthz`.

## Process Model

In single-process mode (default for small restaurants):

```
server --roles ingestion,agent,poster,notifier,api
```

The single binary runs all services as tokio tasks. This is simple to
operate and matches the expected load. Switching to multi-process simply
involves running multiple instances with different `--roles`.

## CI/CD

- GitHub Actions.
- Workflow: `ci.yml`
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `cargo deny check` (licenses + advisories)
  - `sqlx prepare --check` (offline queries)
- Workflow: `release.yml`
  - Triggered on git tag `v*`.
  - Builds the Docker image and pushes to the registry.
  - Deploys to staging automatically; prod deploy is a manual approval step.

## Database Migrations

- Migrations run as an init-container / pre-start hook that executes
  `server migrate up` before the main process starts.
- Migrations are idempotent and forward-only. Rollback is done via a new
  migration, not by reverting.

## Configuration

- `figment` layered config: built-in defaults → TOML file → env vars.
- All config is documented in `config/default.toml` with comments.
- Env var prefix: `APP_` (e.g. `APP_DATABASE_URL`, `APP_GOOGLE_POLL_SECONDS`).

## Resource Budgets

Expected steady-state load (one restaurant, ~50 reviews/week):

- CPU: < 5% on a single vCPU.
- RAM: ~200 MB.
- DB size: < 1 GB for the first several years.
- LLM spend: dominated by the agent. Budget ~200k input tokens + ~50k output
  tokens per month. A monthly cost cap is enforced by the `llm_client` and
  surfaced in the UI.

## Backups & Recovery

- See `08-data-storage.md` for database backup policy.
- Secrets are backed up out-of-band (owner-held password manager entry
  for the master key + encrypted secrets bundle).
- Runbook `deploy/runbook.md` covers: cold start, re-key, platform
  re-auth, full restore.

## Zero-Downtime Assumptions

None required. For a single-restaurant deployment, a 60-second restart
during low-traffic hours is acceptable. The ingestion workers resume from
their `reviews_sync_state` rows after restart without losing reviews.
