## `agent` — Reply Drafting

This crate generates **reply drafts** for a unified `domain::Review`.

### Why this crate exists

- **Consistency**: drafts must obey platform constraints (e.g. reply length caps) and restaurant tone.
- **Safety**: guardrails are enforced in Rust so we never auto-post unsafe content.
- **Traceability**: draft generation is part of an auditable workflow (draft created → human approves → poster posts).

### Key entry point

- `agent::run_agent(...)` returns an `AgentResult` containing a `domain::ReplyDraft`.

### Tests

Tests for agent-specific behavior should live next to the implementation in this crate (unit tests in `src/` and integration tests in `tests/`).

