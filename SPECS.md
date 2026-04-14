# SPECS — Neighbourhood Restaurant Review Agent

A unified, agentic platform for a small neighbourhood restaurant to respond to
customer reviews across Google and UberEats, with a human-in-the-loop approval
workflow. Written in Rust.

## Vision

Give a small restaurant owner a single inbox to read and reply to every public
review across all the platforms they care about. An LLM-backed agent drafts
tone-matched, context-aware replies; a human (the owner or a manager) reviews,
edits if needed, and approves each reply with one click. Approved replies are
posted back to the originating platform automatically.

## Specification Index

| # | Domain / Topic | Document | Summary |
|---|---|---|---|
| 01 | Architecture & System Overview | [specs/01-architecture.md](specs/01-architecture.md) | High-level system architecture, components, and data flow |
| 02 | Google Reviews Integration | [specs/02-google-reviews-integration.md](specs/02-google-reviews-integration.md) | Ingesting and replying to reviews via Google Business Profile API |
| 03 | UberEats Reviews Integration | [specs/03-ubereats-reviews-integration.md](specs/03-ubereats-reviews-integration.md) | Ingesting and replying to reviews via UberEats Merchant API |
| 04 | Unified Review Data Model | [specs/04-unified-review-data-model.md](specs/04-unified-review-data-model.md) | Canonical schema mapping heterogeneous sources into a single model |
| 05 | AI Response Generation Agent | [specs/05-ai-response-agent.md](specs/05-ai-response-agent.md) | Agentic drafting pipeline, prompts, tools, and guardrails |
| 06 | Human-in-the-Loop Workflow | [specs/06-human-in-the-loop-workflow.md](specs/06-human-in-the-loop-workflow.md) | Approval, edit, reject, and escalation flows |
| 07 | Notification System | [specs/07-notification-system.md](specs/07-notification-system.md) | Email / push / SMS alerts to the restaurant owner |
| 08 | Data Storage | [specs/08-data-storage.md](specs/08-data-storage.md) | PostgreSQL schema, migrations, retention |
| 09 | Authentication & Secrets Management | [specs/09-auth-and-secrets.md](specs/09-auth-and-secrets.md) | OAuth, API keys, secret storage |
| 10 | Rust Technical Stack | [specs/10-rust-tech-stack.md](specs/10-rust-tech-stack.md) | Crates, workspace layout, coding standards |
| 11 | API Design | [specs/11-api-design.md](specs/11-api-design.md) | Internal HTTP/JSON API for the web UI and webhooks |
| 12 | Observability & Logging | [specs/12-observability.md](specs/12-observability.md) | Tracing, metrics, structured logs |
| 13 | Deployment & Infrastructure | [specs/13-deployment-infra.md](specs/13-deployment-infra.md) | Hosting, CI/CD, environments |
| 14 | Testing Strategy | [specs/14-testing-strategy.md](specs/14-testing-strategy.md) | Unit, integration, end-to-end, and LLM evaluation |
| 15 | Security, Privacy & Compliance | [specs/15-security-privacy-compliance.md](specs/15-security-privacy-compliance.md) | PII handling, GDPR, platform TOS |

## Status

Draft v0.1 — specifications only. No implementation yet.
