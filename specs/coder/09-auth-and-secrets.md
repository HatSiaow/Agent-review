# 09 — Authentication & Secrets Management

## User Authentication (App Access)

- **Primary**: email + password with Argon2id password hashing (via `argon2`
  crate, memory=64MB, iterations=3).
- **Session management**: signed, HttpOnly, SameSite=Lax cookies. Sessions
  stored server-side in the `sessions` table with 14-day sliding expiry.
- **2FA (optional but recommended)**: TOTP via `totp-rs`. Required for the
  `owner` role by default; configurable.
- **Password reset**: time-limited (30 min), single-use token emailed to
  the registered address.
- **Rate limiting**: login endpoint limited to 5 attempts per 15 minutes per
  IP + email pair.

Single-restaurant scope means user count is small (1 owner + ~2 managers).
No self-service signup — users are invited by the owner.

## Platform OAuth (Google)

- OAuth 2.0 authorization-code flow performed once during setup via a CLI
  command (`cargo run -p cli -- google-auth`) which opens a browser, captures
  the code, and exchanges it for a refresh token.
- The refresh token is persisted in the secrets store; only it (and a short-
  lived access token cache) is kept.

## Platform OAuth (UberEats)

- Client-credentials flow. The client id and secret are loaded from the
  secrets store; the adapter mints access tokens on demand.

## Secrets Storage

A `Secrets` trait abstracts the backend:

```rust
#[async_trait]
pub trait Secrets: Send + Sync {
    async fn get(&self, key: &str) -> Result<SecretString>;
    async fn put(&self, key: &str, value: SecretString) -> Result<()>;
}
```

Two implementations ship:

1. **`EnvFileSecrets`** — reads from a local `.env` file encrypted at rest
   via `age`. Suitable for single-box deployments on the restaurant's own
   server or a small VPS.
2. **`CloudSecretsManager`** — thin client for AWS Secrets Manager /
   Google Secret Manager / HashiCorp Vault. Selected by env
   (`SECRETS_BACKEND=aws|gcp|vault`).

No secret is ever written to logs. The `SecretString` type
(`secrecy::SecretString`) enforces `Debug` redaction.

## Webhook Signature Verification

- **UberEats webhooks**: HMAC-SHA256 over the raw body using the shared
  secret. Comparison is constant-time (`subtle::ConstantTimeEq`).
- **Any future webhook sources** must provide verifiable signatures before
  being enabled.

## Encryption at Rest

- All sensitive DB columns (refresh tokens, password hashes, 2FA seeds) are
  stored encrypted with a per-row envelope using AES-256-GCM; the data key
  is wrapped by a master key loaded from the secrets backend at startup.
- The `encryption` crate centralises this logic so no service calls the
  primitives directly.

## Key Rotation

- Master key rotation is supported via a dual-key window: the service accepts
  both the old and new key during the rotation window, re-encrypts rows
  lazily on read, and logs a rotation completion event once all rows have
  been migrated.
- OAuth refresh tokens are not rotated automatically but the CLI provides a
  `rotate-google-auth` subcommand for when Google invalidates them.

## Audit

All authentication events (login success/failure, 2FA enrol, password reset,
OAuth re-auth) are written to `audit_events`.
