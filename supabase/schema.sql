-- Pi Chat — Supabase schema (backup + sync)
--
-- Run this in the Supabase SQL editor (or via the local admin script) AFTER
-- creating the project. Everything is namespaced by the signed-in Supabase user
-- via RLS. Conversation ids are client-generated UUIDs matching local SQLite;
-- message ids are text (`<conversation_id>:<index>` or a fresh uuid) — see the
-- `messages` table comments — so rows map 1:1.
--
-- All syncable rows carry:
--   revision  BIGINT NOT NULL — monotonic per-row version (bumped on every
--                               upsert AND on tombstone). Monotonicity is
--                               enforced by the trigger below, so a client
--                               revision is stored verbatim (it can never go
--                               backwards because the pull high-water mark only
--                               advances forward).
--   deleted_at BIGINT        — soft-delete tombstone (NULL = live). Keeps a
--                              deletion on one device from being resurrected by
--                              another device's pull.
--   user_id    uuid default auth.uid() — RLS ownership.

create table if not exists conversations (
  id uuid primary key,
  title text not null,
  model text,
  system_prompt text,
  compaction_summary text,
  last_reflected_index bigint,
  imported boolean not null default false,
  import_batch bigint not null default 0,
  created_at bigint not null,
  updated_at bigint not null,
  revision bigint not null,
  deleted_at bigint,
  user_id uuid not null default auth.uid()
);

create table if not exists messages (
  -- Message ids are NOT UUIDs: live-chat turns use a fresh uuid, but imported
  -- conversations use deterministic "<conversation_id>:<index>" ids (so a
  -- re-import maps 1:1 and sync stays idempotent). Hence `text`, not `uuid`.
  id text primary key,
  conversation_id uuid not null references conversations(id) on delete cascade,
  role text not null,
  -- NOT the cross-device ordering authority; see the "index strategy" in the
  -- design. Local devices re-index their messages by (created_at, id) on pull.
  seq bigint not null,
  content text not null,
  model text,
  provider text,
  thinking_level text,
  thinking text,
  usage jsonb,
  stop_reason text,
  attachments jsonb,
  created_at bigint not null,
  revision bigint not null,
  deleted_at bigint,
  user_id uuid not null default auth.uid()
);

create table if not exists memory_files (
  path text not null,            -- e.g. 'core/profile.md'
  content text not null,
  created_at bigint not null,
  updated_at bigint not null default 0,
  revision bigint not null,
  deleted_at bigint,
  user_id uuid not null default auth.uid(),
  primary key (user_id, path)
);

-- Row level security: a client (anon key) may only ever touch its own rows.
alter table conversations enable row level security;
alter table messages enable row level security;
alter table memory_files enable row level security;

drop policy if exists "owner rw" on conversations;
create policy "owner rw" on conversations
  for all using (auth.uid() = user_id) with check (auth.uid() = user_id);

drop policy if exists "owner rw" on messages;
create policy "owner rw" on messages
  for all using (auth.uid() = user_id) with check (auth.uid() = user_id);

drop policy if exists "owner rw" on memory_files;
create policy "owner rw" on memory_files
  for all using (auth.uid() = user_id) with check (auth.uid() = user_id);

-- Store the client-provided revision verbatim on every insert/update, and only
-- ever move it forward. This is what makes client-side LWW work: the receiving
-- device keeps the highest revision it has seen and the trigger guarantees the
-- stored value never regresses across merged upserts.

create or replace function enforce_revision()
returns trigger
language plpgsql
as $$
begin
  if TG_OP = 'INSERT' then
    if new.revision is null then
      new.revision := 0;
    end if;
    return new;
  end if;
  -- UPDATE: never let revision move backwards.
  if new.revision < old.revision then
    new.revision := old.revision;
  end if;
  return new;
end;
$$;

drop trigger if exists trg_conversations_revision on conversations;
create trigger trg_conversations_revision
  before insert or update on conversations
  for each row execute function enforce_revision();

drop trigger if exists trg_messages_revision on messages;
create trigger trg_messages_revision
  before insert or update on messages
  for each row execute function enforce_revision();

drop trigger if exists trg_memory_files_revision on memory_files;
create trigger trg_memory_files_revision
  before insert or update on memory_files
  for each row execute function enforce_revision();

-- Convenience indexes for the pull query shapes.
create index if not exists messages_conv_idx on messages(conversation_id);
create index if not exists messages_revision_idx on messages(revision);
create index if not exists conversations_revision_idx on conversations(revision);
create index if not exists memory_files_revision_idx on memory_files(revision);
