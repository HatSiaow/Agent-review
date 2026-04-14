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
APP_BIND_ADDR=127.0.0.1:3000 cargo run -p server
```

### Database migrations (Postgres)

The storage crate embeds SQL migrations from `crates/storage/migrations/`.

```bash
export DATABASE_URL="postgres://..."
cargo run -p cli -- migrate up
```

### Tests (fast loop)

```bash
cargo test -p domain -p storage -p api
```

### Tests (full workspace)

```bash
cargo test
```

