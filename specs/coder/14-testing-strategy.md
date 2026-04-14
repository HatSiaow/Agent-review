# 14 — Testing Strategy

## Test Layers

| Layer | Scope | Runs in |
|---|---|---|
| Unit | Pure functions, FSM transitions, normalizers, guardrails | `cargo test` |
| Integration | Crate-level, using real Postgres in a container | `cargo test -p storage --features it` |
| HTTP contract | Adapters against fake HTTP servers | `cargo test -p adapters-google` |
| End-to-end | `server` binary, real Postgres, fake upstreams, fake LLM | `cargo test -p e2e` |
| LLM evaluation | Offline, against fixture reviews | `cargo run -p eval` |

## Unit Tests

- Colocated in `mod tests` blocks under `#[cfg(test)]`.
- The `domain` crate has extensive state-machine coverage: every
  permitted transition has at least one test, every forbidden transition is
  asserted to return `InvalidTransition`.
- Guardrail rules have a table-driven test using `rstest` with curated
  positive and negative examples.

## Integration Tests

- `storage` crate spins up Postgres via `testcontainers` (one container per
  test binary, shared by tests).
- Each test runs in its own transaction that is rolled back at the end,
  giving isolation without drop-create cycles.
- Migrations are applied once per container.

## HTTP Contract Tests (Adapters)

- `wiremock` provides a fake server that replays canned responses.
- Fixtures live in `crates/adapters/<platform>/tests/fixtures/` and are
  anonymised samples captured from the real APIs during development.
- Each adapter has tests for:
  - Happy path ingestion.
  - Pagination & stop-at-cursor.
  - 429 backoff (asserts wait behaviour with `tokio::time::pause`).
  - 5xx retry then give up.
  - Auth refresh on 401.
  - Posting success / failure cases.

## End-to-End Tests

- `e2e` crate starts the full `server` binary in-process with:
  - Real Postgres (testcontainers).
  - Fake Google + UberEats upstreams (`wiremock`).
  - Fake LLM (`llm_client` has a `#[cfg(test)] InMemoryModel` that serves
    scripted responses).
- Scenarios:
  - A new 5★ Google review flows from ingestion → draft → approve → post.
  - A 1★ UberEats review is flagged `sensitive` and triggers an SMS stub.
  - A draft rejected once is regenerated; twice gives up and escalates.
  - Ingestion outage for 2 hours triggers an `IngestionStalled` alert.
  - Approval during the undo window can be rolled back.

## LLM Evaluation

A dedicated `eval` binary runs the agent over a curated set of **~60 fixture
reviews** representing diverse ratings, languages, and edge cases.

For each fixture the evaluator checks:

- **Deterministic properties**: language matches, length within limits,
  no banned phrases, no PII, guardrails pass.
- **Scored properties**: tone, specificity, warmth — scored by a second LLM
  prompt acting as judge. Scores below a threshold fail the build.
- **Golden file comparisons**: prompt templates are snapshotted with `insta`
  so accidental prompt changes are reviewed explicitly.

Evaluation runs:

- On every PR that touches `agent/` or prompts (required).
- Nightly on `main` (reports sent to the owner).
- A historical score trend is stored to catch regressions.

## Negative / Security Tests

- Webhook signature verification: rejects wrong signature, accepts right.
- CSRF: missing token rejected, wrong token rejected.
- Session fixation: login issues a fresh session id.
- SQL injection: parameterised queries asserted via `sqlx` type-check; a
  fuzz test runs a small corpus against a few search endpoints.

## Performance Smoke

A lightweight criterion benchmark for:

- Review normalization (should be < 1ms per review).
- Guardrail check (should be < 2ms per draft).

Not a full load test — not needed at this scale.

## CI Matrix

- `ubuntu-latest` only (prod target).
- One Rust toolchain (stable). We do not test MSRV against multiple versions.

## Fixtures & Secrets in Tests

- All fixture payloads have been stripped of real PII.
- Tests never read real secrets; the `Secrets` trait is replaced by an
  `InMemorySecrets` implementation seeded from the test harness.
