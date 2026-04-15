# cli

Purpose: `rr-agent` command-line tool for operator tasks (database migrations against `storage`, placeholder Google OAuth). Shares tracing setup with servers via `common`.

Key entry points: `src/main.rs` (`Migrate`, `GoogleAuth` commands).

Why tests matter: Migration mistakes brick deployments and can corrupt review or draft tables. Even thin CLIs should stay buildable and covered as tooling grows, so schema steps stay aligned with `storage` migrations.

Local tests:

```
cargo test -p cli
```
