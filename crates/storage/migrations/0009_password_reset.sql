-- Password reset tokens for one-time use account recovery.
-- Tokens expire after 30 minutes and are single-use (deleted on use).

create table if not exists password_reset_tokens (
    id uuid primary key,
    user_id uuid not null references users(id) on delete cascade,
    token_hash text not null unique,  -- SHA-256 hex of the token
    created_at timestamptz not null default now(),
    expires_at timestamptz not null,
    used_at timestamptz null
);

create index if not exists password_reset_tokens_user_idx
    on password_reset_tokens (user_id);
create index if not exists password_reset_tokens_expires_at_idx
    on password_reset_tokens (expires_at);
