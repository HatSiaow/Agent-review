# adapter-ubereats

Purpose: Uber Eats Merchant integration. Lists reviews, maps payloads into `domain::Review`, verifies webhook HMAC signatures, and posts replies with platform-specific limits and rate behavior.

Key entry points: `UberEatsReviewClient`, `HttpUberEatsClient`, `InMemoryUberEatsClient`, `UberEatsConfig`, `normalize_ubereats_review`, `verify_webhook_signature`, `needs_personal_handling`. Crypto helpers live in `src/hmac_verify.rs`.

Why tests matter: Signature verification is the main defense against forged webhooks. Normalization bugs mis-attribute ratings or replies and can trigger wrong human-review paths. Client limits and error mapping affect whether real customers see timely, correct public replies.

Local tests:

```
cargo test -p adapter-ubereats
```
