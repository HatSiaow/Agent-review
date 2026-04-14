-- Single-row restaurant profile (singleton id = 00000000-0000-0000-0000-000000000001).
create table if not exists restaurant_settings (
  id uuid primary key,
  payload_json jsonb not null default '{}'::jsonb,
  updated_at timestamptz not null default now()
);

insert into restaurant_settings (id, payload_json)
values ('00000000-0000-0000-0000-000000000001'::uuid, '{}'::jsonb)
on conflict (id) do nothing;
