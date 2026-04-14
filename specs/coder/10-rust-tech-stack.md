# 10 — Rust Technical Stack

## Toolchain

- **Rust edition**: 2021.
- **MSRV**: stable (latest at time of build), pinned via `rust-toolchain.toml`.
- **Lints**: `clippy::pedantic` enabled with a curated allow-list; `#![deny(unsafe_code)]`
  at the workspace level.
- **Formatting**: `rustfmt` with default config + `group_imports = "StdExternalCrate"`.

## Workspace Layout

A single Cargo workspace:

```
Agent-review/
├── Cargo.toml            # [workspace]
├── crates/
│   ├── domain/           # Pure domain types: Review, ReplyDraft, enums, FSM
│   ├── storage/          # sqlx-based repository; migrations
│   ├── adapters/
│   │   ├── google/       # Google Business Profile adapter
│   │   └── ubereats/     # UberEats Merchant adapter
│   ├── llm_client/       # Anthropic API client + retries + usage logging
│   ├── agent/            # Agent loop, tools, guardrails
│   ├── ingestion/        # Poll/webhook orchestration, dedup, enqueue
│   ├── poster/           # Background worker for publishing replies
│   ├── notifier/         # Notification outbox + channel senders
│   ├── api/              # HTTP service (axum) + auth + web UI server
│   ├── web_ui/           # Server-rendered templates (Askama) + htmx
│   ├── cli/              # Admin CLI (migrate, google-auth, replay, etc.)
│   └── common/           # Config loading, errors, tracing bootstrap
└── specs/
    ├── coder/            # Full specifications for implementation
    └── reviewer/         # Reviewer-only specs: diff, tests, acceptance criteria
```

Binaries live in `crates/api`, `crates/cli`, and one `server` binary that can
run any subset of services via a `--role ingestion,agent,poster,notifier,api`
flag (single-process mode).

## Key Dependencies

| Concern | Crate |
|---|---|
| Async runtime | `tokio` (multi-thread) |
| HTTP server | `axum` + `tower` + `tower-http` |
| HTTP client | `reqwest` (rustls) |
| Database | `sqlx` (postgres, tls, macros offline) |
| Migrations | `refinery` |
| JSON | `serde`, `serde_json` |
| Time | `time` or `chrono` (pick one workspace-wide; spec chooses `time`) |
| UUIDs | `uuid` with `v4`, `serde` |
| Logging | `tracing`, `tracing-subscriber`, `tracing-opentelemetry` |
| Metrics | `metrics` + `metrics-exporter-prometheus` |
| Errors | `thiserror` for libraries, `anyhow` for binaries |
| Retries | `backoff` with tokio feature |
| Config | `figment` (env + toml) |
| Secrets | `secrecy`, `age`, (optional) `aws-sdk-secretsmanager` |
| Crypto | `argon2`, `aes-gcm`, `hmac`, `sha2`, `subtle` |
| Templates | `askama` (server UI), `tera` (emails) |
| Webhooks | `axum` handlers with raw body extractor |
| Email | `lettre` |
| Web push | `web-push` |
| Tests | `insta` (snapshots), `wiremock` (HTTP fakes), `rstest` |
| CLI | `clap` (derive) |

## Async & Concurrency

- One multi-thread tokio runtime per process.
- Background workers (ingestion, agent, poster, notifier) are
  `tokio::spawn`ed long-running tasks with graceful shutdown via a
  `CancellationToken`.
- Shared state is immutable where possible; mutable state uses
  `tokio::sync::Mutex` or `RwLock` sparingly.

## Error Handling

- Library crates (`domain`, `storage`, `adapters/*`, `llm_client`, `agent`)
  define their own error enums with `thiserror`.
- Binary crates (`api`, `cli`, `server`) use `anyhow::Result` at the outer
  layer and convert.
- Errors crossing an HTTP boundary become `ApiError` with a machine-readable
  code and a user-safe message.

## Feature Flags

Minimal. Features are used only to gate optional dependencies (`aws-secrets`,
`gcp-secrets`, `twilio`). Runtime behaviour is controlled via config, not
compile-time features.

## Coding Standards

- Prefer `&str` and owned `String` at boundaries; avoid lifetimes in public
  APIs unless they add real value.
- Functions > ~60 lines or > 3 levels of nesting get split.
- Module-level docs on every public module.
- No `unwrap()` / `expect()` outside tests and `main.rs` startup.
- Every external call (HTTP, DB, LLM) is wrapped in a `#[tracing::instrument]`
  span with relevant fields.
