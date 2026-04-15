## Neighbourhood Restaurant Review Agent (Rust)

### Workspace layout

- **Binaries**
  - `crates/server`: runs API + background workers (ingestion/agent/poster/notifier) in one process
  - `crates/cli`: operational CLI (`rr-agent`)
- **Libraries**
  - `crates/domain`: unified review model + state machines + guardrails
  - `crates/storage`: repository boundary (`InMemoryRepository` + `PgRepository`)
  - `crates/api`: axum HTTP API + webhooks
  - `crates/agent`: generates reply drafts (LLM-backed or deterministic)
  - `crates/poster`: posts approved replies back to Google / UberEats
  - `crates/ingestion`: ingestion helpers (dedupe keys, sync state helpers)
  - `crates/adapters/google`, `crates/adapters/ubereats`: platform adapters

### Build

```bash
cargo build
```

### Run (dev)

```bash
# Starts the API and in-process workers.
# NOTE: required for auth/session cookies (must be >= 32 chars).
export APP_SESSION_SECRET="change-me-change-me-change-me-change-me"

# In-memory mode (default): no DB required.
APP_BIND_ADDR=127.0.0.1:3000 cargo run -p server

# Postgres mode: wire storage via DATABASE_URL; migrations run on server startup.
# export DATABASE_URL="postgres://..."
# APP_BIND_ADDR=127.0.0.1:3000 cargo run -p server
```

### Login (dev)

- Login endpoint: `POST /api/v1/auth/login`
- In-memory mode seeds a dev owner user:
  - email: `owner@example.com`
  - password: `password`

### Database migrations (Postgres)

The storage crate embeds SQL migrations from `crates/storage/migrations/`. When running with `DATABASE_URL` set, the server applies migrations at startup. You can also run migrations manually via the CLI:

```bash
export DATABASE_URL="postgres://..."
cargo run -p cli -- migrate up
```

### Tests (targeted loop)

For API, persistence, and the combined server process without running the entire workspace:

```bash
cargo test -p api -p storage -p server
```

### Tests (full workspace)

```bash
cargo test
```

### Formatting and commit scope

`cargo fmt` (and some fix-oriented tooling) can rewrite many files across crates. Run formatting when you intend to, or stage only the files that belong to your change, so commits stay focused on the work at hand.

