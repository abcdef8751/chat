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
- Optional tools — `bash` (a **per-conversation persistent shell**), `read_file`,
  `write_file` (all approval-gated), and `web_search` (Brave via MCP, ungated)
- Reasoning traces from thinking models (inline "Thought" bar → popup)
- Per-turn **activity timeline** grouping reasoning, preamble text, and tool calls
- Plain-Markdown long-term **memory** curated by an idle reflection pass
- Context + price counters (per-turn cost is frozen at the model that produced it)
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
  `async-openai` for LLM types and `rmcp` for the Brave MCP server.
- **The model decides; the user controls.** Tools and memory writes are surfaced,
  gateable, and auditable.
- **Separate storage size from injected context size.** We can store a lot; we
  inject a small, bounded subset at prompt time.
- **Generic OpenAI-compatible client.** Fireworks is only a prefilled default;
  nothing is hardcoded to a provider. Secrets stay in the OS keychain and never
  cross the IPC boundary.

---

## Current status

`cargo test --lib`: **103 passed, 3 ignored** (the opt-in live reasoning-echo
check, the real-export import, and the real models.dev catalog parse). Clippy,
`tsc --noEmit`, `npm run build`, and `npm run e2e` are clean.

| Area              | State | Notes                                                                                                     |
| ----------------- | ----- | --------------------------------------------------------------------------------------------------------- |
| Stack / scaffold  | ✅    | SolidJS + Tailwind v4 + Kobalte + Tauri v2; SQLite via rusqlite (bundled)                                  |
| Streaming loop    | ✅    | Raw SSE parse (keeps provider `reasoning_content`), deltas, abort, capped tool loop                        |
| Config & secrets  | ✅    | OS keychain; models.dev catalog with `/models` fallback; thinking levels; price overrides                  |
| Conversations     | ✅    | Sidebar, search, rename/delete, auto-title, attachments, per-turn timeline                                 |
| Tools             | ✅    | `bash` (persistent shell), `read_file`/`write_file` (gated), `web_search` (MCP, ungated)                   |
| Memory            | ✅    | Plain Markdown, core files + agent-grown files, idle reflection, separate reflection cost, parallel backfill |
| Context + price   | ◑     | Context counter + frozen per-turn cost done; **near-limit banner + compaction (M7) remain**                |
| GUI e2e           | ✅    | `npm run e2e` — mock LLM + WebKitWebDriver (streaming, timeline, tools, reflection)                        |

---

## Core loop (trimmed)

`chat.rs` `run_tool_loop` (shared by live chat and reflection):

```
prompt(userMessage)
├─ build context (system + explicit preferences + memory + history [+ compaction summary])
├─ stream deltas ─────────────────────────── emit `delta` / `thinkingDelta` events
├─ if assistant emits tool calls:
│   ├─ gate each call (approve / deny / abort)
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

**Failure-handling contract**

- Stream error mid-turn → keep partial text, `stop_reason = "error"`, offer retry.
- Tool error → convert to a tool-error result so the model can self-correct; never throw.
- Abort → signal, close stream, persist partial; idempotent.
- Runaway guard → hard cap on consecutive tool rounds.

---

## Data model (SQLite)

Schema lives in `db.rs` `SCHEMA` (idempotent `CREATE TABLE IF NOT EXISTS`), with
best-effort `ALTER TABLE ... ADD COLUMN` migrations in `open()`.

```sql
conversations(
  id, title, model, system_prompt, compaction_summary,
  last_reflected_index,          -- memory-reflection watermark
  imported,                      -- 1 = came from an export; backfill-only
  created_at, updated_at)

messages(
  id, conversation_id, role,     -- user | assistant | tool | memory
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
```

- **`role`:** `memory` rows are consolidation notes. `build_history_messages`
  surfaces them to the model as lightweight `system` messages (so they're part of
  the cacheable prefix in both live and reflection turns), and the UI renders them
  as a centered note, not a turn.
- **Tool-turn persistence:** assistant tool-request rows are
  `{"tool_calls":[{id,name,arguments}]}`; tool-result rows (`role="tool"`) are
  `{"tool_call_id","name","error","output"}`. Frontend parsers in `App.tsx`
  (`parseToolCallList`/`parseToolResult`) and `groupTurns`/`buildEntries` must stay
  in sync.
- **Pricing** is resolved lazily and cached: **override → fetched `model_prices` →
  bundled fallback → default**. Cost is frozen on the message at the rates in
  effect, so switching models never re-prices past turns.
- **Capabilities** ride along in the same cache: `attachment` and `modalities`
  from models.dev, surfaced on `ModelInfo.vision` via `ModelMeta::vision()`,
  which prefers the explicit `modalities.input` list and falls back to the
  coarser `attachment` flag (they agree on ~93% of the catalog; `attachment`
  also covers models that take documents but not images). The composer warns
  when an image is attached to a model that cannot see it, rather than letting
  the provider drop it or reject the turn. `vision` is `None` when the provider
  isn't in the catalog, so an unknown model is never warned about.

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
  content)` replaces a file for curation. No `importance`/`category`.
- **Live vs idle:** the tool list is identical for live, reflection, and
  extraction turns (so the prompt-cache prefix matches). `run_tool_loop`'s
  `ToolMode` enforces access by mode: `Live` may `save_memory`/`read_memory` but
  **not** `write_memory`; `Reflection` may use all three memory tools but is
  refused host/web tools; `Extract` advertises no tools and refuses any call.
  This keeps the chatting model from clobbering curated files without splitting
  the tool list.
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
- **Eligibility:** a chat becomes due only when a message is inserted
  (user/assistant/tool). Merely opening a chat does not touch `updated_at`.
- **Scope:** the Memory tab's **Consolidate this chat** button is per-chat; the
  scheduler sweeps all chats over time. Memory files themselves are global.
- **Explicit vs inferred preferences:** the Settings **"Explicit user preferences"**
  field is injected at the top of every system prompt and labelled authoritative
  (takes precedence over inferred memory); the memory block and reflection prompt
  both say not to duplicate or contradict it. Memory files hold what the assistant
  *infers*.
- **Import** (`import.rs`): reads an Anthropic-format export — either a single
  `conversations.json` holding an array, or a directory of per-conversation JSON
  files. Two properties make it safe over a large archive. **Timestamps are
  preserved**: nothing goes through `insert_message_full`, which would stamp
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
  an abort fail immediately.
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

## Tool surface

Introduced through the same loop, so they're visible, gateable, and aborted like
everything else.

```rust
// Approval-gated (local side effects / local data)
bash(command)                     // runs in the conversation's persistent shell
read_file(path)
write_file(path, content)

// Memory — ungated, app-local. All three are in the shared tool list; access
// is enforced by mode (write_memory is refused during live turns).
save_memory(content, path)        // append; path required
read_memory(path)
write_memory(path, content)       // replace (curation)

// Web search — reused Brave MCP server, read-only (ungated)
web_search(query, count?)
```

The tool list is byte-identical for live and reflection turns so the prompt-cache
prefix matches; `run_tool_loop` refuses disallowed calls by mode instead of
splitting the list (live: no `write_memory`; reflection: no host/web tools).

**MCP integration:** `tools.rs` `McpClient` spawns the official
`@brave/brave-search-mcp-server` (`node .../dist/index.js --transport stdio`,
installed under `~/.config/opencode/node_modules`) once and keeps a long-lived
`rmcp` stdio client (the `RunningService`, not just its `Peer` — dropping the
service closes the transport). Re-spawned once on failure. The app advertises a
stable `web_search` tool that proxies to the server's `brave_web_search`.
`BRAVE_API_KEY` is read from the process environment or the sibling
`~/.config/opencode/mcp/.env` and passed to the child; `web_search` is offered
only when the server entry and a key are both present.

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
stability across streamed updates, auto-scroll, bottom anchoring, the live
activity timeline + tool approval, and memory consolidation. Screenshots →
`e2e/screenshots/` (gitignored).

- **Prereq:** `cargo install tauri-driver --version 2.0.6 --locked` (the **2.x**
  release is the Tauri v2 one; 3.0.0-alpha targets Tauri v3).
- The runner builds the debug binary **only if missing** — after changing Rust
  code, `cargo build` first so the harness picks it up.
- It launches the app with an **isolated `XDG_DATA_HOME`** (seeded with the mock
  endpoint) and writes a throwaway keychain key **only if none exists**.
- **Input / clicks:** on this WebKitGTK build, `Element Click` / `Send Keys` /
  W3C Actions return `unsupported operation` (`/usr/bin/WebKitWebDriver` is the
  GTK4 build while the app runs on `webkit2gtk-4.1`). Clicks are dispatched via
  in-page `el.click()`; the click-churn regression is covered by a DOM-node
  identity assertion. See `e2e/README.md`.

---

## File layout

```
/home/rp/chat
├─ AGENTS.md           # this file (single source of truth)
├─ index.html
├─ vite.config.ts      # Vite + vite-plugin-solid + @tailwindcss/vite
├─ package.json
├─ e2e/                # GUI end-to-end harness (mock LLM + tauri-driver/WebKitWebDriver)
│  ├─ run.sh           # orchestrates mock + vite + tauri-driver + e2e.mjs
│  ├─ e2e.mjs          # WebDriver client + assertions
│  ├─ mock-llm.mjs     # scripted OpenAI-compatible SSE server
│  └─ README.md
├─ src/
│  ├─ index.tsx        # entry; imports ./index.css (Tailwind)
│  ├─ index.css        # Tailwind import + typography + class-based dark variant
│  ├─ App.tsx          # UI shell (sidebar, dialogs, messages, timeline, theme)
│  └─ lib/
│     ├─ api.ts        # typed invoke wrappers over Tauri commands
│     └─ Markdown.tsx  # marked + DOMPurify renderer (prose + dark:prose-invert)
└─ src-tauri/
   ├─ tauri.conf.json  # v2 config (productName, window, devUrl, frontendDist)
   ├─ Cargo.toml       # tauri, serde, rusqlite [bundled], uuid, async-openai, keyring, rmcp, reqwest, tokio, tokio-stream
   ├─ capabilities/default.json
   ├─ build.rs
   └─ src/
      ├─ main.rs       # thin entry -> chat_app_lib::run()
      ├─ lib.rs        # Builder, setup (SQLite + config + MCP + registries + reflection), invoke_handler
      ├─ db.rs         # Db state, schema/migration, conversation/message commands + helpers
      ├─ config.rs     # AppConfig, config.json persistence, legacy key migration
      ├─ secrets.rs    # OS-keychain API key storage (keyring crate) + has/set_api_key
      ├─ models.rs     # list_models (models.dev catalog, /models fallback), chat filter, per-baseUrl cache
      ├─ tools.rs      # tool specs, host executor, approval registry, MCP web_search
      ├─ shell.rs      # persistent per-conversation bash sessions (ShellRegistry)
      ├─ memory.rs     # Plain-Markdown memory: core files, listing, save/read/write, migration
      ├─ import.rs     # Anthropic-export importer (timestamp-preserving, backfill-only)
      ├─ reflection.rs # Idle memory-consolidation scheduler + per-conversation pass
      ├─ attachments.rs # read paths -> Attachment (image data URL / text / metadata)
      ├─ pricing.rs    # pricing + models.dev metadata cache; overrides -> cache -> bundled
      ├─ chat.rs       # stream_chat/stop_chat, raw SSE, run_tool_loop, StreamRegistry, StreamEvent
      └─ chat/
         └─ tests.rs   # chat-loop tests (mock SSE: tools, reasoning, abort, usage)
```

---

## Conventions

### Frontend (SolidJS)

- Use `createSignal` / `createEffect` / `For` / `Show` from `solid-js`. No class components.
- All Tauri calls go through typed wrappers in `src/lib/api.ts`; components never call `invoke` directly.
- Style with Tailwind utility classes only (no CSS modules). Tailwind scans `src/**`.
- Kobalte components are namespaced: `import { Dialog } from "@kobalte/core/dialog"` then `<Dialog.Root>`, `<Dialog.Trigger>`, etc.

### Backend (Rust / Tauri)

- New module → `src-tauri/src/<x>.rs`, declare `mod x;` in `lib.rs`, register commands in `tauri::generate_handler![...]`.
- Commands take `tauri::State<'_, Db>` for DB access; lock the `Mutex<Connection>` briefly.
- Errors return `Result<T, String>`.
- Tauri v2 maps JS `camelCase` args to Rust `snake_case` params automatically (`conversationId` → `conversation_id`).
- `StreamEvent` is serialized straight to the frontend. Enum-level `rename_all` only renames **variants**, so fields need `rename_all_fields = "camelCase"` (e.g. `call_id` → `callId`); keep the `StreamEvent` union in `src/lib/api.ts` in sync.
- `bash` runs in a persistent per-conversation shell; never assume a fresh process/cwd/env. `read_file`/`write_file` are direct host calls.
- Chat-loop tests live in `src/chat/tests.rs`; add loop/tool tests there.

### SQLite

- DB file: app data dir `chat.sqlite`. On Linux: `~/.local/share/com.rp.chat/chat.sqlite`.
- Add tables/columns to `db.rs` `SCHEMA`; for existing DBs add a best-effort `ALTER TABLE ... ADD COLUMN` in `open()` and ignore the duplicate-column error.

---

## Environment gotchas

- **Brave MCP:** the official `@brave/brave-search-mcp-server` (npm), spawned via
  `rmcp` from `~/.config/opencode/node_modules/@brave/brave-search-mcp-server/dist/index.js`.
  `BRAVE_API_KEY` is read from the environment or `~/.config/opencode/mcp/.env`
  and passed to the child process. Needs a separate `BRAVE_API_KEY` from the LLM
  provider key.
- **Fireworks is the default endpoint** (`https://api.fireworks.ai/inference/v1`) but the client is generic; the user enters `baseUrl` + `apiKey` in Settings. Fireworks needs a `fw_` key; Brave needs a separate `BRAVE_API_KEY`.
- **Rust builds are slow** on first run (Wry + bundled SQLite). `cargo check` is faster for iteration.
- **GUI needs a display:** use `npm run tauri dev` from the desktop session; scripted checks use `npm run e2e` (same requirement).
- **Persistent shell:** `bash` runs in a long-lived `bash` per conversation; sentinel-delimited output, 60s timeout (or `exit`) kills/resets. stdin is `/dev/null` unless the command uses a heredoc.
- **`delete_conversation` is async** (it awaits clearing that conversation's shell).
- **Keychain entry:** service `com.rp.chat`, user `api_key`. A legacy plaintext `apiKey` in `config.json` auto-migrates on first launch.

---

## Next / open items

**Milestone 7 — context** (the last functional gap):

1. Reserve + **near-limit banner**: "near context limit — compact or new chat" (non-blocking; never silently compact).
2. Per-conversation compaction: summarize older messages into `conversations.compaction_summary` and feed that into `build_messages` instead of the full history.
3. Optional: sidebar cost readout; virtualized message list.

**V2 (deferred):** conversation recall via a vector index (sqlite-vec + embedder;
behind a retrieval seam); container/sandbox tool execution; richer cross-conversation
memory conflict resolution.

**Also deferred:** the `CompatConfig` layer for provider quirks (thinking field,
`max_tokens`, developer-vs-system role). Default OpenAI-shaped behavior is fine
until a provider breaks it; the first knob (`echo_reasoning_content`) has landed.

**Known gaps:** the per-conversation scratch workspace for `read_file`/`write_file`
is not implemented — host tools operate on real paths with per-call approval.
