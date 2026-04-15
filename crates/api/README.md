# api

Purpose: Axum HTTP surface for health, metrics, versioned JSON API, HTML UI, and partner webhooks. Wires `storage` and `domain` for authenticated review and draft workflows.

Key entry points: `router`, `Store` in `src/lib.rs`. Major modules: `v1`, `webhooks`, `web_ui`, `auth`, `auth_cookies`, `login_rate_limit`, `problem`, `request_ctx`, `store`.

Why tests matter: This boundary accepts untrusted HTTP input and drives state transitions. Weak auth, webhook handling, or rate limits can expose accounts, drop reviews, or allow duplicate side effects. Tests here guard the contract every browser and platform hits.

Local tests:

```
cargo test -p api
```
