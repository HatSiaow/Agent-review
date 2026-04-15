# llm-client

Purpose: Abstraction for LLM-backed reply drafting: request/response types, `LlmClient` trait, deterministic `InMemoryLlm` for tests, and `AnthropicClient` for production calls with timeouts and retries.

Key entry points: `LlmClient`, `GenerateRequest`, `GenerateResponse`, `ModelTier`, `LlmConfig`, `AnthropicClient`, `InMemoryLlm` in `src/lib.rs`.

Why tests matter: Draft quality, prompt fingerprinting, and token metadata feed guardrails and audit. Serialization and tier handling bugs can leak wrong model usage or break reproducibility when disputes arise about automated replies.

Local tests:

```
cargo test -p llm-client
```
