-- Add a durable claim marker to the notifications outbox.
--
-- The previous approach used `SELECT ... FOR UPDATE SKIP LOCKED` without a surrounding
-- transaction that stays open while sending. That allowed duplicates across workers because
-- locks were released immediately after the query.

alter table notifications_outbox
  add column if not exists claimed_at timestamptz null,
  add column if not exists claimed_by text null;

create index if not exists notifications_outbox_claimed_at_idx
  on notifications_outbox (claimed_at);

