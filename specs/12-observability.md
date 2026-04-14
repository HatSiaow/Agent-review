# 12 — Observability & Logging

## Goals

- See what the system is doing in near real time.
- Debug a specific review end-to-end by a single identifier.
- Detect silent failures (no reviews ingested for N hours) automatically.
- Keep operational overhead minimal — one small restaurant should not need
  a full SRE stack.

## Structured Logging

- `tracing` + `tracing-subscriber` with JSON output in production, pretty
  output in dev.
- Every service spans are bootstrapped at `main()` via a helper in
  `common::telemetry`.
- Log levels: default `info`; `debug` for local dev.
- Sensitive fields (OAuth tokens, password hashes, review bodies where PII
  is flagged) are tagged with a `#[tracing::field::skip]` or redacted via
  a custom layer.

### Standard Fields on Every Span

- `service` — e.g. `ingestion`, `agent`, `api`.
- `trace_id`, `span_id` — OpenTelemetry-compatible.
- `review_id`, `draft_id` — where applicable.
- `platform` — `google` / `ubereats`.
- `user_id` — for UI-initiated actions.

## Distributed Tracing

- OpenTelemetry via `tracing-opentelemetry`, exporting OTLP to whatever
  backend the deployment chooses (Tempo, Honeycomb, Jaeger, etc.).
- Sampling: head-based, 100% for error spans, 20% for successful background
  jobs, 100% for user-initiated API calls.
- Trace context propagation on outbound HTTP via `reqwest-tracing`.

## Metrics

Prometheus-format, exposed on `/metrics`. Key metrics:

| Metric | Type | Labels |
|---|---|---|
| `reviews_ingested_total` | counter | `platform`, `status` |
| `reviews_ingestion_errors_total` | counter | `platform`, `kind` |
| `agent_runs_total` | counter | `model`, `result` |
| `agent_run_latency_seconds` | histogram | `model` |
| `agent_tokens_total` | counter | `model`, `kind` (prompt/completion) |
| `drafts_state_transitions_total` | counter | `from`, `to` |
| `drafts_posted_total` | counter | `platform`, `result` |
| `notifications_sent_total` | counter | `type`, `channel`, `result` |
| `http_requests_total` | counter | `method`, `route`, `status` |
| `http_request_duration_seconds` | histogram | `method`, `route` |
| `db_query_duration_seconds` | histogram | `query` |

## Health Endpoints

- `GET /healthz` — process liveness (always `200` if the process is up).
- `GET /readyz` — checks DB round-trip and secrets backend reachability,
  returns `503` if unhealthy.

## Alerts

Shipped as a Prometheus rule file in `deploy/prometheus/alerts.yml`:

| Alert | Condition |
|---|---|
| `IngestionStalled` | `rate(reviews_ingested_total[2h]) == 0 AND hour != quiet_hours` |
| `HighIngestionErrors` | `increase(reviews_ingestion_errors_total[30m]) > 5` |
| `AgentFailureSpike` | `increase(agent_runs_total{result="error"}[15m]) > 3` |
| `PostFailureSustained` | `increase(drafts_posted_total{result="error"}[1h]) > 0` |
| `DraftBacklog` | `drafts_pending_count > 20 for 1h` |
| `SlaBreachPending` | age of oldest pending sensitive draft > 2h |

Alerts fire into the same notifier pipeline (email / SMS) as user events,
giving the owner a single place to see operational issues.

## Per-Review Debug View

The admin UI (owner only) exposes `/admin/reviews/{id}/trace` which shows:

- The raw payload as ingested.
- Every `agent_run` for this review with prompt fingerprint, tool calls,
  token counts, and guardrail verdict.
- Every draft revision.
- Every audit event.
- Every posting attempt and error.

This view is the single source of truth when the owner asks "why did the
bot say *that*?".

## Log Retention

- Local file logs: 14 days rotated daily.
- Remote aggregator (if configured): 30 days.
- Traces: 7 days.
- Metrics: 90 days.
