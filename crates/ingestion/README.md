# ingestion

Purpose: Ingestion helpers: deduplicate incoming `Review` values, detect content updates, validate fields via `domain::validation`, and track per-platform sync metadata (`SyncState`).

Key entry points: `InMemoryDedupIndex::ingest`, `IngestOutcome`, `dedup_key`, `has_content_changed`, `SyncState` in `src/lib.rs`.

Why tests matter: Faulty dedup or change detection duplicates work, loses edits, or replays stale data. That skews SLAs, drafts, and owner notifications built on a single canonical review timeline.

Local tests:

```
cargo test -p ingestion
```
