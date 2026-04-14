# 16 — Frontend Web UI (Askama + htmx) Specification

## Scope

This spec defines the **server-rendered web UI** for the restaurant owner/manager.

- Rendering: **Askama templates** served by the `api` crate.
- Interactivity: **htmx** for partial updates + a small amount of progressive JS only when necessary.
- Data: UI reads from server-rendered HTML and uses the internal JSON API (`/api/v1/...`) for mutating actions and selective dynamic updates (see `11-api-design.md`).

Non-goals:

- A standalone SPA.
- A public third-party API or multi-tenant UI.

## Primary User Goals

- **Process the queue fast**: approve most 4–5★ drafts in one click.
- **Handle sensitive/negative reviews carefully**: see warnings, edit, and/or reject with clear reasons.
- **Never post accidentally**: every posting action is explicitly human-approved, with clear confirmation/undo where required.
- **Maintain context**: view full review + history + audit trail per item.

## UI Information Architecture

### Top-level navigation

- **Queue** (default landing): three tabs as defined in `06-human-in-the-loop-workflow.md`
  - Needs You Now
  - Ready To Send
  - History
- **Settings**
  - Restaurant profile + voice
  - Hours + quiet hours
  - Notification preferences (surface state; actual behavior is defined in `07-notification-system.md`)
- **Users**
  - List, invite, remove (owner only)
- **Account**
  - Login / logout
  - 2FA enrollment (owner required by default; see `09-auth-and-secrets.md`)

### Global UI elements

- **Environment/status banner** (non-dismissable when unhealthy)
  - Shows: degraded mode (e.g. notifier failures), posting backlog, SLA breaches if any are available.
  - Source: server-rendered from backend state; no client polling in v1 unless explicitly implemented.
- **Flash messages**
  - Success: “Approved”, “Skipped”, “Draft regenerated”, etc.
  - Error: Problem-details `title` and a short “Try again” hint.

## Routing & Pages

Routes are served by the `api` crate; HTML routes are not versioned.

### `GET /` (Queue landing)

Default to the **Queue** view. If unauthenticated, redirect to login.

Contains:

- Tabs: Needs You Now / Ready To Send / History
- Filter/search controls (server-rendered, submit via query params)
  - `platform` (google/ubereats)
  - `rating` (1–5)
  - `q` (free text search against author/body; backend-defined semantics)
- List of **Review cards** (see “Card layout”)
- Bulk actions bar (Ready To Send tab only; owner only where applicable)

### `GET /reviews/{id}` (Review detail)

Shows one review and its full timeline:

- Original review (with platform metadata)
- Active draft (if any) + editor
- History: prior drafts, rejected reasons, posted outcome, skip/unskip, drift events
- Audit events (read-only)

### `GET /settings`

Owner can edit; manager can read if allowed by RBAC (see below).

### `GET /users`

Owner only:

- List users + roles
- Invite flow
- Remove user

### Auth routes

Exact endpoints are implementation-defined, but must satisfy:

- Email/password login with rate limiting (see `09-auth-and-secrets.md`)
- Session cookies: `HttpOnly`, `SameSite=Lax`
- 2FA enrollment + challenge where enabled/required

## Review Card Layout (Queue)

Each queue item is a card showing:

- **Platform badge**: Google / UberEats
- **Rating**: 1–5 stars
- **Author**: display name (with safe fallback if anonymous)
- **Created time**: “x hours ago” + exact timestamp on hover
- **Review body**: text or “(Rating only)”
- **Context**: key-value snippets (e.g. ordered items) when present
- **Draft preview**: active draft text
- **Constraints**: remaining characters and language match indicator
- **Flags / warnings**:
  - `sensitive`, `needs_attention`, guardrail warnings, platform rejection, drift detected
  - Warnings must be visually prominent and keyboard accessible

Cards must clearly indicate when actions are disabled due to RBAC or state.

## Interactions & State Transitions

### Common requirements for all mutating actions

- Mutations are POSTs to `/api/v1/...` and require:
  - Valid session cookie
  - `X-CSRF-Token` header (double-submit cookie pattern; see `11-api-design.md`)
- Mutations should supply an `Idempotency-Key` header (UUID v4) generated per user action to prevent duplicate submissions when the user double-clicks or network retries.
- UI must handle RFC 9457 problem-details errors consistently:
  - Show `title` to the user
  - Log/display `code` if available (developer-facing)
  - For `409 invalid_transition`, refresh that card from server state

### Approve

Action:

- UI button **Approve** on a draft in `pending_review` (or `edited` depending on backend state machine).
- Calls `POST /api/v1/drafts/{id}/approve` with optional body `{ "text"?: string }`.

Expected UX:

- On success: card updates to “Approved” state immediately (htmx swap or JSON->HTML partial refresh).
- Show posting is asynchronous: “Approved — will post shortly”.
- If posting later fails, surface in History and on the card (when visible) as `failed` with error summary.

### Edit & Approve

Action:

- Inline editor expands within the card or opens on the detail page.
- Submit calls the same approve endpoint with `{ "text": "<edited>" }`.

UX constraints:

- Show a live character count + remaining characters for the platform.
- Warn (but do not block) if language mismatches `body_language`; allow override.

Audit:

- Must result in audit diff (as per `06-human-in-the-loop-workflow.md`).

### Reject

Action:

- Opens a modal or inline panel:
  - required reason enum
  - optional note
- Calls `POST /api/v1/drafts/{id}/reject`.

Reasons must match the enum in `06-human-in-the-loop-workflow.md`:

- `too_generic`
- `wrong_tone`
- `factually_incorrect`
- `off_policy`
- `language_mismatch`
- `other`

UX:

- After reject, the card moves out of “Ready To Send” / “Needs You Now” into the appropriate state (usually agent rerun queue; visible via server state).
- If backend returns “already rejected/posted”, refresh the card.

### Skip / Unskip

Action:

- `POST /api/v1/reviews/{id}/skip`
- `POST /api/v1/reviews/{id}/unskip`

UX:

- Skip requires a confirm prompt (simple `confirm()` is acceptable) because it suppresses replying.
- Unskip is immediate, with a flash message.

### Regenerate

Action:

- Open a small “hint” input (optional).
- Call `POST /api/v1/reviews/{id}/regenerate` with `{ "hint": "..." }`.

UX:

- Immediately show “Regenerating…” state.
- When new draft is ready, card updates (either user refresh or server push in future; v1 can rely on manual refresh if needed).

### Escalate (Manager-only)

Manager-only action to move an item to the owner’s queue.

Implementation note:

- Endpoint is not specified in `11-api-design.md`; if implemented, it must be added there. Until then, UI should only present escalate if the backend supports it.

### Bulk approve (Owner only)

Defined by `POST /api/v1/drafts/bulk-approve` with `{ "ids": [uuid, ...] }`.

Eligibility:

- Only 5★ drafts with **no guardrail warnings**.

Undo window:

- After bulk approve, show an **Undo** banner with 10-second countdown.
- During the undo window, posting must not begin (backend requirement from `06-human-in-the-loop-workflow.md`).

Implementation note:

- If undo is supported, it must be represented as a backend state transition; UI must not “fake undo” client-side.

## Filters, Sorting, and Tabs

Tabs and sorting must match `06-human-in-the-loop-workflow.md`:

- **Needs You Now**
  - Includes: flagged `sensitive`, `needs_attention`, or guardrail warnings
  - Sort: `created_at` ascending
- **Ready To Send**
  - Auto-drafted replies for 4–5★
  - Sort: rating desc, then `created_at`
- **History**
  - posted, rejected, skipped (including `skipped_timeout`)

UI may offer extra filters, but must never violate the tab definitions above.

## RBAC (Frontend Enforcement)

The UI must reflect the permissions model from `06-human-in-the-loop-workflow.md`:

- **Owner**: all actions.
- **Manager**:
  - Approve/edit/reject only for rating 3–5
  - Read-only for rating 1–2; those must be clearly marked as “Owner required”
  - May escalate (if supported)
- **Viewer**: read-only everywhere

Frontend must not be the only enforcement: backend is authoritative.

## Error Handling & Offline-ish Behavior

- All forms/actions must handle:
  - `401/403`: redirect to login or show “Not authorized”.
  - `409 invalid_transition`: refresh card and show latest state.
  - `429`: show “Too many requests; try again soon”.
  - `5xx`: show error banner and keep the user’s draft edits in the textarea (no data loss).
- When a mutation fails, **do not clear** user edits.

## Accessibility & UX Requirements

- Keyboard navigable: tabs, card actions, modals.
- Focus management:
  - After action, focus returns to the next actionable item in the list.
  - When opening a modal, focus is trapped; Escape closes it.
- Color is not the only signal: warnings and states must have text labels/icons.
- Content safety:
  - Review text is untrusted; UI must escape by default.

## Performance Requirements (UI)

- Queue page should render fast for small datasets (typical: < 200 reviews).
- Avoid heavy client-side bundles; use htmx partial swaps.
- Pagination or infinite scroll is optional; if implemented, must preserve sorting and filters.

## API Integration Contract (Frontend View Model)

The UI consumes `Review` objects as described in `11-api-design.md`:

- `Review.active_draft` is the primary card draft shown.
- `history` is shown on the detail page.

Where the HTML is server-rendered, the backend should map API/domain types into a stable template context (a “view model”) to avoid leaking internal schema changes to templates.

## Open Questions / Follow-ups

- Escalate endpoint shape (if not already present in API).
- Undo bulk approve endpoint/state representation.
- Whether the queue uses polling, server-sent events, or manual refresh in v1.

