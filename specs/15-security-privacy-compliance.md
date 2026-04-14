# 15 — Security, Privacy & Compliance

## Threat Model (Abridged)

Assets we protect:

- Platform OAuth refresh tokens (Google, UberEats).
- Customer review content (may contain PII).
- Owner and manager account credentials.
- Outbound reply content (authenticity matters — a rogue reply harms the
  brand).

Primary threats:

- **Credential theft** (server compromise, leaked logs, careless backups).
- **Unauthorized posting** (attacker approves or mutates a draft).
- **Injection via review body** (prompt injection, XSS in the UI, stored
  log injection).
- **Webhook spoofing** (unsigned or replayed events).
- **Accidental disclosure** of PII in logs, traces, or LLM training data.

Out of scope: nation-state adversaries, insider threats from the owner
themselves.

## Controls

### Credentials & Secrets

- All secrets in the dedicated secrets backend (see `09-auth-and-secrets.md`).
- Refresh tokens encrypted at rest with envelope encryption.
- No secret is ever logged; `secrecy::SecretString` enforces redaction.
- Master key not on disk in plaintext; loaded from backend at startup.

### Authentication & Authorization

- Argon2id password hashing.
- TOTP 2FA required for `owner`.
- Server-side sessions with short sliding expiry.
- RBAC enforced at the service layer, not just UI.
- All mutating endpoints require CSRF double-submit token.

### Webhook Security

- HMAC signatures verified in constant time.
- Replay protection: the webhook handler stores the source event id and
  rejects duplicates within a 24h window.
- Source IP allow-list as defence-in-depth (configurable).

### Input Handling

- Review bodies are treated as untrusted strings.
- In the UI they are rendered via Askama with HTML-escaping by default; no
  `{{ body | safe }}`.
- In LLM prompts they are wrapped in clearly delimited sections and the
  system prompt instructs the model to treat them as data, not
  instructions. Guardrails provide a second line of defence.
- Logs and traces escape untrusted fields.

### Prompt Injection Defences

- The agent's tool calls are validated against a whitelist of tool names.
- Generated tool arguments are schema-checked before execution.
- No tool performs a network write; worst-case a malicious review causes a
  useless lookup.
- The posting step runs *only* on drafts that have passed human approval,
  so even a fully compromised prompt cannot cause a rogue public reply.

### Transport Security

- TLS 1.2+ everywhere (rustls).
- HSTS (`max-age=31536000; includeSubDomains; preload`) on the UI.
- Secure, HttpOnly, SameSite cookies.

### Rate Limiting & Abuse

- Login, webhook, and public-facing endpoints all have rate limits
  (see `11-api-design.md`).
- A global circuit breaker around each platform adapter prevents runaway
  retries in the event of sustained failure.

## Privacy & PII

Customer reviews frequently contain names and sometimes contact details.
We treat all review bodies as personal data.

### Data Minimisation

- Only the fields needed for replying are stored.
- `raw_payload` is retained for 90 days, then reduced to `{}::jsonb`.
- LLM requests omit any customer data not needed to draft the reply
  (e.g. internal order IDs are replaced with a stable synthetic id).

### Data Subject Rights (GDPR)

- The owner can trigger a **redact-by-source-review-id** operation from the
  admin UI, which replaces `body_text`, `author_display_name`, and
  `raw_payload` with a tombstone and logs the action.
- Redaction propagates to backups through the retention cycle (backups
  older than 30 days age out naturally).
- A short data processing notice is provided as a README fragment for the
  owner to add to their privacy policy.

### LLM Provider Data Handling

- The Anthropic API is configured with a zero-retention / no-training
  setting where available.
- Where the owner prefers fully local inference, an alternate model
  backend (e.g. local Llama via an OpenAI-compatible server) can be
  plugged into `llm_client` — the rest of the system does not care.

## Compliance Posture

- **GDPR**: lawful basis is *legitimate interest* for operational review
  replies, with data subject rights honoured as above.
- **Platform Terms of Service**: the Google and UberEats adapters must
  respect each platform's TOS for automated posting. Specifically:
  - Each reply is human-approved, which satisfies "no unattended
    automated responses" clauses.
  - We do not scrape; only documented APIs are used.
  - Per-platform branding/mention rules are encoded as guardrails.
- **PCI-DSS**: out of scope (we never touch payment data).
- **SOC 2 / ISO 27001**: not targeted for v1.

## Security Testing

- `cargo audit` / `cargo deny advisories` runs in CI.
- Dependabot-equivalent updates weekly.
- Manual review of any crate upgrade that crosses a major version or
  introduces a new `unsafe` block.
- A lightweight security review is part of the PR template for any change
  touching `adapters/*`, `api`, `auth`, or `llm_client`.

## Incident Response

- Incidents are captured in `docs/incidents/YYYY-MM-DD-<slug>.md` using a
  lightweight template: impact, timeline, root cause, corrective actions.
- The runbook lists exact commands to revoke Google / UberEats tokens, to
  rotate the master key, and to take the poster worker offline if a rogue
  reply is suspected.
