# AGENTS.md — Pi Chat

This is the working document for agents (and humans) building **Pi Chat**, a small desktop AI chat app.

**Read this first.** It tells you where things are, how to build, what's done, and what's next. The broader product spec/decisions live in [`PLANS.md`](./PLANS.md) — this file is the _project-operations_ layer: status, commands, conventions, and next-step instructions.

---

## What it is

A general-purpose AI chat app (not a coding agent) with:

- An expandable sidebar listing past conversations
- Streaming assistant replies (OpenAI-compatible API)
- Optional tools (`read_file`/`write_file`; `bash` runs in a **per-conversation persistent shell** so `cd`/`export` survive; `web_search` via Brave)
- Reasoning traces from thinking models (inline "Thought" bar → popup)
- Dark mode by default (header light/dark toggle)
- Markdown-file memory + per-conversation compaction
- A price meter
- File attachments (native picker + drag-drop; images as vision parts, documents folded into the prompt)

**Stack:** SolidJS + Tailwind v4 + Kobalte + Tauri v2 (Rust). Backend logic lives in Rust (`src-tauri`); the UI is a Solid SPA.

**Key architectural stance (from PLANS.md):** no agent framework. We implement a stripped-down loop in Rust. We reuse `async-openai` for LLM streaming and `rmcp` to talk to an existing Brave MCP server. We do **not** import pi or any agent SDK.

---

## Current status

**Milestone 1 — scaffold: ✅ DONE**

The stack is wired and both halves build. What exists today:

| Area         | What's there                                                                                                             |
| ------------ | ------------------------------------------------------------------------------------------------------------------------ |
| App shell    | `src/App.tsx` — sidebar (conversation list) + main chat area, styled with Tailwind, uses Kobalte `Dialog` for "New chat" |
| Frontend API | `src/lib/api.ts` — typed `invoke` wrappers + `Conversation`/`Message` types                                              |
| Tailwind     | v4 via `@tailwindcss/vite`, imported in `src/index.css`                                                                  |
| SQLite       | `src-tauri/src/db.rs` — rusqlite (bundled), schema for `conversations`/`messages`/`model_prices`, migration on open      |
| Commands     | `list_conversations`, `create_conversation`, `list_messages`, `add_message` registered in `src-tauri/src/lib.rs`         |
| Config       | `src-tauri/tauri.conf.json` — product `pi-chat`, window "Pi Chat"                                                        |

**Verified:** `npm run build` (Vite, succeeded) and `cargo build` in `src-tauri` (succeeded, ~2m51s).

**Not yet run:** the GUI itself. `npm run tauri dev` will compile + launch the window; it needs a display session (won't work headless). Run it once to confirm the SQLite list + "New chat" create flow end-to-end.

---

**Milestone 2 — streaming loop: ✅ DONE**

An end-to-end streaming turn works against a configurable OpenAI-compatible endpoint (default Fireworks). `cargo build`, `cargo clippy`, and `cargo test` (incl. a mock-SSE streaming integration test) all pass; `npm run build` passes.

| Area              | What's there                                                                                                                                               |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| LLM client        | `src-tauri/src/chat.rs` — `async-openai` streaming via `Client.chat().create_stream`; `event-stream` deltas emitted to the frontend over a Tauri `Channel` |
| Config            | `src-tauri/src/config.rs` — `get_config`/`set_config`, persisted as JSON in the app data dir (`config.json`). Plaintext for now (keychain is Milestone 3)  |
| Models            | `src-tauri/src/models.rs` — `list_models` calls `GET /models` on the configured base URL                                                                   |
| Streaming command | `chat::stream_chat` — reads conversation + history, persists the user turn, streams, persifies the assistant turn with `usage` + `stop_reason`             |
| Abort             | `chat::stop_chat` — sets a per-conversation cancel flag; partial text is preserved (`stop_reason = "aborted"`)                                             |
| Failure contract  | Stream error keeps partial text (`stop_reason = "error"`); missing key/model returns a command error the UI surfaces                                       |
| Frontend          | `src/App.tsx` — chat bar wired (Send / Stop), live assistant bubble via `Channel`, minimal Settings dialog (baseUrl/apiKey/model + model picker)           |

**Verified:** `cargo build`, `cargo clippy --all-targets`, and `cargo test --lib` (5 tests) succeed in `src-tauri`; `npm run build` succeeds. Still needs a live GUI run from a display session.

---

**Milestone 3 — config + models polish: ✅ DONE**

API key now lives in the OS keychain (never on disk, never crosses the IPC boundary), the header has a working model picker backed by a cached, chat-filtered model list, and Settings persists on close with inline errors.

| Area                  | What's there                                                                                                                                                                                         |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Secrets               | `src-tauri/src/secrets.rs` — `keyring` v4 (Secret Service on Linux / Keychain on macOS / Credential Manager on Windows). Commands `has_api_key`, `set_api_key` (empty/None → delete)                  |
| Config                | `config.rs` — `AppConfig` is now `base_url`/`model` only; a legacy plaintext `apiKey` in `config.json` is migrated to the keychain on first load and stripped from the file                          |
| Key stays server-side | `list_models` and `stream_chat` read the key from the keychain in Rust; the frontend only ever sees a boolean. (`tauri-plugin-keyring` was skipped deliberately: its JS-facing API would put the key through the frontend, violating the PLANS decision.) |
| Model picker          | Header `<select>` lists chat-capable models from `list_models`, shows the current model even when the list is empty/fetch failed; changing it persists immediately via `set_config`                   |
| Filtering + cache     | `models.rs` — heuristic denylist for clearly non-chat ids (embedding/whisper/tts/flux/rerank/…); raw list cached in memory per base URL (`ModelCache` state), `refresh: true` re-fetches              |
| Settings dialog       | Persists on close (`onOpenChange(false)` → `persistSettings`); API key field is write-only (placeholder shows "saved in keychain"), "Remove saved key" button; "Load" saves the draft first, then fetches so connection errors show inline; post-close errors show as a header banner |

**Verified:** `cargo build`, `cargo clippy --all-targets`, `cargo test --lib` (11 tests, incl. a live keychain roundtrip) in `src-tauri`; `npx tsc --noEmit` + `npm run build` for the frontend. Still needs a live GUI run from a display session.

---

**Milestone 4 — conversations polish: ✅ DONE**

| Area            | What's there                                                                                                                                                        |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Auto-title      | `db.rs` `insert_message` — first user message titles an untitled conversation (`derive_title`: first line, 60 chars)                                                 |
| updated_at      | Maintained on every insert; sidebar sorts by it (list query already ordered)                                                                                           |
| Rename/delete   | Commands `rename_conversation`, `delete_conversation` (FK cascade via `PRAGMA foreign_keys = ON`); sidebar hover ✎ / ✕ with inline confirm                               |
| Search          | Command `search_conversations` — case-insensitive substring over title AND message content; sidebar search box (debounced)                                              |
| Markdown render | `src/lib/Markdown.tsx` — `marked` + DOMPurify + `@tailwindcss/typography` (prose); assistant bodies render markdown, user stays plain text                              |
| Auto-create     | Sending with no active conversation creates one on the fly; sidebar refreshes after each turn (new titles/bumped chats)                                                |

**Verified:** `cargo test --lib` (12 tests), clippy clean, `tsc --noEmit`, `npm run build`.

---

**Milestone 5 — tools: ✅ DONE**

The chat command now runs a capped tool loop; tools are visible, gateable, and persisted like any other turn.

| Area             | What's there                                                                                                                                                                                                                                            |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Tool loop        | `chat.rs` `stream_chat` — up to `MAX_TOOL_ROUNDS` (8) rounds; streamed tool-call fragments accumulated by index; per-round text persisted, usage summed across rounds; cap reached → `stop_reason = "length"`                                             |
| Host tools       | `tools.rs` — `bash` (60s timeout, stdout+stderr+exit, output truncated at 20k chars), `read_file`, `write_file`; all errors become tool-error text the model can self-correct (never throws)                                                              |
| Approval gate    | `ApprovalRegistry` + `approve_tool`/`deny_tool` commands — stream emits a `toolCall` event, awaits a oneshot; bash/read/write gated, Stop denies pending calls; UI shows an Approve/Deny card per gated call                                              |
| web_search       | `tools.rs` `McpClient` — long-lived `rmcp` stdio client spawning `node ~/.config/opencode/mcp/brave-search.mjs` (ungated); kept alive via stored `RunningService` (dropping the peer alone closes the transport!), re-spawned once on failure            |
| History format   | Assistant tool-request rows: `{"tool_calls":[{id,name,arguments}]}`; tool-result rows (role `tool`): `{"tool_call_id","name","error","output"}` — `build_messages` converts both back to request shapes so multi-round history replays correctly          |
| Events/UX        | `StreamEvent` gained `toolCall`/`toolResult`; live cards show args + Approve/Deny while streaming; persisted tool turns render as collapsible result blocks and "assistant wants X" cards                                                                   |

**Verified:** `cargo build`, `cargo clippy --all-targets`, `cargo test --lib` (22 tests at the time — incl. mock-SSE tool-call fragment accumulation and a live MCP handshake/list_tools against the real Brave server), `tsc --noEmit`, `npm run build`. GUI-exercised during M5.5 below.

---

**Milestone 5.5 — UX, reasoning traces, persistent shell: ✅ DONE**

Post-M5 additions driven by live GUI use. `cargo test --lib` is now 31 tests; clippy, `tsc --noEmit`, and `npm run build` clean.

| Area             | What's there                                                                                                                                                                                                                                                                                    |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Layout/theme     | `App.tsx` — user messages right-aligned; class-based dark mode (`@custom-variant dark` in `index.css`) defaults to dark with a header Light/Dark toggle persisted in `localStorage`; `Markdown.tsx` adds `dark:prose-invert`                                                                     |
| Reasoning traces | The chat command parses the raw SSE itself (not async-openai's typed stream) so provider `reasoning_content` / thinking content-parts aren't dropped. `StreamEvent::ThinkingDelta` streams live; `messages.thinking` stores the trace; the UI shows an inline "Thinking…/Thought" bar opening a live-updating popup |
| Persistent shell | `shell.rs` — one long-lived `bash --noprofile --norc` per conversation; `cd`/`export`/functions persist across calls. Sentinel-delimited output, `exec 2>&1` merges stderr, 60s timeout kills+respawns, `kill_on_drop`; registry keyed by conversation id; `delete_conversation` (now async) clears it |
| Chat-loop tests  | Tests moved to `src/chat/tests.rs` (`#[cfg(test)] mod tests;`). Mock-SSE server covers deltas/usage, reasoning capture, tool-call fragments, approved + denied tool rounds, thinking persistence, abort-with-partial, and SSE framing                          |
| Bug fixes        | `StreamEvent` fields now use `rename_all_fields = "camelCase"` — without it `callId` arrived `undefined` and Approve silently hung. Tool args render single-field values raw (bash → the command) and multi-field as pretty JSON. Whitespace-only preambles are no longer persisted/rendered |

**Verified:** `cargo test --lib` (31), `cargo clippy --all-targets` clean, `tsc --noEmit` + `npm run build`. GUI-exercised live: streaming, tool Approve/Deny (`pwd`/`date` via bash), the reasoning "Thought" bar, and dark mode. Persistent shell not yet GUI-tested (needs an app restart).

---

**Milestone 6 — memory: ✅ DONE**

Markdown-file memory + the two memory tools + a Memory tab. `cargo test --lib` is now 42 tests; clippy, `tsc --noEmit`, and `npm run build` clean.

| Area             | What's there                                                                                                                                                                                                                     |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Memory files     | `memory.rs` `MemoryState` — `memory/` dir in app data with `index.md` (auto-generated titles+summaries) + `preferences.md`/`identity.md`/`goals.md`/`notes.md`; entries are YAML-frontmatter blocks; writes are atomic (temp + rename) |
| save_memory      | `save_memory(content, path?, category?, importance)` — explicit `path` targets any existing/new `.md` file (overrides `category`); else category→canonical-file routing. Dedupe-by-title merge/update, summary auto-derived, `index.md` regenerated (indexes custom files too) |
| read_memory      | `read_memory(path)` — on-demand body reads (frontmatter stripped); file names sanitized (no `/`, `..`, non-`.md`) so reads stay inside the memory dir                                                                                |
| Context injection| `chat.rs` `system_prompt_with_memory` appends the bounded index block to every system prompt; bodies only enter context via `read_memory`                                                                                          |
| Commands         | `list_memory_files`, `write_memory_file`, `delete_memory_file` (registered in `lib.rs`)                                                                                                                                             |
| Memory tab       | Header "Memory" dialog — file list + editor, explicit Save, delete-with-confirm; `index.md` is read-only and regenerated on save                                                                                                   |
| Gating           | Both memory tools are **ungated** (confirmed with user); writes stay visible via the Memory tab and the injected index                                                                                                             |

**Verified:** `cargo test --lib` (42, incl. save/dedupe/category-routing/parse-roundtrip/traversal-rejection and system-prompt injection), `cargo clippy --all-targets` clean, `tsc --noEmit` + `npm run build`. GUI-exercise still pending a display session.

---

**Context + price counters: ✅ DONE** (counters only — M7/M8 remainder below)

Per-model context window and token rates resolve from a bundled table with user overrides; the header shows context used/window and the conversation's estimated cost.

| Area             | What's there                                                                                                                                                                                                                     |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pricing data     | `pricing.rs` — bundled pattern→(context window, $/1M in/out) fallback table; `get_pricing(modelId)` resolves **override → fetched cache → bundled → default**                                                                      |
| Price fetching   | `refresh_pricing` fetches the free public **models.dev** catalog (`api.json`), matches the provider by the configured base URL, and upserts every model's `cost` + `limit.context` into `model_prices` (no API key needed)          |
| Context tracking | `chat.rs` `track_context` stores the final tool round's `prompt + completion` as `usage.context_tokens`; `sum_usage` also normalizes provider cached-token counts into `usage.cached_tokens`/`cache_write_tokens`                     |
| Overrides        | `AppConfig.modelOverrides` persisted in `config.json`; Settings "Pricing & context" fields + "Reset to default"                                                                                                                    |
| Header readout   | `tokens used / window` (color at 75%/90%) + conversation cost; "—" when rates unknown; tooltip shows rates + an override note                                                                                                      |
| Settings refresh | "Refresh prices" button → `refreshPricing()`, reports "Updated N models from `<provider>`"; `Pricing.source` (`override`/`fetched`/`bundled`/`default`) drives the caption                                                |
| Cost             | sum over assistant messages of `uncached×input + cached×cache_read + cache_write×cache_write + completion×output`; unknown cache rates fall back to the input rate (budget: single active model)                                      |
| Cache rates      | `cache_read`/`cache_write` flow override → fetched `model_prices` → (bundled has none); editable in Settings and shown in the header tooltip                                                                                        |

**Verified:** `cargo test --lib` (51, incl. models.dev provider matching/parse, cached-token normalization across provider shapes, bundled/override/partial-override resolution, and `context_tokens`), `cargo clippy --all-targets` clean, `tsc --noEmit` + `npm run build`. Live fetch manually validated against models.dev (Fireworks provider `api` matches the configured base URL; 22 models incl. all current Fireworks ids).

---

**User preferences, model identity, real model names + thinking levels: ✅ DONE**

| Area             | What's there                                                                                                                                                                                                                                                        |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Preferences      | `AppConfig.preferences` (persisted in `config.json`); Settings "User preferences" textarea. Injected at the **top** of every system prompt (`User preferences:\n…`).                                                                                                  |
| Model identity   | `chat.rs` `build_system_prompt` appends `You are currently running as the model "<name>"`; the name is resolved from the models.dev cache with the id as fallback.                                                                                                   |
| Real model names | **The picker list comes from models.dev** for a matched endpoint (`pricing::provider_model_ids`), so every entry has a real `name` (plus context/reasoning). `models::list_models` refreshes the catalog on demand (`ensure_cached`), caches ids in memory per base URL, and falls back to the endpoint's own `GET /models` only when models.dev has no matching provider (self-hosted). `ModelInfo` carries `name`; header picker and Settings render `name ?? id`. |
| Thinking levels  | `pricing::thinking_options(modelId)` → `{options, source, supportsReasoning}`. Uses models.dev `reasoning_options` `effort.values` when present; otherwise the OpenAI levels (`minimal`/`low`/`medium`/`high`). Non-reasoning models expose none (selector hidden). Header selector persists `AppConfig.thinkingLevel` immediately; non-empty → `reasoning_effort` on each request. |
| Cache schema     | `model_prices` gained `name`/`reasoning`/`reasoning_options` (+`ALTER TABLE` migration); `has_cached` requires names so pre-migration rows re-fetch once.                                                                                                              |

**Verified:** `cargo test --lib` (55, incl. thinking-option derivation and system-prompt ordering), `cargo clippy --all-targets` clean, `tsc --noEmit` + `npm run build`. GUI-exercise pending a display session.

---

**Settings UX + accurate per-turn pricing + file attachments: ✅ DONE**

| Area              | What's there                                                                                                                                                                                                                                                                                                                        |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Settings close    | `persistSettings` reads the persisted config back and writes only `baseUrl` + `preferences` (+ a typed key), so pressing **Done** never changes the active model. The header picker owns the model and persists immediately. The catalog is re-fetched only when the base URL or key changed.                                            |
| Settings layout   | Model loader, manual "Pricing & context" editor, and the "Available models" list removed. Dialog is `max-h-[85vh] overflow-y-auto` (shrinks + scrolls).                                                                                                                                                                               |
| Catalog fetch     | Startup shows cached prices/models immediately, then a background `list_models(true)` replaces them when models.dev resolves (`catalogVersion` re-runs pricing/thinking). No periodic polling.                                                                                                                                       |
| Model picker      | Options are derived (`modelOptions`) with the active model always first; a `ref` + effect re-applies `select.value` after the list is rebuilt. WebKit clears a `<select>` when its selected `<option>` is removed, and Solid's `value` binding doesn't re-run because `model` is unchanged — hence the explicit re-sync. Never disabled/grayed while loading. |
| Preferences/model | `build_system_prompt` injects `User preferences:` at the top (empty → omitted) and the active-model line; a request-body test asserts both reach the provider.                                                                                                                                                                       |
| Pricing accuracy  | `pricing::resolve_for` + `cost_of_usage` bill uncached/cache-read/cache-write/output buckets without overlap. `run_chat_turn` **freezes** the turn's cost into `usage.cost` (priced with the model actually used), so switching models never retroactively re-prices past turns. The header sums frozen costs; legacy turns without `cost` are priced per-message-model via `pricingByModel`. |
| Attachments       | Paperclip button (left of the composer) opens the native picker via `tauri-plugin-dialog`; native drag-drop via `getCurrentWebview().onDragDropEvent` with a drop overlay. `attachments.rs` `read_attachments` reads paths on the blocking pool (images → base64 data URL; text files → contents; else metadata). Pending files preview above the composer (image thumbnail, or doc chip + size + remove). Sent files render inside the user bubble and persist in `messages.attachments`. Text files fold into the message; images go as multimodal `image_url` parts. |

**Verified:** `cargo test --lib` (65, incl. base64 vectors, attachment read/persist/send, frozen cost, request body), `cargo clippy --all-targets` clean, `tsc --noEmit` + `npm run build`. GUI-exercise pending (native picker + drag-drop can't run headless).

**Deps/config added:** `tauri-plugin-dialog` (Rust) + `@tauri-apps/plugin-dialog` (JS); capability `dialog:default`.

---

## Commands

Frontend / Tauri tasks, run from the repo root (`/home/rp/chat`):

```bash
npm install            # install JS deps (first time)
npm run dev            # Vite dev server only (no Tauri window)
npm run build          # Vite production build (outputs to dist/)
npm run tauri dev      # compile Rust + launch the app window
npm run tauri build    # compile + bundle an installer
```

Rust-only:

```bash
cd src-tauri
cargo check           # fast type/borrow check
cargo build           # full debug build (slow first time: Wry + bundled SQLite)
```

---

## File layout

```
/home/rp/chat
├─ PLANS.md            # product spec + decisions + v1/v2 scope + build order
├─ AGENTS.md           # this file (project operations / status / next steps)
├─ index.html
├─ vite.config.ts      # Vite + vite-plugin-solid + @tailwindcss/vite
├─ package.json
├─ src/
│  ├─ index.tsx        # entry; imports ./index.css (Tailwind)
│  ├─ index.css        # Tailwind import + typography + class-based dark variant
│  ├─ App.tsx          # UI shell (sidebar, dialogs, messages, tool cards, reasoning popup, theme)
│  └─ lib/
│     ├─ api.ts        # typed invoke wrappers over Tauri commands
│     └─ Markdown.tsx  # marked + DOMPurify renderer (prose + dark:prose-invert)
└─ src-tauri/
   ├─ tauri.conf.json  # v2 config (productName, window, devUrl, frontendDist)
   ├─ Cargo.toml       # tauri, serde, serde_json, rusqlite [bundled], uuid, async-openai, keyring, rmcp, reqwest, tokio, tokio-stream
   ├─ capabilities/default.json
   ├─ build.rs
   └─ src/
      ├─ main.rs       # thin entry -> chat_app_lib::run()
      ├─ lib.rs        # Builder, setup (SQLite + config + MCP + registries), invoke_handler
      ├─ db.rs         # Db state, schema migration, conversation/message commands + helpers
      ├─ config.rs     # AppConfig (base_url/model/preferences/thinking_level), config.json persistence, legacy key migration
      ├─ secrets.rs    # OS-keychain API key storage (keyring crate) + has/set_api_key commands
      ├─ models.rs     # list_models (models.dev catalog, /models fallback), chat-capable filter, per-baseUrl cache
      ├─ tools.rs      # tool specs, host executor, approval registry, MCP web_search
      ├─ shell.rs      # persistent per-conversation bash sessions (ShellRegistry)
      ├─ memory.rs     # Markdown-file memory: entries, index.md, save/read tools, commands
      ├─ attachments.rs # read file paths → Attachment (image data URL / text / metadata); read_attachments command
      ├─ pricing.rs    # pricing + models.dev metadata cache (name/reasoning); overrides -> cache -> bundled; thinking_options; cost_of_usage
      ├─ chat.rs       # stream_chat/stop_chat, raw SSE parsing, StreamRegistry, StreamEvent
      └─ chat/
         └─ tests.rs   # chat-loop tests (mock SSE: tools, reasoning, abort, usage)
```

---

## Conventions

### Frontend (SolidJS)

- Use `createSignal` / `createEffect` / `For` / `Show` from `solid-js`. No class components.
- All Tauri calls go through typed wrappers in `src/lib/api.ts`; components never call `invoke` directly.
- Style with Tailwind utility classes only (no separate CSS modules). Tailwind scans `src/**` automatically.
- Kobalte components are namespaced: `import { Dialog } from "@kobalte/core/dialog"` then `<Dialog.Root>`, `<Dialog.Trigger>`, etc. Trigger defaults to a `<button>`; pass `class` to style.

### Backend (Rust / Tauri)

- Add a new module (e.g. `src-tauri/src/<x>.rs`), declare `mod x;` in `lib.rs`, register commands via `tauri::generate_handler![...]`.
- Commands take `tauri::State<'_, Db>` for DB access; lock the `Mutex<Connection>` for the duration of the command (short-lived, sequential).
- Errors return `Result<T, String>`.
- Tauri v2 maps JS `camelCase` args to Rust `snake_case` params automatically (e.g. `conversationId` → `conversation_id`).
- `StreamEvent` is serialized straight to the frontend. Enum-level `rename_all` only renames **variants**, so multi-word fields need `rename_all_fields = "camelCase"` (e.g. `call_id` → `callId`); keep the `StreamEvent` union in `src/lib/api.ts` in sync.
- `bash` runs in a persistent per-conversation shell (`shell.rs`); never assume a fresh process/cwd/env. `read_file`/`write_file` remain direct host calls.
- Chat-loop tests live in `src/chat/tests.rs` (`#[cfg(test)] mod tests;`); add loop/tool tests there.

### SQLite

- DB file lives in the app data dir at `chat.sqlite`. On Linux: `~/.local/share/com.rp.chat/chat.sqlite`.
- Schema is maintained in `db.rs` `SCHEMA` (idempotent `CREATE TABLE IF NOT EXISTS`). Add new tables/columns there.
- `messages.index` is a per-conversation mono­tonic int (assignment in `add_message`). `usage`/`stop_reason` are JSON/text scalars; `thinking` holds the model's reasoning trace (nullable); `attachments` holds the JSON array of file attachments (nullable; `insert_message_full` writes it, `insert_message` passes `None`).
- Adding a column to an existing DB: append it to `SCHEMA` and add a best-effort `ALTER TABLE ... ADD COLUMN` in `open()` (ignore the duplicate-column error). See `thinking` in `db.rs`.

---

## Environment gotchas

- **Brave tool (later milestone):** we'll spawn `node ~/.config/opencode/mcp/brave-search.mjs` via `rmcp`. It needs `BRAVE_API_KEY`, which the script loads from its sibling `~/.config/opencode/mcp/.env` (already present on this machine). The script resolves `@modelcontextprotocol/sdk` from `~/.config/opencode/node_modules` — it works because Node walks up from the script's own directory. Don't relocate the script.
- **Fireworks is the default endpoint** (`https://api.fireworks.ai/inference/v1`), but the client is a generic OpenAI-compatible client; user enters `baseUrl` + `apiKey` in Settings. Fireworks needs a `fw_` key; Brave needs a separate `BRAVE_API_KEY`.
- **Rust builds are slow** on first run (Wry + bundled SQLite). `cargo check` is faster for iteration; `cargo build` is what `tauri dev` uses.
- **No display in CLI:** don't try to run the Tauri window from a headless shell. Use `npm run tauri dev` from the user's desktop session.
- **Persistent shell:** `bash` calls run in a long-lived `bash` per conversation (`shell.rs`); output is sentinel-delimited and a 60s timeout (or `exit`) kills/resets the session. Command stdin is `/dev/null` unless it uses a heredoc, so don't expect interactive input.
- **`delete_conversation` is async** (it awaits clearing that conversation's shell), unlike the other DB commands.

---

## Next milestone

### Milestone 7 — context (from PLANS.md build order)

Reserve + near-limit banner + per-conversation compaction:

1. Derive a per-model context window (fallback ~200k) minus a reserve; compute remaining budget from the conversation's stored `usage`.
2. Show a **non-blocking** banner when near the limit: "near context limit — compact or new chat." User chooses; never silently compact.
3. Per-conversation compaction: summarize older messages into `conversations.compaction_summary` (the column already exists) and feed that summary into `build_messages` instead of the full history.
4. pi's compaction summary format is the reference; no framework import.

The context **counter** and **price meter** already landed (see the "Context + price counters" section above). Remaining from these milestones: the near-limit banner + compaction here in M7, and an optional sidebar cost readout. Then Milestone 9 — remaining polish (virtualized message list + thinking-level selector; markdown render, dark mode, reasoning traces, persistent shell, and memory already landed in M4/M5.5/M6).

Deferred from Milestone 3 (optional): the `CompatConfig` layer for provider quirks (thinking field, `max_tokens`, developer vs system role). Default OpenAI-shaped behavior is fine until a provider breaks it.

---

## Open items / notes for the next session

- **GUI status:** M2–M5.5 core flows are now GUI-exercised (streaming, Stop/partial, keychain, model picker, tool Approve/Deny, reasoning "Thought" bar, dark mode). Still to verify in the GUI: the persistent shell (`cd`/`export` across calls) after an app restart, and the file attachments flow (native picker + drag-drop) since it can't run headless.
- **Keychain entry:** service `com.rp.chat`, user `api_key` (Secret Service on Linux). A legacy plaintext `apiKey` in `config.json` auto-migrates on first launch.
- **Tool turn persistence format** (see M5 table): assistant `{"tool_calls":…}` / role `tool` `{"tool_call_id","name","error","output"}` JSON in `messages.content`. Frontend parsers in `App.tsx` (`parseToolCalls`/`parseToolResult`) must stay in sync.
- **MCP client lifetime gotcha:** keep the `RunningService` in state, not just the `Peer` — dropping the service closes the transport (bit us once; see `McpClient`).
- **Pricing source:** Fireworks has no usable public pricing API; `refresh_pricing` pulls from the free models.dev catalog (`api.json`) matched by base URL, caching into `model_prices`. Resolution is override → fetched → bundled. Bundled fallback rates are approximate.
- Scratch workspace (per-conversation dir for read/write tools) is not implemented; host tools operate on real paths with per-call approval, per the v1 PLANS note.
