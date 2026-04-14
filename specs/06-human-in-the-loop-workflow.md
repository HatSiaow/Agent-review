# 06 — Human-in-the-Loop Workflow

## Principle

No reply is ever posted without an explicit human action. The owner (or a
delegated manager) must review every draft. The workflow should make the
common case — a brief thanks on a 5★ review — a single click, while still
giving space for careful handling of negative reviews.

## Actors

- **Owner** — primary user. Can approve, edit, reject, and change settings.
- **Manager** — delegated user. Can approve/edit/reject within configured
  rating bounds (e.g. 3–5 ★); 1–2 ★ reviews always escalate to the Owner.
- **Agent (LLM)** — drafts only. Has no authority to post.
- **System** — runs ingestion, posting, notifications.

## Review Queue

The web UI presents three tabs:

1. **Needs You Now** — drafts flagged `sensitive`, `needs_attention`, or
   with guardrail warnings. Sorted by `created_at` ascending.
2. **Ready To Send** — auto-drafted replies for 4–5★ reviews. Sorted by
   rating descending, then `created_at`.
3. **History** — posted, rejected, skipped.

Each card shows the original review, the draft, and per-platform constraints
(remaining characters, language).

## Actions

| Action | Effect |
|---|---|
| **Approve** | Draft state -> `approved`; poster worker picks it up. |
| **Edit & Approve** | Draft text replaced (`generated_by=human_edit`); state -> `approved`; audit logs the diff. |
| **Reject** | Draft state -> `rejected`; owner selects a reason (e.g. "too generic", "wrong tone"). Review returns to the agent's queue up to 2 times. |
| **Skip** | Review status -> `skipped`; no reply will be posted. Reversible. |
| **Regenerate** | Discards the current draft and enqueues a new agent run with a free-text hint. |
| **Escalate** | Manager action only; moves the item to the Owner's queue. |

Each action is a POST to the internal API and is idempotent by draft id +
action token.

## Bulk Approve

For 5★ reviews with no guardrail warnings, the owner may bulk-approve from
the "Ready To Send" tab. Bulk approve has a 10-second undo window during
which the poster worker does not begin.

## Reject Feedback Loop

Rejection reasons are captured as a fixed enum plus optional free text:

- `too_generic`
- `wrong_tone`
- `factually_incorrect`
- `off_policy`
- `language_mismatch`
- `other`

These reasons feed an offline evaluation set (see `14-testing-strategy.md`)
and can be surfaced to the agent on regeneration as a hint.

## SLA & Escalation

- Drafts in `Needs You Now` for more than **2 hours** trigger a second
  notification.
- Drafts in `Needs You Now` for more than **24 hours** are escalated:
  the notifier uses SMS (if configured) and the item is pinned.
- If a review has no action after **72 hours**, the system auto-skips it
  with status `skipped_timeout` and records an audit entry.

## Permissions Model

Simple role-based access control:

```
role: owner    -> all actions
role: manager  -> approve/edit/reject on rating 3..5; read-only on 1..2
role: viewer   -> read-only
```

Sessions are cookie-based, backed by the `api` service (see `11-api-design.md`).

## Audit

Every action writes an `AuditEvent` with the acting user, before/after draft
text (for edits), and the previous/next state. The owner can view history
per review.
