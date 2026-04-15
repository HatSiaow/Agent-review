# server

Purpose: Production binary: binds HTTP with `api::router`, runs PostgreSQL or in-memory store, and hosts background workers (ingestion, agent drafting, poster, SLA, notifier outbox).

Key entry points: `src/main.rs` (`main`, worker loops). Shared helpers include sync watermark checks and notification id stability (see `#[cfg(test)]` module in the same file).

Why tests matter: Workers orchestrate the full path from platform fetch to drafted reply to owner alert. Off-by-one cursor or quiet-hours parsing bugs cause missed reviews, duplicate notifications, or premature SLA actions.

Local tests:

```
cargo test -p server
```
