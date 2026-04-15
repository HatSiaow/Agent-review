# domain

Purpose: Pure domain model: reviews, drafts, users, settings, audit events, validation, guardrails, and finite-state machines for review and draft lifecycles. No I/O.

Key entry points: `model`, `review_fsm`, `fsm` (draft FSM), `guardrails`, `validation`, `settings`, `audit` (see `src/lib.rs` exports).

Why tests matter: This crate encodes what states are legal and what text or transitions are allowed. Bugs here cause wrong public replies, skipped human review, or inconsistent audit trails across the whole restaurant-review pipeline.

Local tests:

```
cargo test -p domain
```
