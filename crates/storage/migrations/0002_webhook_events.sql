-- Webhook replay protection (at-least-once delivery).
--
-- We keep a de-duplication record for (platform, event_id) so that webhook
-- retries/replays don't double-ingest reviews. Implementations should retain
-- rows for at least 24 hours (GC job can prune older rows).

create table if not exists webhook_events (
  platform text not null,
  event_id text not null,
  received_at timestamptz not null,
  primary key (platform, event_id)
);

create index if not exists webhook_events_received_at_idx
  on webhook_events (received_at desc);

