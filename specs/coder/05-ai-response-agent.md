# 05 — AI Response Generation Agent

## Purpose

Given a unified `Review`, produce a high-quality draft reply in the voice of
the restaurant, appropriate to the rating, platform, and language, and store
it as a `ReplyDraft` for human approval.

## Model

- **Primary model**: `claude-sonnet-4-6` (fast, good quality for short replies).
- **Escalation model**: `claude-opus-4-6` for reviews flagged as sensitive
  (1-star, mentions of food safety, allergies, discrimination, or legal risk
  keywords).
- Calls are made via the Anthropic API from Rust using `reqwest`. A thin
  internal wrapper crate `llm_client` handles auth, retries, and usage logs.

## Agentic Loop

The agent is a small ReAct-style loop with bounded iterations (max 3 tool
calls per review):

1. **Classify** the review (rating bucket, language, topics, sentiment).
2. **Retrieve context** via tools as needed.
3. **Draft** a reply.
4. **Self-check** against the guardrails.
5. **Emit** the `ReplyDraft`.

### Tools Exposed to the Agent

Each tool is implemented in Rust and called by the agent via tool-use.

| Tool | Description |
|---|---|
| `lookup_menu_item(name)` | Returns canonical name, ingredients, allergens, price |
| `lookup_policy(topic)` | Returns the restaurant's policy on refunds, allergens, reservations, etc. |
| `get_past_replies(rating, limit)` | Returns recent approved replies for style priming |
| `translate(text, target_lang)` | Translates text; used when the owner wants a bilingual reply |
| `check_banned_phrases(text)` | Returns any banned phrases (legal/brand) found in the draft |

Tools return JSON and are deterministic. The agent must never invoke HTTP
endpoints directly — only through these tools.

## Prompt Composition

The system prompt is assembled from:

- **Identity & voice**: "You are the owner of *Chez Luca*, a small family
  trattoria. You speak warmly, in the first person plural ('we'), and keep
  replies short (2–4 sentences)."
- **Restaurant facts**: name, cuisine, address, opening hours, signature dishes.
- **Style guide**: do / don't examples loaded from the `past_replies` tool.
- **Platform constraints**: length limits (1000 chars Google, 500 chars UberEats).
- **Language**: match the language of the review.
- **Guardrails** (see below).

The user prompt contains the review body, rating, and any context (ordered
items for UberEats, delivery vs dine-in, etc.).

Prompts are versioned; each draft stores the `prompt_fingerprint` so
responses can be reproduced.

## Guardrails

Enforced post-generation in Rust before a draft is persisted:

- **No promises of refunds or free items** unless the review matches a
  pre-approved refund policy pattern AND the rating is ≤ 2 AND the issue is
  explicitly about food/order quality.
- **No admission of legal liability** (regex + keyword list on English and
  the restaurant's secondary language).
- **No personal data about staff** (names, phone numbers, emails).
- **No profanity, even if the review is abusive.**
- **Length**: platform-specific hard limit.
- **Language match**: the draft language must equal `body_language` unless
  the language is not one of the configured supported languages.
- **Self-reference check**: the draft must contain the restaurant name or a
  first-person-plural marker ("we", "our").

If any guardrail fails, the agent retries once with a corrective system note.
If it fails again, the draft is stored in state `pending_review` but flagged
`guardrail_warning` so the human sees the warning.

## Rating-Specific Behaviour

| Rating | Behaviour |
|---|---|
| 5★ | Brief, warm thanks. Encourage return visit. Auto-drafted. |
| 4★ | Thank + invite feedback on what could have been better. Auto-drafted. |
| 3★ | Acknowledge mixed experience, invite direct contact. Auto-drafted but flagged `needs_attention`. |
| 2★ | Apologise, invite direct contact via email. Use escalation model. Flagged `sensitive`. |
| 1★ | Same as 2★, plus notify the owner via high-priority channel. |

Reviews matching sensitive keywords (allergy, food poisoning, illness,
lawsuit, discrimination, harassment) are **always** escalated regardless of
rating.

## Observability

Every agent run records:

- input review id
- model used
- tool calls (name, arguments, result hash)
- token usage (prompt + completion)
- latency
- guardrail verdict
- output draft id

Stored in the `agent_runs` table (see `08-data-storage.md`).

## Failure Modes

- **LLM timeout / 5xx**: retry up to 3 times with backoff, then mark review
  `drafting_failed` and notify.
- **Tool error**: the agent receives the error as tool result and may try
  again or continue without that context.
- **Loop budget exceeded**: the best partial draft is saved with a
  `budget_exhausted` flag.
