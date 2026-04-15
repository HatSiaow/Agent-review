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
#
# Secrets are loaded from a backend selected by SECRETS_BACKEND:
# - memory  (default): process-local, empty on boot
# - envfile: encrypted env file (age)
# - aws    : AWS Secrets Manager (prefix-based)

# -----------------------
# Required secret (auth)
# -----------------------
# The server requires `app.session_secret` (>= 32 bytes). You can provide it via the secrets backend
# under the key `app.session_secret`, or via env var fallback:
export APP_SESSION_SECRET="change-me-change-me-change-me-change-me"

# -----------------------
# Run with SECRETS_BACKEND=memory (default)
# -----------------------
APP_BIND_ADDR=127.0.0.1:3000 SECRETS_BACKEND=memory cargo run -p server

# -----------------------
# Run with SECRETS_BACKEND=envfile (age-encrypted env file)
# -----------------------
# Required:
# - APP_AGE_IDENTITY_FILE: path to an age identity file used for decrypting the envfile at runtime
#
# Optional:
# - APP_SECRETS_FILE: path to the encrypted envfile (default: secrets.env.age)
# - APP_AGE_RECIPIENT: required only when *writing* envfile secrets (not needed to run the server)
export APP_AGE_IDENTITY_FILE="$HOME/.config/age/keys.txt"
export APP_SECRETS_FILE="secrets.env.age"
# export APP_AGE_RECIPIENT="age1..."
APP_BIND_ADDR=127.0.0.1:3000 SECRETS_BACKEND=envfile cargo run -p server

# -----------------------
# Run with SECRETS_BACKEND=aws (AWS Secrets Manager)
# -----------------------
# Optional:
# - APP_AWS_SECRETS_PREFIX: secret name prefix (default: rr-agent)
export APP_AWS_SECRETS_PREFIX="rr-agent"
APP_BIND_ADDR=127.0.0.1:3000 SECRETS_BACKEND=aws cargo run -p server

# Postgres storage (any secrets backend):
# export DATABASE_URL="postgres://..."
# APP_BIND_ADDR=127.0.0.1:3000 SECRETS_BACKEND=memory cargo run -p server
```

### Secrets configuration

The server reads secrets by **key name** from the configured backend:

- **Required**
  - `app.session_secret` (must be \(\ge 32\) bytes; env fallback: `APP_SESSION_SECRET`)
- **Optional**
  - `google.oauth_refresh_token`
  - `ubereats.oauth_client_secret`
  - `ubereats.webhook_secret` (env fallback: `UBEREATS_WEBHOOK_SECRET`)
  - `anthropic.api_key`

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

