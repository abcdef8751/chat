-- Supabase Storage setup for large / binary chat attachments.
--
-- Run this in the Supabase SQL editor (Dashboard → SQL Editor → New query),
-- once per project, *after* supabase/schema.sql.
--
-- Why: an attachment with nothing inline to send (text past 1 MB, an image past
-- 10 MB, a PDF/zip/other binary) is uploaded to this private bucket instead of
-- being silently dropped, and the model reaches it with the `fetch_file` tool.
--
-- Object layout: `<user_id>/<uuid>-<sanitized name>`, e.g.
--   8f14e45f-.../0d3a…-report.pdf
-- The first path segment is the owner, and every policy below compares it with
-- `auth.uid()::text`, so a user can only read/write their own prefix. The
-- bucket is private: objects are only reachable with a signed-in JWT.

insert into storage.buckets (id, name, public)
values ('attachments', 'attachments', false)
on conflict (id) do nothing;

-- One policy per operation, all keyed on the owner prefix. `(storage.foldername(name))[1]`
-- is the first path segment, which is the uploader's user id.
drop policy if exists "attachments: owner can read" on storage.objects;
create policy "attachments: owner can read"
  on storage.objects for select
  to authenticated
  using (
    bucket_id = 'attachments'
    and (storage.foldername(name))[1] = auth.uid()::text
  );

drop policy if exists "attachments: owner can insert" on storage.objects;
create policy "attachments: owner can insert"
  on storage.objects for insert
  to authenticated
  with check (
    bucket_id = 'attachments'
    and (storage.foldername(name))[1] = auth.uid()::text
  );

-- Covers upsert-style writes (the client uploads with `x-upsert: false`, so this
-- is only a safety net) and any future replace-in-place flow.
drop policy if exists "attachments: owner can update" on storage.objects;
create policy "attachments: owner can update"
  on storage.objects for update
  to authenticated
  using (
    bucket_id = 'attachments'
    and (storage.foldername(name))[1] = auth.uid()::text
  )
  with check (
    bucket_id = 'attachments'
    and (storage.foldername(name))[1] = auth.uid()::text
  );

drop policy if exists "attachments: owner can delete" on storage.objects;
create policy "attachments: owner can delete"
  on storage.objects for delete
  to authenticated
  using (
    bucket_id = 'attachments'
    and (storage.foldername(name))[1] = auth.uid()::text
  );
