# AGENTS.md — Pi Chat

This is the working document for agents (and humans) building **Pi Chat**, a small desktop AI chat app.

**Read this first.** It tells you where things are, how to build, what's done, and what's next. The broader product spec/decisions live in [`PLANS.md`](./PLANS.md) — this file is the *project-operations* layer: status, commands, conventions, and next-step instructions.

---

## What it is

A general-purpose AI chat app (not a coding agent) with:
- An expandable sidebar listing past conversations
- Streaming assistant replies (OpenAI-compatible API)
- Optional tools (read/write/bash, gated; `web_search` via Brave)
- Markdown-file memory + per-conversation compaction
- A price meter

**Stack:** SolidJS + Tailwind v4 + Kobalte + Tauri v2 (Rust). Backend logic lives in Rust (`src-tauri`); the UI is a Solid SPA.

**Key architectural stance (from PLANS.md):** no agent framework. We implement a stripped-down loop in Rust. We reuse `async-openai` for LLM streaming and `rmcp` to talk to an existing Brave MCP server. We do **not** import pi or any agent SDK.

---

## Current status

**Milestone 1 — scaffold: ✅ DONE**

The stack is wired and both halves build. What exists today:

| Area | What's there |
|---|---|
| App shell | `src/App.tsx` — sidebar (conversation list) + main chat area, styled with Tailwind, uses Kobalte `Dialog` for "New chat" |
| Frontend API | `src/lib/api.ts` — typed `invoke` wrappers + `Conversation`/`Message` types |
| Tailwind | v4 via `@tailwindcss/vite`, imported in `src/index.css` |
| SQLite | `src-tauri/src/db.rs` — rusqlite (bundled), schema for `conversations`/`messages`/`model_prices`, migration on open |
| Commands | `list_conversations`, `create_conversation`, `list_messages`, `add_message` registered in `src-tauri/src/lib.rs` |
| Config | `src-tauri/tauri.conf.json` — product `pi-chat`, window "Pi Chat" |

**Verified:** `npm run build` (Vite, succeeded) and `cargo build` in `src-tauri` (succeeded, ~2m51s).

**Not yet run:** the GUI itself. `npm run tauri dev` will compile + launch the window; it needs a display session (won't work headless). Run it once to confirm the SQLite list + "New chat" create flow end-to-end.

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
│  ├─ index.css        # @import "tailwindcss";
│  ├─ App.tsx          # UI shell (sidebar, dialog, message area)
│  └─ lib/api.ts       # typed invoke wrappers over Tauri commands
└─ src-tauri/
   ├─ tauri.conf.json  # v2 config (productName, window, devUrl, frontendDist)
   ├─ Cargo.toml       # tauri, serde, serde_json, rusqlite [bundled], uuid
   ├─ capabilities/default.json
   ├─ build.rs
   └─ src/
      ├─ main.rs       # thin entry -> chat_app_lib::run()
      ├─ lib.rs        # Builder, setup (opens SQLite), invoke_handler
      └─ db.rs         # Db state, schema migration, Tauri commands
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

### SQLite
- DB file lives in the app data dir at `chat.sqlite`. On Linux: `~/.local/share/com.rp.chat/chat.sqlite`.
- Schema is maintained in `db.rs` `SCHEMA` (idempotent `CREATE TABLE IF NOT EXISTS`). Add new tables/columns there.
- `messages.index` is a per-conversation mono­tonic int (assignment in `add_message`). `usage`/`stop_reason` are JSON/text scalars.

---

## Environment gotchas

- **Brave tool (later milestone):** we'll spawn `node ~/.config/opencode/mcp/brave-search.mjs` via `rmcp`. It needs `BRAVE_API_KEY`, which the script loads from its sibling `~/.config/opencode/mcp/.env` (already present on this machine). The script resolves `@modelcontextprotocol/sdk` from `~/.config/opencode/node_modules` — it works because Node walks up from the script's own directory. Don't relocate the script.
- **Fireworks is the default endpoint** (`https://api.fireworks.ai/inference/v1`), but the client is a generic OpenAI-compatible client; user enters `baseUrl` + `apiKey` in Settings. Fireworks needs a `fw_` key; Brave needs a separate `BRAVE_API_KEY`.
- **Rust builds are slow** on first run (Wry + bundled SQLite). `cargo check` is faster for iteration; `cargo build` is what `tauri dev` uses.
- **No display in CLI:** don't try to run the Tauri window from a headless shell. Use `npm run tauri dev` from the user's desktop session.

---

## Next milestone

### Milestone 2 — streaming loop (from PLANS.md build order)

Goal: an end-to-end streaming turn against a hardcoded OpenAI-compatible (Fireworks) model.

Concrete tasks:
1. **Add `async-openai`** to `src-tauri/Cargo.toml` (`async-openai` crate; verify feature `stream`). Configure the client with `base_url` from config (Fireworks default for now) and a `fw_` API key.
2. **Config surface:** a Tauri command to get/set `baseUrl` + `apiKey` (stored in the OS keychain — `tauri-plugin-keyring` is the plan; or a config JSON for now). Add a minimal Settings dialog in the UI.
3. **Model list:** a command that calls `GET /v1/models` on the configured base URL, filters to chat-capable ids, returns them to the UI for a model picker.
4. **Streaming command:** a `#[tauri::command]` (or long-running task) that: takes the current conversation's messages, builds context (system + history) → converts to async-openai chat request (streaming) → **emits `delta` events** to the frontend via a Tauri channel/event → appends the assistant message to SQLite (persist partial on abort/error).
5. **Frontend:** subscribe to the `delta` event and append to a live message; wire the chat bar's send button; show a stop button that aborts.

Refer to the core-loop event sequence and failure-handling contract in PLANS.md (`Core loop` and `Failure handling contract`).

See PLANS.md for the full v1 scope and the remaining milestones (config+models, conversations polish, tools, memory, context, price meter).

---

## Open items / notes for the next session

- Identify the first `async-openai` integration points; confirm the `stream` feature flag and the streaming event handling (tool-call deltas).
- Decide keychain vs config-file storage for secrets (PLANS.md defaults to keychain — `tauri-plugin-keyring`).
- The price meter needs a bundled per-model pricing table (lazy on model selection, cached in `model_prices`). We'll need the Fireworks model IDs in use.
- `web_search`/memory tools are later milestones; the SQLite schema + `model_prices` table are already in place.
