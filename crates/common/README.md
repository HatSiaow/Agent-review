# common

Purpose: Small shared utilities for binaries, mainly `RUST_LOG`-aware tracing initialization used by `server` and `cli`.

Key entry points: `init_tracing` in `src/lib.rs`.

Why tests matter: Broken logging setup fails silently in production and slows incident response. Keeping this crate tested avoids duplicated, diverging bootstrap code across services that handle sensitive review traffic.

Local tests:

```
cargo test -p common
```
