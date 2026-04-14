# Diff Review Checklist (Reviewer-Only)

This folder is intentionally scoped to what a reviewer needs to validate a PR:
**diff impact**, **tests**, and **acceptance criteria**.

## What to verify on every PR

- **Diff scope**: changed modules match the intended spec area (no accidental refactors).
- **State-machine correctness**: transitions and invariants are preserved (see AC).
- **External boundaries**: adapter HTTP behavior, webhook verification, auth/session behavior.
- **Observability**: new flows emit trace/log/metric fields that allow end-to-end debugging.
- **Secrets & PII**: no secrets in logs; PII handled as specified; fixtures anonymised.

## Test expectations (from spec)

- **Unit**: FSM transitions, normalizers, guardrails.
- **Integration**: storage + Postgres (testcontainers), migrations applied once, per-test rollback.
- **HTTP contract**: wiremock fixtures; pagination; retry/backoff; auth refresh; posting outcomes.
- **E2E**: full server with real Postgres + fake upstreams + fake LLM; core scenarios pass.
- **Eval**: prompt changes and agent logic changes run offline evaluation and must not regress.

Canonical details live in `specs/reviewer/14-testing-strategy.md`.
