# storage

Purpose: Persistence layer: `Repository` trait, PostgreSQL implementation (`PgRepository`, SQLx migrations), in-memory repository for tests/dev, and list-query helpers for reviews, drafts, and notification outbox.

Key entry points: `Repository`, `PgRepository`, `PgRepositoryConfig`, `InMemoryRepository`, `ReviewListQuery`, `DraftListQuery`, `NotificationOutboxItem` from `src/lib.rs`. Implementations in `repo.rs`, `pg.rs`, `memory.rs`, `list_filters.rs`.

Why tests matter: Authorization, dedup keys, draft states, and outbox delivery all depend on durable, race-safe storage. SQL or filter regressions can leak drafts across tenants, lose webhook idempotency, or double-send owner alerts.

Local tests:

```
cargo test -p storage
```
