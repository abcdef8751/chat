User instructions:

- Preferably don't read the messages in the database
- If you need to update the database schema, use the `supabase` CLI, it's logged in

# AGENTS.md — Pi Chat

This is the single working document for **Pi Chat**, a small desktop AI chat app:
what it is, how it's built, where things are, and what's next. (The old
`PLANS.md` product spec was folded in here — the sections below are the source of
truth for both architecture and status.)

---

## What it is

A general-purpose AI chat app (not a coding agent) with:

- An expandable sidebar listing past conversations (search, rename, delete)
- Streaming assistant replies over any OpenAI-compatible endpoint
- Optional tools — `bash` (a **one-shot per-call shell**), `read_file`,
  `write_file` (all approval-gated), and Brave's native search tools
  (`brave_web_search`/`local`/`image`/`video`/`news`/`summarizer`, ungated)
- `fetch_image(url)` — the assistant can attach a web image to its reply
  (ungated; raster-only; persisted on the message, so it survives reload)
- Reasoning traces from thinking models (inline "Thought" container → popup),
  interleaved with the answer as it streams
- Per-turn **activity container**: one collapsible per turn, labelled
  `Thought · N tools`, holding the reasoning button(s) (which open the trace
  popup — the raw trace never renders inline), the tool cards, and the interim
  text. It opens itself while the turn streams and collapses when it finishes;
  the final answer renders below it. Interim text lives *inside* it rather than
  between the answer's segments because some models reuse their pre-tool
  narration as a draft and restate it in the answer (the system prompt also asks
  them not to, but that is only a nudge)
- LaTeX math (`$…$` / `$$…$$`) rendered with KaTeX
- Click any transcript image to expand it centred on screen
- Plain-Markdown long-term **memory** curated by an idle reflection pass
- Context + price counters (per-turn cost is frozen at the model that produced
  it), with older history compacted into a summary once a chat nears the window
- File attachments (native picker + drag-drop; images as vision parts, documents
  folded into the prompt)
- Dark mode by default (header toggle)

**Stack:** SolidJS + Tailwind v4 + Kobalte + Tauri v2 (Rust). Backend logic lives
in `src-tauri`; the UI is a Solid SPA.

**Guiding principles & key stances**

- **Small and self-contained.** One Tauri binary, one SQLite file, a directory of
  Markdown files for memory. No microservices.
- **No agent framework.** The core loop is simple and implemented directly in
  Rust. pi is a *pattern reference only* — we don't import or vendor it. We reuse
  `async-openai` for LLM types, and call Brave Search + `web_fetch` natively over
  `reqwest`.
- **The model decides; the user controls.** Tools and memory writes are surfaced,
  gateable, and auditable.
- **Separate storage size from injected context size.** We can store a lot; we
  inject a small, bounded subset at prompt time.
- **Generic OpenAI-compatible client.** Fireworks is only a prefilled default;
  nothing is hardcoded to a provider. Secrets stay in the OS keychain and never
  cross the IPC boundary.

---

## Current status

`cargo test --lib`: **311 passed, 3 ignored** (the opt-in live reasoning-echo
check, the real-export import, and the real models.dev catalog parse). Clippy,
`tsc --noEmit`, and `npm run build` are clean. The Android target also
type-checks (`cargo check --target aarch64-linux-android`, NDK clang wired via
`CC_aarch64_linux_android` etc.). The GUI e2e needs a display and is run
separately — it passes end to end (`ALL ASSERTIONS PASSED`).

| Area              | State | Notes                                                                                                     |
| ----------------- | ----- | --------------------------------------------------------------------------------------------------------- |
| Stack / scaffold  | ✅    | SolidJS + Tailwind v4 + Kobalte + Tauri v2; SQLite via rusqlite (bundled)                                  |
| Streaming loop    | ✅    | Raw SSE parse (keeps provider `reasoning_content`), deltas, abort, capped tool loop                        |
| Config & secrets  | ✅    | OS keychain; models.dev catalog with `/models` fallback; thinking levels; per-model price **and capability (modality) overrides** |
| Conversations     | ✅    | Sidebar, search, rename/delete, auto-title, attachments, per-turn timeline, **editable user messages** (edit → truncate → re-run) |
| Tools             | ✅    | `bash` (one-shot shell), `read_file` (text **and image-aware**)/`write_file` (gated; **optional YOLO auto-approve**), memory tools incl. `memory_str_replace`, native Brave search tools + `web_fetch` + `fetch_image` (ungated) |
| UI shell          | ✅    | `App.tsx` split into `src/components/*`; mobile drawer + full-screen memory pages; hardware back stack; memory page list→detail on narrow viewports |
| Memory            | ✅    | Plain Markdown, core files + agent-grown files, idle reflection, separate reflection cost, parallel backfill |
| Cron jobs         | ✅    | Scheduled runs append to a thread; per-job tool policy; catch-up max 1; OS notification (desktop) + badge |
| Cloud sync        | ✅    | Opt-in Supabase backup+sync: email/password GoTrue auth, PostgREST push/pull, LWW by revision + tombstones, memory files, optional client-side encryption |
| Context + price   | ✅    | Context counter, frozen per-turn cost, near-limit note, forced compaction (M7)                            |
| GUI e2e           | ✅    | `npm run e2e` — mock LLM + WebKitWebDriver (streaming, timeline, tools, reflection)                        |

---

## Core loop (trimmed)

`chat.rs` `run_tool_loop` (shared by live chat and reflection):

```
prompt(userMessage)
├─ build context (system + preferences + memory + [compaction summary] + verbatim history)
├─ if the request is over 80% of the window: fold older history into the summary first
├─ stream deltas ─────────────────────────── emit `delta` / `thinkingDelta` events
├─ if assistant emits tool calls:
│   ├─ gate each call (approve / deny / abort) — skipped when auto_approve_tools
│   ├─ execute tool
│   ├─ on error: return a tool-error result (model self-corrects)
│   └─ loop back to the LLM (MAX_TOOL_ROUNDS = 8)
├─ persist message + usage + stopReason
└─ emit final message event
```

Guards: capped tool rounds; idempotent abort that preserves partial text; stream
errors keep partial text and mark `stop_reason = "error"`. Within a turn, each
tool-call round's reasoning is echoed on its assistant tool-call message for the
following rounds (DeepSeek thinking mode requires it, else HTTP 400); traces are
**never** replayed across user turns. Toggle: `AppConfig.echo_reasoning_content`.

**Edit → re-run.** `db::edit_user_message` rewrites a `user` row and deletes
every later row in the conversation (recording per-message tombstones so the
truncation propagates), then the frontend re-streams. That re-stream passes
`regenerate: true`, which makes `run_chat_turn` build the turn from `content`
without inserting a second `user` row and drops the already-persisted copy from
the in-memory history. `regenerate: false` is the ordinary send path.

**YOLO mode.** `AppConfig.auto_approve_tools` (Settings toggle) auto-approves
gated tools: the approval wait is skipped and the `ToolCall` event is emitted
with `gated: false`, so the UI never shows the pending approve/deny row. Local
data only (not synced).

**`read_file` images.** A raster image (png/jpg/gif/webp/bmp; SVG/ICO excluded;
capped at `MAX_IMAGE_BYTES`) read by `read_file` is delivered to the model as an
**image part** when the turn's model is vision-capable — and the vision check is
override-aware (`resolve_capabilities`: user override → models.dev → text-only
default). Because async-openai's tool messages are text-only, the image rides a
synthetic **user** message appended *after* the round's tool results (so parallel
tool calls stay adjacent to their results); it is request-scoped and never
persisted. A non-vision model gets a plain text result saying it cannot view
images.

**Failure-handling contract**

- Stream error mid-turn → keep partial text, `stop_reason = "error"`, offer retry.
- Tool error → convert to a tool-error result so the model can self-correct; never throw.
- Abort → cancels the in-flight request/tool dispatch immediately via a
  race-free `CancelToken` (`StreamRegistry`), then closes the stream and persists
  the partial; idempotent.
- Runaway guard → hard cap on consecutive tool rounds.

---

## Clock updates

The model is told the time by a **`clock` row written immediately before each
user turn** — a real `messages` row, hidden from the timeline. The **system
prompt is left untouched**.

- **Stored, not injected.** A *transient* note is the wrong shape for prompt
  caching: it sits at the end of one request but not the next, so the sequence
  shifts at its insertion point and every turn has to re-send the previous
  exchange — uncached — from there on. A stored row makes each request an exact
  extension of the previous one, so the provider's prefix cache covers the whole
  conversation. A note is ~25 tokens (~100 chars) of cached context per turn.
- **Why every turn.** An earlier version only injected a note when **≥5 minutes**
  had passed since the last update or the UTC date had rolled over. Because
  nothing was stored, each note vanished with the turn that injected it, so every
  turn in between carried **no clock at all** and the model could answer "I don't
  know the current time" seconds after being told. Always writing one removes
  both the gap and the staleness.
- **Shape.** UTC time + weekday, the device's local wall time, and the offset:
  `Current time: 2026-10-16 14:30 UTC (Friday). The user's local time is 2026-10-16 17:30 (UTC+03:00).`
  The offset is read fresh from the OS every turn (`chrono::Local`, DST-aware —
  the one place chrono is used), so a DST change or a move to another timezone
  shows up immediately, and the local time is spelled out so the model never does
  the arithmetic (or gets the sign wrong). The date rides every note because chat
  history carries no timestamps.
- **Bookkeeping, not a turn.** `clock` rows are local-only like `memory` notes:
  never synced (revision 0 / dirty 0), no `updated_at` bump (so a note can't reset
  the reflection idle timer or reorder the sidebar), skipped by `groupTurns` in
  the timeline, and filtered out of `summarize_history`'s fold. On the
  `regenerate` path (the edited user row already exists) the note is stored just
  after it; every other turn stores it before.
- **Replay.** `build_history_messages` surfaces `clock` rows as `system`
  messages, so live and reflection build the same prefix. Reflection gets no
  *fresh* note (`build_context` directly) — only the stored ones.

The system prompt's `This conversation started on <date> (<Weekday>) UTC.` line
carries its weekday too.

---

## Context compaction

Keeps a long conversation inside the model's window without losing the raw
record. When a live turn's request would reach **80%** of the resolved window
(`COMPACTION_THRESHOLD`), the turn runs a compaction pass first, then builds the
request from the result.

- **Layout:** exactly `system prompt → compaction summary (a system message) →
  last ~15k tokens of history verbatim → the new turn` (`COMPACTION_KEEP_TOKENS`).
  The summary comes from a single tool-less summarization round.
- **Incremental:** the pass folds the *previous* summary plus only the messages
  since the last watermark, so a long chat is never re-summarized from scratch
  and the summarizer call stays bounded.
- **Boundary:** `conversations.compaction_through` stores the **id** of the
  newest folded message — never an index, because cross-device pull renumbers
  `index`. Messages after it are sent verbatim. The kept tail never begins on a
  `tool` row, so a tool result is never orphaned from its call.
- **Storage:** raw messages are **never deleted**; compaction changes only what
  is *injected*. The summary + boundary live on the conversation row.
- **Cost:** each pass is recorded in `memory_reflections` (background
  maintenance spend, kept out of chat cost), like a reflection run.
- **UI:** a non-blocking note above the composer past 80%, a `compacted` chip in
  the header meter, and a line in the meter tooltip.

**No race with memory consolidation.** Reflection and compaction both read the
transcript and write maintenance state, so their interlock is explicit:

1. Compaction runs *inside* the live turn, which is registered in
   `StreamRegistry` — so the reflection scheduler skips that conversation and an
   in-flight reflection's watcher aborts it. The two never run at once.
2. They write **disjoint columns**, one locked statement each: `set_compaction`
   owns `compaction_summary`/`compaction_through`; `set_last_reflected_index`
   owns `last_reflected_index`. Compaction never touches `updated_at`, so it
   cannot reset the reflection idle timer or reorder the sidebar.
3. Raw messages are never deleted, so a reflection reading the transcript at any
   moment sees valid data.
4. Reflection reads its summary + boundary from the **same conversation snapshot**
   as its history, so an interleaved compaction write cannot desync them (a stale
   boundary only widens the verbatim range, never drops context).

The per-conversation reflection pass uses the **same compacted view** as a live
turn, so the shared prefix stays prompt-cacheable and the pass stays bounded on a
long chat. The backfill's extraction (map) phase deliberately keeps the **raw**
history — it wants every fact, not the gist.

`compaction_through` is **local-only** (not synced; it needs no Supabase schema
change). `compaction_summary` still rides the conversation row. Because the two
halves have different lifetimes, the resolver requires **both**: a summary
without its boundary, or a boundary whose summary was nulled by a sync pull's
last-writer-wins, both make the compaction be ignored (full history sent) rather
than guess a split that could drop context.

---

## Data model (SQLite)

Schema lives in `db.rs` `SCHEMA` (idempotent `CREATE TABLE IF NOT EXISTS`), with
best-effort `ALTER TABLE ... ADD COLUMN` migrations in `open()`.

```sql
conversations(
  id, title, model, system_prompt, compaction_summary,
  compaction_through,            -- id of the newest message folded into the summary (local-only)
  last_reflected_index,          -- memory-reflection watermark
  imported,                      -- 1 = came from an export; backfill-only
  created_at, updated_at)

messages(
  id, conversation_id, role,     -- user | assistant | tool | memory | clock
  "index",                        -- per-conversation monotonic order
  content, model, provider, thinking_level, thinking,
  usage,                          -- JSON token/cost (frozen `cost` per turn)
  stop_reason, attachments,       -- JSON attachment array
  created_at)

model_prices(                     -- models.dev cache
  provider, model_id, input_per_million, output_per_million,
  cache_read_per_million, cache_write_per_million, context_window,
  name, reasoning, reasoning_options, attachment, modalities,
  fetched_at, PRIMARY KEY(provider, model_id))

memory_reflections(               -- reflection spend, kept separate from chat cost
  id, conversation_id, model, usage, cost, files, note, created_at)

memory_extractions(               -- staged backfill summaries (map-phase output)
  conversation_id PRIMARY KEY, watermark, model, usage, cost, payload,
  folded,                         -- 1 = already folded into memory (resume)
  created_at)

memory_backfill_runs(             -- reduce-pass spend (spans many conversations)
  id, model, usage, cost, files, note, created_at)

cron_jobs(                        -- local-only scheduled runs
  id, conversation_id REFERENCES conversations(id) ON DELETE CASCADE,
  title, schedule, prompt, enabled, tool_policy, notify,
  next_run_at, last_run_at, run_count, run_requested, created_at)

cron_runs(                        -- per-fire audit trail
  id, job_id REFERENCES cron_jobs(id) ON DELETE CASCADE,
  conversation_id, trigger_message_id, scheduled_for,
  started_at, finished_at, stop_reason, error, usage, cost)
```

- **`role`:** `memory` rows are consolidation notes. `build_history_messages`
  surfaces them to the model as lightweight `system` messages (so they're part of
  the cacheable prefix in both live and reflection turns), and the UI renders them
  as a centered note, not a turn. `clock` rows are the per-turn time note (see
  "Clock updates"): same treatment, but the timeline skips them entirely. Both
  roles are **local-only bookkeeping** — never synced, never bumping the
  conversation's revision or `updated_at`.
- **Tool-turn persistence:** assistant tool-request rows are
  `{"tool_calls":[{id,name,arguments}]}`; tool-result rows (`role="tool"`) are
  `{"tool_call_id","name","error","output"}`. Frontend parsers in `App.tsx`
  (`parseToolCallList`/`parseToolResult`) and `groupTurns`/`buildEntries` must stay
  in sync.
- **Pricing** is resolved lazily and cached: **override → fetched `model_prices` →
  bundled fallback → default**. Cost is frozen on the message at the rates in
  effect, so switching models never re-prices past turns.
- **Capabilities** are resolved **user override → models.dev cache → default**
  (`pricing::resolve_capabilities`): `ModelOverride.{input_modalities,
  output_modalities, reasoning}` beat the cached `attachment`/`modalities`/
  `reasoning` from models.dev, which beat the defaults (**text in, text out,
  reasoning supported**). The override is the *only* way to declare capability
  for a manually-added provider, which never gets a models.dev entry (see
  "models.dev lookup" below). `ModelInfo` exposes the raw catalog values
  (`reasoning`/`attachment`/`modalities`) plus a resolved `vision` bit; the
  composer's image warning re-resolves vision **client-side** from the override
  plus those raw values (`src/lib/capabilities.ts`, mirroring
  `resolve_capabilities`) rather than reading the baked-in `vision` bit, which
  goes stale the moment a capability is toggled in Settings (the model list
  isn't refetched then — so a model just marked image-capable no longer warns).
  The Settings "Capabilities" card edits the override by click-to-toggle. A
  model with no override and no catalog modalities resolves to text-only, so an
  un-annotated custom model is warned about images — as long as it is in the model
  list at all — until you mark it `image`-capable.
- **models.dev lookup.** The catalog is one public JSON blob
  (`https://models.dev/api.json`), matched to a provider **by base URL** (exact
  `api` first, then host). But the fetch is **gated on `catalog_id`**: only a
  provider added via the "Add provider" catalog picker ever consults models.dev;
  a manual/custom endpoint lists via its own `GET /models` (which carries no
  capability metadata), so its models get the defaults above. Cache rows in
  `model_prices` are keyed by the normalized base-URL string (`provider_key`),
  not by catalog id.

  **Saving a provider's key refetches its model list immediately**
  (`doUpdateProvider` → `loadModels(true, id)`). Without that the picker stayed
  empty until a restart, because nothing else changes when a key is added:
  `closeSettings` only refreshes when the *endpoint* or the **Brave** key changed
  (`persistSettings` reads `braveKeyDraft`, not the provider key), and the
  header-sync effect deliberately early-returns unless the active
  conversation/provider changed.

---

## Memory system

Plain Markdown files in the app data dir — **no frontmatter, no index, no inbox**.
The system starts with only the **core** files and the agent grows it with its own
topic files.

- **Core files** (`profile.md`, `preferences.md`, `goals.md`) are injected into
  every system prompt **in full**. Other files are listed by name + first line and
  read on demand.
- **Tools:** `save_memory(content, path)` appends to the named file (path
  required, file created if missing); `read_memory(path)`; `write_memory(path,
  content)` replaces a file for curation; `memory_str_replace(path, old_string,
  new_string)` replaces a single unique occurrence (errors — leaving the file
  untouched — on zero or multiple matches). No `importance`/`category`. The
  shared memory instruction tells the model to `read_memory` the relevant file
  before editing it.
- **Live vs idle:** the tool list is identical for live, reflection, and
  extraction turns (so the prompt-cache prefix matches). `run_tool_loop`'s
  `ToolMode` enforces access by mode: `Live` and `Reflection` may use all four
  memory tools; `Reflection` is refused host/web tools; `Extract` advertises no
  tools and refuses any call. (Live memory writes were once blocked to keep the
  chatting model from clobbering curated files; the list stays unified, so
  access is enforced by mode at dispatch rather than by splitting the list.)
- **Idle reflection** (`reflection.rs`): a background scheduler (`spawn`, ~1 min
  tick) reflects one due conversation per tick once it has been idle ≥
  `memory_reflection_idle_minutes` (default 30, toggle in Settings). It rebuilds
  the **same leading system prompt as a live turn** + the full transcript (so the
  shared prefix stays prompt-cacheable), then appends a **trailing system
  message** carrying the maintenance task and the most recent consolidation notes
  (across chats, so it doesn't repeat work). It runs the reflection tool loop,
  diffs the files, records usage/cost, inserts a `memory` note when files changed,
  and advances `conversations.last_reflected_index`. The model reads non-core
  files on demand via `read_memory` before rewriting them.
- **Model:** the pass resolves the **conversation's own provider + model**
  (`reflect_conversation`), not the global `base_url`/`model` mirrors, and the
  scheduler must not bail out when the *global* model is blank — eligibility is
  per conversation. The **thinking level** is the conversation's own as well: a
  live turn records the level it actually ran at on every row it writes
  (`messages.thinking_level`, previously always `NULL`), and the pass resolves the
  conversation's newest recorded level, falling back to the device's configured
  level for chats that predate the recording. The exception is the backfill's
  reduce pass, which
  stays on the active provider's default model for a whole run (`active_turn_cfg`).
- **Eligibility:** a chat becomes due only when a message is inserted
  (user/assistant/tool). Merely opening a chat does not touch `updated_at`.
- **Scope:** the Memory tab's **Consolidate this chat** button is per-chat; the
  scheduler sweeps all chats over time. Memory files themselves are global.
- **Explicit vs inferred preferences:** the Settings **"Explicit user preferences"**
  field is injected at the top of every system prompt and labelled authoritative
  (takes precedence over inferred memory); the memory block and reflection prompt
  both say not to duplicate or contradict it. Memory files hold what the assistant
  *infers*.
- **Import** (`import.rs`): reads an Anthropic-format export — a single
  `conversations.json` holding an array of conversations (bulk/directory exports
  are not supported yet). Two properties make it safe over a large archive.
  **Timestamps are preserved**: nothing goes through `insert_message_full`, which
  would stamp
  `now` and re-title the chat, so imported chats sort into the sidebar by their
  real dates via the existing `ORDER BY updated_at DESC`. And **imported chats
  are excluded from the idle sweep**: `mark_imported_reflected` sets their
  `last_reflected_index` to their newest message, so the 60s-tick reflection
  never wanders into an archive. The export flattens a whole tool round-trip into
  one assistant message, so `rows_for_message` splits on block boundaries back
  into the app's native `assistant(text)` → `assistant({"tool_calls":…})` →
  `tool({…})` rows, which is what makes imported tool calls render in the
  activity timeline. Re-importing is a no-op (`INSERT OR IGNORE`), not a clobber.
  Each conversation is written in **one transaction**, so an interrupted import
  can never leave a chat holding half its messages — which matters because a
  re-import skips conversations that already exist, so a half-written chat would
  stay half-written forever. It is also ~2.5× faster than a statement per row,
  since each of those would otherwise be its own WAL commit. `ImportState`
  guards against two imports running at once.
  **Attachments cannot be imported**: the export carries only `{file_uuid,
  file_name}` references and the accompanying zip holds nothing but the JSON, so
  `rows_for_message` appends an `[attached: … — contents are not included in the
  export]` marker instead of silently dropping the context.
- **Backfill** (`reflection.rs`): restricted to **imported** conversations, and
  among those to the **newest import batch** — `conversations.import_batch` is
  stamped once per import run (`db::next_import_batch`), so importing a second
  archive never re-sweeps the first one (an older batch that was never finished
  is left alone). Chats the user actually had are the sequential pass's job.
  Reflecting over an
  imported archive one conversation per 60s tick is both slow (~20h for 1200
  chats) and unsafe to parallelize naively, because every pass is a
  read-modify-write on the same Markdown files and `reflection_tail`'s "recent
  notes" only deduplicates because passes are serial. So it splits in two. The
  **map** phase runs many extraction passes concurrently (`ToolMode::Extract`), staging a plain-text
  summary per conversation in `memory_extractions` and never touching memory.
  Concurrency is under **AIMD**, not a fixed number: Fireworks enforces adaptive
  TPM limits (not a concurrency cap) whose ceiling depends on account tier and
  model size tier, so any fixed value is either too timid or over-drives into
  sustained 429s. It starts at `DEFAULT_CONCURRENCY` (8), grows one permit per
  full window of clean completions (additive in round-trips, not requests —
  growing per request would jump from 8 to 700 in seconds), and halves on a
  throttle down to a floor of 1, capped at 64. Only *transient* failures back
  off: a 400 or a bad model must not collapse concurrency for the whole run. The
  live limit is reported in `BackfillStatus.concurrency`.
  Extraction advertises **no tools at all** — it is pure summarization, which
  also drops the tool rounds so the map phase is one request per conversation
  instead of ~3; the `Extract` gate is only a guard against a hallucinated call.
  It also gets a **minimal task-specific system prompt**, not the live-turn one:
  the live prompt documents memory tools extraction does not advertise, gives
  routing guidance that belongs to the reduce, and inlines every core memory
  file — ~1.9k tokens irrelevant to "list the durable facts in this transcript",
  paid once per conversation (28% of the map phase's prompt volume). Extraction
  is deliberately blind to current memory: the map recalls, the reduce dedupes,
  and showing the extractor what is already known invites it to omit facts.
  A conversation with nothing durable yields the literal sentinel `NOTHING`,
  which `is_nothing` normalizes to an empty payload — it is *not* an empty
  string, so without that the reduce would be handed a "summary" reading
  `NOTHING`. Empty payloads are retired as folded without a reduce pass.
  The **reduce** phase is a single serial writer that folds those summaries into
  memory through the ordinary reflection tool loop. It is **one pass by
default**; it only chunks when the payload exceeds `CONTEXT_BUDGET` (80%) of the
  selected model's context window, and then in chronological order. Each pass
  after the first is told the date memory already covers, because memory entries
  are undated prose and that is the only thing making "later wins" work across
  passes. Conversations under `DEFAULT_MIN_CHARS` (200) are skipped. That floor
  is a *noise* filter, not a cost control: it was 2000 on the theory that short
  chats are most of the passes but a rounding error of the content, but the
  passes are cheap and the short tail is not empty of signal — a 1.9k-char chat
  about audio gear named the user's existing IEMs and their EQ habit. 200 drops
  the "." and "hm" conversations and keeps everything else. A failed map pass just
  leaves that conversation un-extracted (no partial memory); the staging area is
  only cleared when every reduce pass lands. Every pass is wrapped in
  `with_retry`: Fireworks enforces *adaptive* token-per-minute limits rather than
  a concurrency cap, and its docs are explicit that ramping up too quickly draws
  429s, so a cold burst of extraction passes must back off rather than record
  permanent failures. `is_transient` retries 429/503/timeouts and lets a 400 or
  an abort fail immediately. **Both phases run on the active provider, not the
  top-level `base_url`/`model` mirrors.** A backfill has no conversation to pin,
  so `active_turn_cfg` resolves the *active* provider's base URL and its default
  model (falling back to `cfg.model`), rather than trusting the `AppConfig`
  `base_url`/`model` fields — those mirrors are persisted separately from
  `providers[]` and can drift stale against the active provider, which would send
  the default model to an old endpoint and surface as a provider-side "model not
  found" 404. The ordinary per-conversation reflection pass already resolves each
  chat's own provider; backfill just does the same for the active one.
- **Interruption is resumable in both phases.** The map phase is watermark-based
  (`memory_extractions.watermark` vs the newest message), so a cancel, crash, or
  failure leaves it consistent and a re-run skips exactly what is already staged.
  The reduce phase records progress per batch: `list_extractions` returns only
  `folded = 0` rows, each batch is marked folded as soon as its pass lands, and
  `folded_through` recovers the fold date so "later wins" survives a restart.
  A re-run therefore continues at the first unfolded batch instead of re-folding
  everything — which would both re-pay for finished work and, because
  `memory_through` would reset to `None`, silently lose the date hint that makes
  cross-batch contradiction resolution work. `upsert_extraction` resets `folded`
  on conflict, so a conversation that grows new messages is consolidated again.
  **Folded rows are never deleted** — they are the extraction watermark. Clearing
  them at the end of a successful run would make every conversation look pending
  again, so the next run would redo the whole archive; and it would erase the
  distinction between conversations that succeeded and the ones that *failed*
  (which leave no row at all, and so stay pending — exactly the set a re-run
  should retry). "Discard staged" therefore drops only `folded = 0` rows.
- **Migration:** `MemoryState::migrate_legacy` converts old frontmatter files to
  plain `## title` + body and deletes `index.md` on first load. Existing extra
  files are kept as ordinary files.

---

## Cron jobs

A **cron job** is a schedule + a prompt + a conversation thread. Each fire appends
one ordinary `user → assistant` exchange to that thread, so a run is a
*continuation*: it sees the full prior history (memory notes, clock notes,
compaction summary), runs with memory available, and its reply lands in the
conversation the user can scroll and answer. Agent-created jobs hang off the
conversation in which they were created. **Job definitions are local-only**
(never synced); the messages a run produces are normal rows and sync, so a second
device just sees the thread continue.

- **Scheduler** (`cron.rs`, mirrors `reflection::spawn`): one pass 3 s after
  startup (a slot missed while the app was closed catches up), then every 20 s so
  `* * * * *` jobs feel right. Global kill-switch `AppConfig.cron_enabled`
  (Settings → Scheduled jobs; local, not synced). Due = an enabled job with
  `next_run_at <= now`, or any job with a queued one-shot `run_requested` (so
  "Run now" works on a paused job), oldest slot first, capped at 5 launches per
  tick and 3 concurrent runs.
- **Interlock:** a conversation is claimed through `StreamRegistry`
  (`try_register`, not `register` — a live turn may start in the window since the
  due check, and replacing its cancel token would make Stop abort the wrong
  turn). The registry records whether the holder is a cron run or a live turn:
  a live send (and an inline message edit, enforced again in `edit_user_message`)
  is refused while a cron run owns the chat, while a Stop-then-resend may still
  replace an earlier *live* turn. The run's two claims are released by an RAII
  guard, so even a panic cannot leave a chat permanently "streaming". A busy chat
  defers the job to the next tick **without advancing its slot**. The reflection
  scheduler already skips `is_streaming` conversations and aborts an in-flight
  pass when one appears, so cron, live and reflection never overlap. Stop in the
  UI aborts a runaway scheduled reply too: the `cron-updated` started/finished/
  error events drive a client-side "cron running" set, which is folded into the
  composer's busy state (`isBusy` = live streaming OR cron running) even though a
  scheduled run has no stream channel of its own. A run also has a 15-minute
  watchdog that cancels its token, so a wedged provider connection cannot lock
  the chat out of new sends.
- **Approvals are turn-scoped.** `run_tool_loop` tracks only the call ids it
  parked on an approval and releases exactly those when the turn ends
  (`ApprovalRegistry::deny_calls`). A background cron run finishing therefore
  cannot answer a pending approve/deny prompt the user is looking at in another
  conversation (the old global `deny_all` could).
- **Catch-up is max one.** The overdue slot fires once, then `next_run_at` is
  recomputed from **now**, so a device that slept for a day resyncs forward
  instead of replaying every missed slot. A manual "Run now" never touches the
  schedule (it only records `last_run_at`), except that it satisfies an already
  overdue slot so the scheduler cannot fire that same slot again a tick later.
- **Run path:** `chat::run_turn_core` (shared with live turns) does the clock
  note, compaction check, price freeze, tool rounds, abort/error semantics and
  `fetch_image` attachments; `cron::run_job` wraps it with `ToolMode::Cron`, a
  `NullSink`, a `cron_runs` row and `cron-updated` events. The user turn's text is
  the prompt plus an autonomous footer naming the job, so the model and a reader
  both know why it appeared.
- **Tool policy (per job):** the tool *list* stays byte-identical (cache
  alignment); `ToolMode::Cron(policy)` enforces at dispatch. `readonly` (default)
  refuses `bash`/`read_file`/`write_file` with a self-correcting tool-error
  naming the fix; `autoapprove` runs them unapproved. Memory / Brave / web tools
  are always allowed. The cron-management tools are allowed too, so a job can
  reschedule or disable itself. The job's policy is authoritative — it does not
  inherit the global YOLO switch, and `readonly` is not overridden by it.
  **Privilege escalation is gated:** `create_cron`/`update_cron` are ungated for
  ordinary use, but a call whose `tool_policy` is `autoapprove` is treated as a
  **gated** call (`chat::grants_autoapprove`) — the model may freely create
  readonly jobs, while granting unattended host-tool access needs the user's
  click in a live turn. A `readonly` run cannot mint an `autoapprove` job at all
  (its mode gate refuses the call before dispatch), so a prompt-injected page
  cannot bootstrap a persistent shell through the cron tools.
- **Schedule format:** a preset token (`hourly`, `daily@HH:MM`,
  `weekly@mon:HH:MM`) or a raw **5-field** cron expression, resolved in the
  device's local wall clock (`cron::schedule`, `croner`). Input is normalized on
  write; a 6-field/seconds expression is rejected, so the floor is once a minute.
  `cron_preview` gives the UI live validation + next-run text as you type.
- **Bookkeeping:** `cron_runs` is the audit trail (scheduled slot, started/
  finished, stop reason, usage, frozen cost, error). It is inserted at start with
  an empty `trigger_message_id` and updated when the turn's user row exists; the
  timeline maps that id to the job title as a **"scheduled" chip**, without a new
  column on `messages`. A run that cannot start (no model or no provider key)
  moves the slot forward silently — no run row, no counter bump — so it does not
  retry every 20 s.
- **Notifications:** `tauri-plugin-notification` (pinned `=2.4.0`: 2.5+ requires
  tauri 2.12, which would drop the vendored `tauri-runtime-wry` patch). On
  completion/failure a job with `notify` raises an OS notification (title = job
  title, body = reply snippet or error), suppressed while the window is focused
  *and* the run's conversation is the open one (`cron_set_active` is fed by the
  UI). The `cron-updated` event drives the in-app badge and thread refresh.
  Desktop-first: Android needs a notification channel + `POST_NOTIFICATIONS`
  (same deferred-device pattern as the other mobile work); the event/badge path
  is platform-neutral.
- **UI:** `src/components/CronPage.tsx` (Header → Cron) lists jobs (schedule,
  next/last run, count, enabled/notify toggles, policy badge, Run now / Edit /
  Delete), hosts the create/edit form (preset + raw cron, live preview, prompt,
  policy, notify) and the recent-runs audit trail. Sidebar conversations owning
  an enabled job get a clock glyph; the transcript shows the "scheduled" chip.

## Tool surface

Introduced through the same loop, so they're visible, gateable, and aborted like
everything else.

```rust
// Approval-gated (local side effects / local data) — ungated when YOLO
// (`AppConfig.auto_approve_tools`) is on.
bash(command)                     // runs a fresh `bash -c` per call (one-shot); Android
                                  //   executes inside the user's Termux (see ANDROID_SHELL.md)
read_file(path)                   // text, or an image part when the model is
                                  //   vision-capable (see "read_file images")
write_file(path, content)

// Memory — ungated, app-local. All four are in the shared tool list; access
// is enforced by mode.
save_memory(content, path)        // append; path required
read_memory(path)
write_memory(path, content)       // replace (curation)
memory_str_replace(path, old_string, new_string)   // replace one unique occurrence

// Web search — native Brave Search API, read-only (ungated)
brave_web_search(query, count?)
brave_local_search(query, count?)
brave_image_search(query, count?)
brave_video_search(query, count?)
brave_news_search(query, count?)
brave_summarizer(query, count?)

// Web — native HTTP GET, read-only (ungated)
web_fetch(url, max_chars?, char_start?)   // HTML → readable text; refuses
                                  //   non-public addresses. max_chars caps the
                                  //   returned text; char_start offsets into it.

// Web — native HTTP GET of an image, read-only (ungated)
fetch_image(url)                  // raster only (png/jpg/gif/webp/bmp; capped at
                                  //   MAX_IMAGE_BYTES, same SSRF + redirect
                                  //   guards as web_fetch). Delivered as an image
                                  //   part when the turn's model is vision-capable
                                  //   and PERSISTED on the assistant row, so the
                                  //   attachment is still there after a reload.

// Cron management — app-local, read/write to local DB (ungated; live + cron
// modes only, refused by reflection/extraction by mode)
create_cron(prompt, schedule, title?, conversation_id?, tool_policy?, notify?)
list_crons()
update_cron(id, prompt?, schedule?, title?, tool_policy?, notify?, enabled?)
delete_cron(id)
run_cron_now(id)                  // queues an immediate fire (~20 s tick)
```

The tool list is byte-identical for live and reflection turns so the prompt-cache
prefix matches; `run_tool_loop` refuses disallowed calls by mode instead of
splitting the list (reflection: no host/web tools; extract: none).
Gating (`tools::is_gated` = bash/read_file/write_file) is bypassed entirely when
YOLO mode is on.

**Brave integration (native, no node/MCP process):** `tools.rs` `BraveSearch`
calls `api.search.brave.com` directly over `reqwest` (the same six-tool surface
the official `brave-search-mcp-server` exposes), with the key from the OS
keychain (`secrets::brave_key`). Because there's no child process, config file,
or node runtime, it works identically on desktop and Android. The Brave tools
are offered only when a key is stored (`brave_available()`). Setup: add a Brave
Search API key in Settings → keychain.

**`web_fetch` (native, ungated):** `tools.rs` `execute_web_tool` fetches an
`http(s)` URL over `reqwest`, converts the HTML to readable text, and truncates
the result (the body is streamed and capped, so a huge response is not
buffered). The model may window the returned text with an optional `max_chars`
(which can only lower the hard ceiling, never raise it) and a `char_start`
offset — enough to read just the top of a page, or page through a long one; a
windowed result reports the range it returned so continuation is unambiguous.
It needs no key and is read-only, so it is ungated like the Brave
tools — but it refuses non-public addresses (loopback, RFC1918 private,
link-local, ULA, CGNAT `100.64/10`, and the cloud metadata endpoints
`169.254.169.254` / `192.0.0.192` / `100.100.100.200`), resolving hostnames and
refusing if any resolved address is non-public. Each redirect is re-checked for
scheme and a non-public literal host (max 5 hops). The result text is framed as
untrusted external data, because a prompt-injected page must not be treated as
instructions.

**Residual `web_fetch` risks — consciously accepted (personal, local-first
tool):** a redirect to a public-looking name that resolves to a private address
(DNS rebinding) is not caught, and the untrusted-content framing is an
instruction, not enforcement — a prompt-injected page could still drive the
ungated `write_memory`/`memory_str_replace` into the synced memory files. Both
are judged unlikely and low-impact enough to accept, rather than pay for
IP-pinning / egress control or add approval prompts. The sharper un-gated edge is
outbound data smuggled in tool *arguments* (a `web_fetch` URL or a search query),
which stays bounded because `read_file` and the keychain are gated. Note also
that un-gating `write_memory` means an ordinary (non-injected) live turn may
overwrite curated files by accident — the trade that removed the old
"consolidation-only" gate. The cron tools widen this surface: an injected page
could also create/update/delete scheduled jobs and queue runs. Creating a
*readonly* job is low-impact (it posts into a thread and spends tokens); granting
`autoapprove` is the dangerous one and is **gated** (see "Cron jobs"), while a
readonly run cannot mint an autoapprove job. Revisit (gate `web_fetch`/the memory
writes, or pin the validated IP) if this ever ships to a wider audience.

---

## Commands

Run from the repo root (`/home/rp/chat`):

```bash
npm install            # install JS deps (first time)
npm run dev            # Vite dev server only (no Tauri window)
npm run build          # Vite production build (outputs to dist/)
npm run tauri dev      # compile Rust + launch the app window
npm run tauri build    # compile + bundle an installer
npm run e2e            # GUI end-to-end test (mock LLM + WebDriver; see below)
```

Rust-only:

```bash
cd src-tauri
cargo check           # fast type/borrow check
cargo test --lib      # unit + mock-SSE integration tests
cargo build           # full debug build (slow first time: Wry + bundled SQLite)
```

Optional live provider check (ignored by default; hits the real endpoint):

```bash
cargo test --lib -- --ignored live_reasoning_echo
# overrides: PI_LIVE_BASE_URL, PI_LIVE_MODEL; key read from the keychain
```

### GUI end-to-end testing (`e2e/`)

`npm run e2e` drives the **real webview** against a mock OpenAI-compatible SSE
server (`e2e/mock-llm.mjs`) using `tauri-driver` + `WebKitWebDriver`. It asserts
the behavior unit tests can't reach: no reply truncation (the mock's final SSE
event omits its terminating blank line), reasoning-trace clickability / DOM
stability across streamed updates, auto-scroll, bottom anchoring, the inline
transcript (reasoning containers + always-inline tool cards, and the gated
approval state) and memory consolidation. Screenshots →
`e2e/screenshots/` (gitignored).

- **Prereq:** `cargo install tauri-driver --version 2.0.6 --locked` (the **2.x**
  release is the Tauri v2 one; 3.0.0-alpha targets Tauri v3).
- The runner builds the debug binary **only if missing** — after changing Rust
  code, `cargo build` first so the harness picks it up.
- It launches the app with an **isolated `XDG_DATA_HOME`** (seeded with the mock
  endpoint) *and an isolated secret store*: `run.sh` exports
  `PI_SECRETS_DIR="$DATA_DIR/secrets"`, and `secrets.rs` redirects every
  read/write/remove into plain `0600` files there when that variable is set
  (unset — the default — the OS keychain path is byte-for-byte unchanged; the
  Android file store shares the same `file_store` module, so the two cannot
  drift). A run therefore cannot read, write or delete a real keychain entry. The
  harness still *proves* it: it snapshots every `com.rp.chat` entry (via
  `secret-tool`, sha256) before and after and fails if they differ, on the failure
  path too.
- The mock key is set through the **provider edit form**, never Settings' first
  `input[type=password]` — **that one is the Brave Search key**. Getting this
  wrong is silent: an earlier version typed `mock-key` into the Brave slot and
  left the LLM key unset, which is why the picker stayed empty
  (`model picker … : [""]`) and why a `brave_key` of `mock-key` appeared in the
  keychain. (That history is why this used to be mis-documented as a keychain
  failure.)
- `run.sh` seeds the config's `model` with a **deliberately unlisted** id
  (`e2e-never-listed`). The header `<select>` always synthesizes a stub option for
  the current model, so seeding the mock's own id would make the picker assertion
  vacuous; `mock-model` appearing proves the endpoint fetch actually ran.
- **Ports are strict.** `run.sh` fails fast if `1420`/`8317`/`4444` are busy and
  starts Vite with `--strictPort` — otherwise a stale dev server on 1420 is
  silently tested instead of the tree under test.
- **Input / clicks:** on this WebKitGTK build, `Element Click` / `Send Keys` /
  W3C Actions return `unsupported operation` (`/usr/bin/WebKitWebDriver` is the
  GTK4 build while the app runs on `webkit2gtk-4.1`). Clicks are dispatched via
  in-page `el.click()`; the click-churn regression is covered by a DOM-node
  identity assertion. See `e2e/README.md`.

---

## Cloud sync (Supabase)

Opt-in, local-first mirror, off by default. **SQLite stays the source of truth**;
with no network, sync enabled ≠ logged in, or the toggle off, the module is idle
and the app is byte-identical to before. Never blocks the UI.

- **Auth:** email + password against GoTrue (`/auth/v1/token?grant_type=password`)
  with the publishable (anon) key. The session (access/refresh token + user id +
  email) is stored in the OS keychain under a separate `sync_session` account and
  never crosses IPC. The service-role key is never compiled in — `build.rs` embeds
  only `SUPABASE_URL` + `SUPABASE_PUBLISHABLE_KEY` from `.env` (gitignored). RLS
  (`auth.uid() = user_id`) is the authorization boundary.
- **Sync engine** (`sync.rs`): push upserts `dirty` rows to PostgREST
  (`Prefer: resolution=merge-duplicates`, scoped clear by `(id, revision)` so a
  row written mid-flight isn't dropped); pull fetches `revision > last_seen` and
  applies with last-writer-wins by revision, tombstones (`deleted_at` → local
  hard-delete), and re-orders a conversation's local `index` by `(created_at,
  id)` for consistent cross-device ordering. **Live progress** rides
  `SyncStatus` for the Settings panel: push reports per batch against `pending`,
  and pull reports per page with a total (`pull_total`, from PostgREST
  `Prefer: count=estimated` → `Content-Range` on each table's first page) that the
  UI turns into a bar — `estimated` (exact up to `db-max-rows`, planner estimate
  beyond) rather than `count=exact`, because the unbounded exact COUNT over the
  whole filtered set on a large `messages` table under RLS blows Supabase's
  statement timeout (57014) and wedges the pull at its first page; a server
  without the count leaves pull `↓N` without a denominator. **User config** (explicit
  preferences + providers + price/capability overrides) syncs as a single global
  `user_config` row. It is the one place a single row's whole-row LWW was not
  enough: a row mixes fields edited independently, so the row revision had to
  stand in for "whose content is authoritative" — wrong every time a row is
  CREATED rather than edited — and three different states (never set, deliberately
  cleared, merely inherited) were all just "empty". So the payload carries
  **per-key revisions** and merging is per key.

  **v2 payload.** `{ v: 2, preferences, providers, activeProviderId, baseUrl,
  model, modelOverrides, revs }`, where `revs` maps a key to the revision at which
  THIS device last set it — scalars by field name, collection entries as
  `provider:<id>` / `override:<model>`. Therefore: a key **absent from `revs`** (or
  0) was **never set** and is adoptable from the other side; `revs[key] > 0` with an
  **empty/absent value** is a **clear**, which is a real edit that propagates; and
  otherwise the value is ordered against the other side by that revision. A key
  present in `revs` whose value is gone is a per-key TOMBSTONE — how a removed
  provider is carried. `v` marks the format: a payload without it is **v1** (the
  value-only shape) and is migrated ON READ (`migrate_v1`), which stamps the row's
  own revision on the keys it carries and leaves its empty/default keys at 0 — the
  distinction v1 could not express. `model` is deliberately never stamped (it is
  not content on its own) and is ordered by `KEY_BASE_URL`, so the legacy
  `base_url` + `model` pair is never mixed.

  **Merge** (`SyncableConfig::merge` / `merge_entries`) is per-key
  last-writer-wins: the higher revision wins, and a tie falls back to the v1 value
  rule (the side that CARRIES a value wins; `self` on a tie), which keeps it
  behaviour-identical to the old `union_with` for v1 payloads. Two consequences
  worth knowing: a conflicting key **converges on the NEWER value** on both sides
  rather than resolving local-wins (the devices agree instead of each keeping its
  own copy), and a clear now survives instead of being refilled.

  **Who does what.** `db::sync_user_config` mirrors the live config into the row on
  every edit (Settings saves, provider edits), stamping a fresh revision on each
  CHANGED key — diffing values rather than tracking edit provenance is what keeps
  "cleared" (a change) apart from "never set" (no key at all). It never creates a
  row for a config that has never been edited, so a blank device cannot publish:
  that is now a property of the payload rather than an empty-guard special case.
  The PUSH fetches the server row, merges, and pushes only when the result differs
  from it (our newer keys, or keys it lacks), at a revision above the server's — so
  a push can never drop a key it left unset. The PULL merges each incoming row per
  key and adopts the result (`converge_user_config_row`), projecting the merged
  values into the live config AND writing the row: dirty if we still hold something
  newer (the next push publishes it), clean at the remote's revision if we only
  absorbed. There is **no row-revision gate** anywhere — that gate is what let a
  blank-but-newer row refuse the canonical config forever. The post-pull
  `reconcile_user_config` (called from `reconcile_config_after_pull`) re-reads the
  row BY ID, ignoring the high-water, so it can still heal a device whose watermark
  already ran past the server's revision; it re-mirrors the live config first, so a
  local edit that had not been stamped yet cannot be read as a clear.

  Ciphertext we can't decrypt is never overwritten (locked-device contract). A
  remote tombstone is a revisioned event, not a permanent ban: it is never ADOPTED
  (an empty payload would read as "everything cleared") and content OLDER than the
  delete is suppressed — with its dirty flag cleared, so a suppressed row neither
  shows as pending nor re-fetches forever — while a local edit made SINCE the delete
  is strictly newer and republishes. The row revision survives as the transport
  clock only: the pull's `revision > last_seen` filter, and the mirror's floor at
  `max(row, watermark)` so a slow-clock device cannot wedge its config below the
  server's.
  The reflection watermark
  (`last_reflected_index`) rides on the conversation row and is preserved across
  re-index. The first sync per account (guarded by a per-`user_id` sentinel)
  seeds all pre-existing rows +
  memory files dirty so enabling backs up full history once. Three cross-device
  gotchas are handled explicitly: (1) **imported chats are sync-dirtied** —
  `insert_imported_conversation` writes them with `dirty = 1` (and a duplicate
  re-import re-marks them, so an archive imported *after* the first sync still
  reaches other devices); (2) the first-sync **seed skips empty memory files** —
  a fresh device auto-creates empty `profile.md`/`preferences.md`/`goals.md`
  stubs, and if those were seeded dirty their freshly-bumped revisions would beat
  the real content on the mirror under LWW and silently drop it on pull. (3)
  **message tombstones for edit-time truncation** — `edit_user_message` deletes
  later rows, so it records a row per deleted id in `sync_tombstones`
  (which gained a nullable `conversation_id` column for this; the truncated row
  is gone by push time and the remote `messages` row needs a non-null
  `conversation_id`). Push emits those as `messages` rows with `deleted_at`, and
  `clear_message_tombstones` retires them after a successful push. Inherits the
  same wall-clock-revision clock-skew caveat as conversation tombstones.
- **Memory files** sync whole-file (identity by path), tracking metadata in the
  `memory_sync` table; a failed apply is left dirty and retried.
- **Client-side encryption** (`crypt.rs`, opt-in, **off by default** — see
  "Next / open items" for the multi-device caveat): content fields (conversation
  title/system_prompt/summary, message content/thinking/usage/attachments, memory
  content) are encrypted with AES-256-GCM (`enc:v1:<base64 nonce||ct||tag>`)
  before upload and decrypted on pull, so Supabase only ever holds ciphertext.
  Structural columns (`id`, `revision`, `conversation_id`, `created_at`,
  `deleted_at`, `path`) stay plaintext so LWW/tombstones/ordering still work.
  The 256-bit data key is a **random key generated on the device** and stored in
  the OS keychain (`secrets::enc_key_*`), never in `config.json`. It can be shown
  as a portable **recovery code** (`sync_recovery_code`, base64) and pasted on
  another device (`sync_import_key`) to unlock the same mirror — so multi-device
  works with no server state and no passphrase. `sync_set_encryption` generates
  the key (if absent), and marks every row dirty so the plaintext mirror is
  re-uploaded as ciphertext; `sync_remove_encryption` does the reverse. A device
  without the key that finds an encrypted mirror is **locked**: `do_sync` probes
  the server (`fetch_encrypted_sample`), refuses to push (so it can never
  overwrite ciphertext with plaintext), and surfaces `SyncStatus.locked` so the
  UI asks for the recovery code. **The local SQLite DB is deliberately NOT
  encrypted** — it lives on the user's own machine (OS disk encryption covers
  idle-at-rest), and keeping it plaintext avoids "lost key = locked out of
  everything". Losing the key only costs the cloud backup, never local data.
- **Commands:** `sync_sign_in`, `sync_sign_out`, `sync_status`, `sync_toggle`,
  `sync_now`, `sync_set_encryption`, `sync_remove_encryption`, `sync_import_key`,
  `sync_recovery_code` (see `api.ts`).
  Background scheduler (60s tick) runs push+pull only
  when enabled + signed in; a `syncing` swap-guard prevents overlapping runs.
  It also runs one round **immediately on startup** (rather than after the first
  60s wait), so changes made on another device reconcile as the window opens;
  the call is a no-op when sync is disabled or signed out.
- **Schema:** `supabase/schema.sql` (run in the Supabase SQL editor) creates
  `conversations`, `messages`, `memory_files` with revision/deleted_at, enables
  RLS, and stores the client revision via a trigger.

> **Production blocker — email confirmation is currently OFF.** For a personal
> local-first tool we run Supabase Auth with the **"Confirm email"** toggle off
> (sign-up logs you in immediately; there is no emailed confirmation link). GoTrue
> `/auth/v1/verify` links therefore never come back into the app. Before shipping
> to a wider audience you MUST: turn **Confirm email** ON in Supabase Auth, point
> the **Site URL** /
> **Redirect URL** away from the default `http://localhost:3000`, and add a
> confirmation callback to the app (a localhost HTTP verifier or a `pichat://`
> deep-link) so clicking the emailed link lands back in the app and completes
> verification. The backend already maps `email_not_confirmed` to a friendly
> "check your inbox" message, so the auth side is ready for that switch.

## File layout

```
/home/rp/chat
├─ AGENTS.md           # this file (single source of truth)
├─ index.html
├─ vite.config.ts      # Vite + vite-plugin-solid + @tailwindcss/vite
├─ package.json
├─ scripts/            # android-debug.sh / android-release.sh (`tauri android build` wrappers)
├─ e2e/                # GUI end-to-end harness (mock LLM + tauri-driver/WebKitWebDriver)
│  ├─ run.sh           # orchestrates mock + vite + tauri-driver + e2e.mjs
│  ├─ e2e.mjs          # WebDriver client + assertions
│  ├─ mock-llm.mjs     # scripted OpenAI-compatible SSE server
│  └─ README.md
├─ src/
│  ├─ index.tsx        # entry; imports ./index.css (Tailwind)
│  ├─ index.css        # Tailwind import + typography + class-based dark variant
│  ├─ App.tsx          # thin shell: shared chat/settings state + mounts the surfaces
│  ├─ components/      # surfaces split out of App.tsx (Sidebar, Header, MessageList,
│  │                   #   Timeline, ImageLightbox, Composer, MemoryPage, CronPage,
│  │                   #   SettingsDialog)
│  └─ lib/
│     ├─ api.ts        # typed invoke wrappers over Tauri commands
│     ├─ platform.ts   # Android bridge: isAndroid / onBack / exitApp / readAttachmentFromUri
│     ├─ turns.ts      # pure turn/timeline helpers + types (groupTurns/buildEntries/parsers)
│     ├─ icons.tsx     # inline SVG icons
│     └─ Markdown.tsx  # marked + DOMPurify renderer (prose + dark:prose-invert)
└─ src-tauri/
   ├─ tauri.conf.json  # v2 config (productName, window, devUrl, frontendDist)
   ├─ Cargo.toml       # tauri, serde, rusqlite [bundled], uuid, async-openai, keyring, reqwest, tokio, tokio-stream, chrono
   ├─ capabilities/default.json
   ├─ vendor/tauri-runtime-wry/  # vendored+patched crate (see PATCH.md) via [patch.crates-io]
   ├─ gen/android/     # generated Android project (Kotlin: MainActivity.kt window
   │                   #   insets/back + picker launcher, PickerPlugin.kt system
   │                   #   file picker, FileBridgePlugin.kt content:// resolver,
   │                   #   KeepAliveService.kt foreground service, RunCommandPlugin.kt Termux bridge)
   ├─ build.rs         # embeds SUPABASE_URL/PUBLISHABLE_KEY from ../.env (rerun-if-changed)
   └─ src/
      ├─ main.rs       # thin entry -> chat_app_lib::run()
      ├─ lib.rs        # Builder, setup (SQLite + config + Brave + registries + reflection), invoke_handler
      ├─ db.rs         # Db state, schema/migration, conversation/message commands + helpers
      ├─ config.rs     # AppConfig, config.json persistence, legacy key migration
      ├─ secrets.rs    # OS-keychain API key storage (keyring crate) + has/set_api_key
      ├─ crypt.rs      # client-side AES-256-GCM + Argon2id key wrapping (cloud sync)
      ├─ models.rs     # list_models (models.dev catalog, /models fallback), chat filter,
      │                 #   per-baseUrl cache, ModelInfo (override-aware `vision`)
      ├─ tools.rs      # tool specs, host executor, gating + approval registry
      │                 #   (YOLO bypass), native BraveSearch
      ├─ shell.rs      # one-shot host shell executor (fresh `bash -c` per call);
      │                 #   Android routes to the Termux bridge (android.rs)
      ├─ android.rs    # Android [mobile]: Termux RUN_COMMAND bridge (`run_command`
      │                 #   plugin), `content://` file bridge (`file_bridge`), rustls init
      ├─ memory.rs     # Plain-Markdown memory: core files, listing, save/read/write, migration
      ├─ import.rs     # Anthropic-export importer (timestamp-preserving, backfill-only)
      ├─ reflection.rs # Idle memory-consolidation scheduler + per-conversation pass
      ├─ cron.rs       # Cron scheduler (due selection, catch-up, interlock), run
      │                 #   path (run_job), cron agent tools + UI commands
      ├─ cron/
      │  ├─ schedule.rs # 5-field cron / preset parsing + next-fire (local wall clock)
      │  └─ tests.rs    # cron DB/scheduler tests (due selection, run state, cascade)
      ├─ sync.rs       # Supabase sync engine + auth commands + 60s scheduler
      └─ sync/
         └─ tests.rs   # sync tests (revision, LWW, tombstones, re-index, mock PostgREST)
      ├─ attachments.rs # read paths -> Attachment (image data URL / text / metadata)
      ├─ pricing.rs    # pricing + models.dev metadata cache; overrides -> cache -> bundled;
      │                 #   resolve_capabilities (modalities/reasoning, override-aware)
      ├─ chat.rs       # stream_chat/stop_chat, raw SSE, run_tool_loop, StreamRegistry, StreamEvent
      └─ chat/
         └─ tests.rs   # chat-loop tests (mock SSE: tools, reasoning, abort, usage)
```

---

## Conventions

### Frontend (SolidJS)

- Use `createSignal` / `createEffect` / `For` / `Show` from `solid-js`. No class components.
- All Tauri calls go through typed wrappers in `src/lib/api.ts`; components never call `invoke` directly. Android-only hooks live in `src/lib/platform.ts` (`isAndroid`/`onBack`/`exitApp`/`readAttachmentFromUri`).
- UI surfaces live in `src/components/*` (Sidebar, Header, MessageList, Timeline, Composer, MemoryPage, SettingsDialog); `App.tsx` is a shell that owns shared state and passes it down as props. Put new surface state in the owning component.
- Style with Tailwind utility classes only (no CSS modules). Tailwind scans `src/**`.
- Kobalte components are namespaced: `import { Dialog } from "@kobalte/core/dialog"` then `<Dialog.Root>`, `<Dialog.Trigger>`, etc.

### Backend (Rust / Tauri)

- New module → `src-tauri/src/<x>.rs`, declare `mod x;` in `lib.rs`, register commands in `tauri::generate_handler![...]`.
- Commands take `tauri::State<'_, Db>` for DB access; lock the `Mutex<Connection>` briefly.
- Errors return `Result<T, String>`.
- Tauri v2 maps JS `camelCase` args to Rust `snake_case` params automatically (`conversationId` → `conversation_id`).
- `StreamEvent` is serialized straight to the frontend. Enum-level `rename_all` only renames **variants**, so fields need `rename_all_fields = "camelCase"` (e.g. `call_id` → `callId`); keep the `StreamEvent` union in `src/lib/api.ts` in sync.
- `bash` runs a fresh `bash -c <cmd>` process per call — state never persists (`cd`/`export`/functions reset each call; see `ANDROID_SHELL.md`). On Android an executor routes every call into the user's Termux via its `RUN_COMMAND` intent; `read_file`/`write_file` follow the same path (desktop: direct host calls).
- Chat-loop tests live in `src/chat/tests.rs`; add loop/tool tests there.

### SQLite

- DB file: app data dir `chat.sqlite`. On Linux: `~/.local/share/com.rp.chat/chat.sqlite`.
- Add tables/columns to `db.rs` `SCHEMA`; for existing DBs add a best-effort `ALTER TABLE ... ADD COLUMN` in `open()` and ignore the duplicate-column error.

---

## Environment gotchas

- **Brave Search (native):** the app calls `api.search.brave.com` directly over
  `reqwest` — no node, no MCP process, no `~/.config/opencode` path, so it works
  on desktop and Android alike. The key lives in the OS keychain (separate
  `brave_key` entry from the LLM key) and is entered in Settings. Web tools are
  offered only when a key is stored.
- **Fireworks is the default endpoint** (`https://api.fireworks.ai/inference/v1`) but the client is generic; the user enters `baseUrl` + `apiKey` in Settings. Fireworks needs a `fw_` key; Brave needs a separate Brave Search API key.
- **Rust builds are slow** on first run (Wry + bundled SQLite). `cargo check` is faster for iteration.
- **GUI needs a display:** use `npm run tauri dev` from the desktop session; scripted checks use `npm run e2e` (same requirement).
- **One-shot shell:** `bash` runs a fresh `bash -c` per call — no persistent session, so `cd`/`export` do not survive (the agent already assumes non-persistence; see `ANDROID_SHELL.md`). 60s per-call timeout kills the child. On Android, execution happens inside the user's Termux (`crate::android` + `RunCommandPlugin`).
- **Android:** `npm run tauri android init` created `src-tauri/gen/android`. The Rust bridge compiles for the aarch64 Android target via `cargo check --target aarch64-linux-android` (set `CC_aarch64_linux_android`/`AR_aarch64_linux_android`/linker to the NDK clang). The actual Termux `RUN_COMMAND` round-trip requires a device.
- **Android TLS (rustls-platform-verifier):** reqwest 0.13 routes *every* HTTPS
  request (models.dev catalog, streams, Brave, sync) through
  `rustls-platform-verifier`. On Android that verifier can't reach the OS trust
  store by itself — it needs a JNI env + `Context`, and without them the first
  handshake hard-aborts the whole process (SIGABRT, "Expect
  rustls-platform-verifier to be initialized"). Two things are wired up so this
  works: (1) `android.rs` exports `Java_com_rp_chat_MainActivity_initRustls`
  (a `#[no_mangle]` JNI fn) that calls
  `rustls_platform_verifier::android::init_with_env`, invoked once from
  `MainActivity.onCreate`; and (2) `app/build.gradle.kts` locates the
  `rustls-platform-verifier-android` AAR via `cargo metadata` (local maven repo)
  and bundles its `org.rustls.platformverifier` Kotlin component, with a
  proguard keep-rule so release minify doesn't strip it. Adding a `reqwest`
  fallback with webpki roots is an alternative if device CA trust is ever not
  wanted; `rustls-webpki-roots` isn't in reqwest 0.13.4 yet.
- **Keychain entry:** service `com.rp.chat`, user `api_key`. A legacy plaintext `apiKey` in `config.json` auto-migrates on first launch.
- **Android secrets:** the `keyring` crate has **no Android backend** (its `v1`
  backend is cfg-gated away from Android), so every keychain op would error
  there. `secrets.rs` abstracts reads/writes behind `read`/`write`/`remove`; on
  Android those hit one file per secret (`secret_<hex>`) in the app-private data
  dir (device-encrypted + sandboxed, `0600`), seeded once in `setup` via
  `secrets::init_android`. Account names are hex-encoded so provider ids can't
  collide, and an empty file reads as "not set" (matching keyring). Desktop
  still uses the OS keychain, unchanged. Because the dialog opens *before* its
  async setup, an unavailable keychain can never block opening Settings.
- **Android edge-to-edge insets:** edge-to-edge is enforced on Android 15+
  (targetSdk 35+), so without handling window insets the app's header hides
  under the status bar and the composer/bottom row under the gesture-nav bar.
  `MainActivity.onWebViewCreate(webView)` overrides the `WryActivity` hook and
  sets an `OnApplyWindowInsetsListener` that translates the system-bar +
  display-cutout insets into WebView padding — one hook, no Rust/plugin changes.
- **Android relaunch blank-screen + vendored patch:** after a swipe-away,
  `prevent_exit()` + `KeepAliveService` keep the process alive with **zero
  windows**; upstream `tauri-runtime-wry` 2.11.4 never delivers
  `RunEvent::Resumed` on mobile (the loop drives `ControlFlow::Wait`, so its
  `StartCause::Poll` arm is dead), so the window-recreate handler in `lib.rs`
  never ran and the relaunch came back blank until the process was killed. Fixed
  by **vendoring + patching** the crate (`src-tauri/vendor/tauri-runtime-wry`,
  upstream PR #15678) via `[patch.crates-io]` — see `PATCH.md` for the exact hunk
  and removal steps. Device-verification still pending.
  **Do NOT bump `tauri` past 2.11.x without re-solving this.** Upstream 2.12.x
  *does* carry the Android plugin-launcher fix (tauri#15506 — the "unregistered
  ActivityResultLauncher" file-picker break, fixed by PRs #15798 + #15949), but it
  also moves to tao 0.36 / wry 0.57, where `tauri-runtime-wry`'s mobile
  `Event::Resumed | Event::Suspended` arm is **gone**, so this patch no longer has
  a site to apply to — and tauri#15671 (blank relaunch) is **still open / unmerged**
  (#15678). Bumping today would trade the picker fix for a regression of the
  blank-screen bug. Revisit once #15678 (or an equivalent tao 0.36 fix) lands, then
  re-vendor or drop per `PATCH.md`.
- **Android file pick + hardware back:** attachments are picked by our own
  **`picker` plugin** (`PickerPlugin.kt`), not `tauri-plugin-dialog`: Tauri's
  `PluginManager` binds the dialog plugin's `ActivityResultLauncher` to the first
  Activity and never rebinds it, so any Activity recreation (which this app hits
  on every swipe-away → icon relaunch, since `prevent_exit()` +
  `KeepAliveService` keep the process alive) makes every later `showFilePicker`
  fail with "Attempting to launch an unregistered ActivityResultLauncher" until
  the process is killed (tauri#15506 — see the deferred-bump note in this list).
  `PickerPlugin` registers its launcher in `MainActivity.onCreate` — re-run on
  every creation, the one moment registering is legal — and
  `pick_attachment_uris` launches it over the mobile bridge; the returned
  `content://` URIs go through the existing content-URI reader
  (`FileBridgePlugin` / `read_attachment_from_uri`, which builds the
  `Attachment`). Desktop still uses the dialog plugin. Hardware back goes through
  `MainActivity`'s `OnBackPressedCallback`, which dispatches a **cancelable**
  `pi-back` event; `src/lib/platform.ts`'s `onBack` returns `true` to consume it,
  otherwise the activity finishes (so back exits only on the chat page). Both
  need a device.
- **Supabase URL is compile-time, from `.env`.** `build.rs` reads `../.env` and
  emits `cargo:rustc-env=SUPABASE_URL=…`. It now declares
  `cargo:rerun-if-changed` on that path: without it, cargo reuses a build-script
  output produced before `.env` existed (e.g. a `CARGO_TARGET_DIR` shared with a
  worktree), leaving the `https://localhost.supabase.co` fallback in the binary —
  which surfaces as an opaque sync request error (now a clear one via
  `sync::ensure_configured`).

---

## Next / open items

**Recent backlog (landed):** a 13-item pass — drawer-open keyboard, memory page
list→detail on mobile, Enter=newline on Android, live↔persisted timeline
interleave, YOLO mode, Android file/image picker, touch-reachable chat-list
actions, Android relaunch fix (vendored patch), `read_file` images, editable +
resolved model capabilities, hardware back stack, stop→composer, editable
messages. `App.tsx` was split into `src/components/*` to make that parallelisable.

Residuals/open from that pass:

- **Android behaviors need a device** (picker, hardware back, and the
  relaunch/recreate fix); the Kotlin isn't compiled off-device.
- **Deferred: bump `tauri` 2.11.x → 2.12.x** — it carries the upstream Android
  plugin-launcher fix for the "file picker silently stops working after a
  swipe-away/relaunch" bug (tauri#15506), but 2.12 also moves to tao 0.36 / wry
  0.57, where the vendored `tauri-runtime-wry` patch no longer applies and
  tauri#15671 (blank relaunch) is still open. Bumping now would regress the
  launch bug. See the vendored-patch gotcha for the full condition.
- **Android: the Memory "Import file…" picker still uses
  `tauri-plugin-dialog`** (`importChats` in `App.tsx`), so it inherits the same
  "unregistered ActivityResultLauncher" break after an Activity recreation — and
  a `content://` URI can't be read by `std::fs` anyway (`import.rs` takes a path).
  Route it through the `picker` plugin and copy/stream the file to a real path
  when it's next touched. The two *attachment* pickers were fixed in-app via the
  `picker` bridge (see the Android file-pick gotcha).
- **`stop()` does not restore the draft.** The user row is persisted before
  streaming starts, so restoring would duplicate it on the next send; the turn
  stays in the transcript and is editable instead. (Zero-event aborts therefore
  leave a user turn with no reply.)
- **`regenerate` dedup** matches the last `user` row by content (robust to a row
  appended after the edit).
- **Message-tombstone** propagation shares the wall-clock-revision clock-skew
  caveat noted above.

**Milestone 7 — context** (done except the optional tail):

1. ✅ **Near-limit note**: a non-blocking line above the composer past 80% and a
   `compacted` chip in the meter. Compaction is automatic, so the note informs
   rather than prompting action (the original "compact or new chat" wording no
   longer applies).
2. ✅ **Per-conversation compaction**: older messages are summarized into
   `conversations.compaction_summary` and fed into the request as
   `system + summary + last ~15k tokens verbatim`; forced at 80% of the window.
   See "Context compaction" above.
3. Optional: sidebar cost readout; virtualized message list.

**Known limitation (compaction):** `COMPACTION_KEEP_TOKENS` (15k) is a fixed
absolute budget, so a window whose 80% threshold is below ~19k tokens leaves
nothing to fold (the whole history fits the verbatim budget) and no compaction
runs. Also, the first pass folds everything before the 15k tail in one request;
in practice that is smaller than the request that triggered it, but a very large
jump (e.g. a freshly imported archive) could produce a large summarizer call.
Deeper work: a request-size-aware keep budget and window-aware chunking (as the
backfill reduce does).

**V2 (deferred):** conversation recall via a vector index (sqlite-vec + embedder;
behind a retrieval seam); container/sandbox tool execution; richer cross-conversation
memory conflict resolution.

**Also deferred:** the `CompatConfig` layer for provider quirks (thinking field,
`max_tokens`, developer-vs-system role). Default OpenAI-shaped behavior is fine
until a provider breaks it; the first knob (`echo_reasoning_content`) has landed.

**TO FIX (legacy provider layer, low priority — not currently causing problems):**
the pre-multi-provider compatibility shim is load-bearing and should eventually be
migrated away. It includes: synthesis of an implicit `legacy` provider when
`providers[]` is empty, the `base_url`/`model` mirrors kept in sync with the active
provider (`sync_active` in `providers.rs`), and `providers::resolve()` falling back
to the legacy `api_key` keychain entry. It exists so old installs / saved
`config.json`s keep working after the provider refactor, and several code paths
(settings, chat, pricing, model resolution, and the `user_config` sync via
`syncable_payload`/the empty-guard) go through it. Retiring it means making every
config a real provider (a UX/schema decision), so treat it as its own migration
project — do NOT rip it out as a drive-by edit. The sync empty-guard already works
*with* it: factory-default mirrors count as "empty/not configured" while a
legacy custom `base_url`/`model` is treated as real content and still syncs.

**Client-side encryption — implemented but OFF by default (revisit later).** The
`crypt.rs` / `sync_set_encryption` feature works (AES-256-GCM, random key in the
OS keychain, optional recovery code), but the **multi-device workflow is awkward**:
unlocking a second device means copying a base64 recovery code over by hand
because the key never leaves the device. That's why it stays off for now — the
plaintext mirror is fine for a personal, single-device setup given Supabase's
no-train/no-sell policy. Revisit if a smoother key-transfer story is wanted
(e.g. wrapping the key with a user passphrase and syncing the wrapped blob, or an
OS-level cross-device key sync) before recommending it. Note: the key/commands
are inert unless the user enables it (`sync_enabled` + a stored key).

**Known gaps:** the per-conversation scratch workspace for `read_file`/`write_file`
is not implemented — host tools operate on real paths with per-call approval.

**Cron residuals (v1):**

- **The scheduler only runs while the process lives.** There is no OS-level
  alarm/background service: desktop close-to-tray and Android's `KeepAliveService`
  keep it alive, but if the app is fully killed a due job fires on the **next
  launch** (catch-up max 1), not at its wall-clock time. A true background
  scheduler would need platform work (WorkManager on Android, a systemd
  user timer / launchd agent on desktop).
- **No retries.** A failed run is recorded (`cron_runs.error`) and notified; the
  schedule moves on. Transient provider errors therefore cost that slot.
- **Android notifications are deferred** (channel + `POST_NOTIFICATIONS` +
  manifest). The plugin is registered and desktop works today; the in-app
  `cron-updated` badge/refresh path is platform-neutral.
- Job definitions are local-only by design, so two devices each run their own
  copy of a job; the *threads* sync, so runs from both devices interleave.
