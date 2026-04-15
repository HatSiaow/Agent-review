-- Helper indexes for efficient GC operations.
-- The GC job performs time-based deletes which need covering indexes.

create index if not exists agent_runs_created_at_idx
  on agent_runs (created_at);

create index if not exists notifications_outbox_sent_at_gc_idx
  on notifications_outbox (sent_at)
  where sent_at is not null;

create index if not exists webhook_events_received_at_idx
  on webhook_events (received_at);
