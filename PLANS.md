# AI Chat App — Plan

A small, desktop AI chat app. Built on **SolidJS + Tailwindcss + Kobalte + Tauri** (Rust backend). This doc fixes the architecture and splits the work into **v1** (ship it) and **v2** (deferred stretch goals).

---

## Guiding principles

- **Small and self-contained.** One Tauri binary, one SQLite file, a directory of Markdown files for memory. No microservices.
- **No agent framework dependency.** The core loop is simple; we implement it directly in Rust. We treat **pi's `agent` core as a pattern reference only** — borrow its message types, `thinkingLevelMap` concept, and compaction summary format, but do not import or vendor it.
- **The model decides; the user controls.** Tools and memory writes are surfaced, gateable, and auditable. Nothing autonomous happens invisibly.
- **Separate *storage size* from *injected context size*.** We can store a lot; we only ever inject a small, bounded, ranked subset at prompt time.

---

## Key decisions (agreed)

| Topic | Decision |
|---|---|
| Agent framework | **None.** Implement the trimmed loop in Rust. pi used as reference only. |
| LLM calls | Through a Rust `#[tauri::command]`. Avoids CORS (arbitrary user `baseUrl`) and keeps the API key out of the frontend. Streams `delta` events to the UI via Tauri channels/events. |
| API transport | OpenAI-compatible `text/event-stream`. Thin adapter so per-provider quirks can be hand-tuned. |
| Streaming lib | **async-openai** (confirmed). Keep a thin adapter on top for per-provider server quirks. |
| Context limit | Use the model's `contextWindow` (default ~200k) minus a reserve. When remaining budget runs low, show a **non-blocking banner**: "near context limit — compact or new chat." User chooses; never silently compact. |
| Tools | Host access with a **per-call approval gate**. Behind a swappable `ToolExecutor` trait. Per-conversation scratch workspace. |
| Product identity | **General-purpose chat assistant** (confirmed, not a coding agent). Tools are an optional enhancement; `web_search` is a **primary** tool for current-info questions. System prompt/behavior reflect a general assistant. |
| Web search | Reuse the existing MCP server at `~/.config/opencode/mcp/duckduckgo-search.mjs` (a `web_search` tool). Talk to it from Rust via the **`rmcp`** crate over stdio. Read-only remote call → **not approval-gated** (confirmed). |
| Memory (facts) | **Markdown files + title routing.** No vector DB. |
| Conversation recall | **Deferred to v2.** This is the only thing that would justify a vector DB. |
| Secret storage | API key in OS keychain (`tauri-plugin-keyring`), not plaintext. |
| Thinking levels | Map to `reasoning_effort` on OpenAI-compatible endpoints. Model-level `thinkingLevelMap` (pi concept): declaratively mark which levels a model supports; the chat-bar selector shows only supported levels. |

---

## Architecture

```
App (SolidJS SPA)
├─ Sidebar (Kobalte collapsible)
│   ├─ New Chat
│   ├─ Search
│   └─ ConversationList
├─ Main
│   ├─ Header — model picker, settings, price meter, "near limit" banner
│   ├─ MessageList — markdown/code render, virtualized
│   └─ ChatBar — textarea, attach files, send/stop, thinking-level selector
└─ Dialogs (Kobalte)
    ├─ Settings — baseUrl, apiKey, user preferences
    └─ Memory tab — list/edit/delete memory entries

Rust backend (Tauri commands)
├─ config: get/set baseUrl, apiKey (keychain), models catalog
├─ models: fetch GET /v1/models → show all; lazy per-model price + cache
├─ chat stream: build context → LLM SSE → emit delta events
├─ tools: execute (approve-gated), tool loop control
└─ memory: save_memory, read_memory, list memory MD files
```

- **Frontend:** SolidJS signals + `<For>` over message rows; `createResource` for streaming deltas into a mutable message.
- **Backend:** Tauri commands owned by Rust; streaming via Tauri channels → events to the UI.

---

## Core loop (trimmed)

Implemented in Rust. Mirror pi's event sequence but reduced.

```
prompt(userMessage)
├─ build context (system + profile + compaction summary + recent messages)
├─ convert to LLM format
├─ stream deltas ──────────────────────────── emit `delta` events
├─ if assistant emits tool calls:
│   ├─ gate each call (approve / deny / abort)
│   ├─ execute tool
│   ├─ on error: return { role: tool, content, isError: true } (model self-corrects)
│   └─ loop back to LLM (capped tool rounds)
├─ persist message + usage + stopReason
└─ emit final message event
```

Guards:
- **Max consecutive tool rounds** before handing back to the user (e.g. 8).
- **Abort** is idempotent; partial text preserved.
- **Stream error** → keep partial, mark `stopReason: "error"`, offer retry.
- **Reasoning echo:** within a turn, each tool-call round's `reasoning_content` is echoed on its assistant tool-call message for the following rounds (DeepSeek thinking mode requires it, else HTTP 400). Traces are **never** replayed across user turns. Toggle: `AppConfig.echo_reasoning_content` (Settings).

---

## Data model (SQLite)

```sql
CREATE TABLE conversations (
  id TEXT PRIMARY KEY,              -- uuid
  title TEXT,                       -- derived from first user message
  model TEXT,
  system_prompt TEXT,
  compaction_summary TEXT,          -- Tier 1 roll-up
  created_at INTEGER,
  updated_at INTEGER
);

CREATE TABLE messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  role TEXT NOT NULL,               -- user | assistant | tool | system
  "index" INTEGER NOT NULL,         -- order within conversation
  content TEXT NOT NULL,            -- plain text or content-block JSON
  model TEXT,
  provider TEXT,
  thinking_level TEXT,
  thinking TEXT,                    -- reasoning trace (reasoning_content), shown behind a toggle
  usage JSON,                       -- token/cost
  stop_reason TEXT,
  attachments TEXT,                 -- JSON array of file attachments (path-read: image dataURL / text / metadata)
  created_at INTEGER
);

CREATE TABLE model_prices (
  provider TEXT NOT NULL,
  model_id TEXT NOT NULL,
  input_per_million REAL,          -- USD per 1M input tokens
  output_per_million REAL,
  cache_read_per_million REAL,
  cache_write_per_million REAL,
  fetched_at INTEGER,
  PRIMARY KEY (provider, model_id)
);
```

Price rates are resolved **lazily** (on model selection) and cached in `model_prices`. Memory lives **outside SQLite** as Markdown files (see Memory system). The `compaction_summary` column is per-conversation. `usage`/`stop_reason` let us display cost and drive the near-limit banner.

---

## Memory system (v1)

A directory of Markdown files in the app data dir, surface-routed by title.

```
memory/
├─ index.md                 # auto-generated: title + one-line summary per entry
├─ preferences.md
├─ identity.md
├─ goals.md
└─ notes.md
```

Each file has YAML frontmatter; the model never edits the index directly.

```markdown
---
title: "Prefers dark mode"
category: preference     # preference | identity | goal | note
importance: 3            # 0-5
created: 2025-09-08
source_conversation: conv_1a2b3c
summary: "Prefers dark mode across most apps."
---
User prefers dark mode; dislikes bright themes.
```

**Read path (title → decide → read):**
- The small `index.md` (titles + summaries) is **always injected** into context.
- The model routes on titles and only calls `read_memory(path)` when a summary isn't enough.
- Bodies are read **on demand**, so per-turn injected context stays bounded even as storage grows.

**Write path (`save_memory`):**
```
save_memory(content, path?, category?, importance)
```
- Maps `category` → a canonical file (`preferences.md`, etc.), **appending/updating** rather than creating one file per thought. The model may instead pass an explicit `path` to write to a specific existing or new file; `path` overrides `category`.
- Regenerates the entry's `summary` and `index.md` automatically.
- Dedupe on write: if the entry already exists, merge/update instead of duplicating.
- User can always edit/delete via the **Memory tab** (which is literally these MD files).

**Bounded size:** the always-injected thing is only the title+summary index. If a single file bloats, run a **consolidation pass** (summarize it into tight bullets) — the same idea as conversation compaction.

**Tuning for aggressive writing:** `save_memory` is constrained by a stability test (stable + user-specific + non-transient), an `importance` gate, dedupe-on-write, a user-visible review surface, and opt-in privacy (per-session "don't save" toggle).

**Profile block (Tier 0):** a small, curated, always-injected context source (identity, preferences, standing context), distinct from the auto-written memory files. Cheap and trusted.

---

## File attachments (v1)

Attach files to a message from the composer. The paperclip button opens the native picker (Tauri dialog plugin); files can also be dropped onto the window (Tauri's native drag-drop event). Pending files preview above the composer (image thumbnail, or a document chip with name + size + remove). Once sent, files render inside the user bubble and persist with the message (`messages.attachments` JSON).

- **Reading:** `attachments.rs` reads each path on the blocking pool. Images become a base64 data URL; text-like files carry their contents; everything else keeps metadata only.
- **Sending:** text-file contents are folded into the message text (`--- File: name ---`); images are sent as multimodal `image_url` content parts. This requires a vision-capable model for images.
- **Storage:** the `Attachment` JSON (`id`, `name`, `mime`, `size`, `kind`, `dataUrl`, `text`) is stored on the user row and replayed from history.

---

## Tool surface

Introduce tools through the same loop, so they're visible, gateable, and aborted like everything else.

```rust
// Approval-gated (local side effects / local data): approve/deny/abort per call.
// bash runs in a per-conversation persistent shell: cd/export/functions persist across calls.
bash(command: string)
read_file(path: string)
write_file(path: string, content: string)

// Memory tools
save_memory(content: string, path?: string, category?: ..., importance: 0-5)
read_memory(path: string)

// Web search — reused MCP server, read-only (not gated)
web_search(query: string, count?: u8 = 5)

// v2 (deferred)
search_conversations(query: string, limit?: u8)
```

**MCP integration:** `web_search` is not reimplemented. We spawn `node ~/.config/opencode/mcp/brave-search.mjs` and keep a **single long-lived `rmcp` stdio client** per app session, then map its `web_search` call to our `ToolResult`. Unlike the DuckDuckGo path, **Brave needs a `BRAVE_API_KEY`** — the script loads it from its sibling `.env` (or env vars), so we just spawn it and let it resolve the key. The MCP seam means we can add other servers later (context7, google_search, playwright, filesystem) without changing the loop.

Tool execution is isolated behind a `ToolExecutor` trait so it can be swapped (host → container) without changing the loop:

```rust
trait ToolExecutor {
    async fn execute(&self, call: ToolCall, gate: &ApprovalGate) -> Result<ToolResult>;
}
```

- **v1:** `HostExecutor` (bash, read_file, write_file) + approval gate.
- **v2:** `ContainerExecutor` (docker/podman, scratch workspace mounted in, results read back) for unattended or multi-user use.
- Read tools are **also gated** for privacy. Conversations operate in a per-conversation scratch workspace by default.

---

## Configuration & providers

- **Settings dialog:** user is prompted for `baseUrl` + `apiKey` (stored in OS keychain) plus profile/memory prefs. The client is a **generic OpenAI-compatible client** — any endpoint the user enters works. The Fireworks endpoint (`https://api.fireworks.ai/inference/v1`) is prefilled as a convenient default for v1, but nothing is hardcoded to Fireworks.
- **Models:** for an endpoint that matches a **models.dev** provider (the default Fireworks does), the picker lists that catalog with real names, context windows, and reasoning metadata — no API key needed. Only an unmatched/self-hosted endpoint falls back to its own `GET /v1/models`. The catalog is pulled at startup (cached rows render immediately, then replaced when the fetch resolves) and re-pulled when the base URL or API key changes; there is no manual "Load"/refresh in Settings, and the header picker is the only place the model changes (persisted immediately; closing Settings never changes it).
- **Thinking levels:** map to `reasoning_effort`; per-model `thinkingLevelMap` marks supported levels; the chat-bar selector is filtered accordingly. Non-reasoning models hide the selector. Because servers vary, the thinking-field mapping is an **advanced compat override** the user can set. Reasoning traces produced in a tool round are echoed back on the assistant tool-call message for the following rounds of the same turn (DeepSeek thinking mode requires this); gated by `echo_reasoning_content` (default `true`) so providers that reject the field can opt out.
- **Price meter:** each assistant message stores full `usage` (input/output/cacheRead/cacheWrite tokens) — request `stream_options.include_usage` so the stream reports it. Cost is `usage × rates`, computed per turn and **frozen** on the message at the model/rates in effect at that time, so switching models never retroactively re-prices past turns. Rates are resolved **lazily** and **cached in `model_prices`** (models.dev, refreshed at startup and on endpoint/key change). Models with no known rate show "—".

**Compat layer:** `async-openai` is tuned to *real OpenAI's* exact request/response shape. A small `CompatConfig` normalizes the knobs a server deviates on (thinking field, `max_tokens` field, `developer` vs `system` role, `usage` in stream). For v1 it defaults to OpenAI-shaped behavior with **advanced overrides the user can set** — nothing Fireworks-specific. Preset dropdown only if we add more providers. The first such knob to land is `echo_reasoning_content` (reasoning echo between tool calls).

**Usage vs rates (pricing):** OpenAI-compatible APIs return token `usage`, never dollar cost — `/v1/models` carries only metadata, and there's no price-per-token in the response. So cost is `usage × rates`. Fireworks exposes **no usable public pricing API** (`/models` omits prices; the schema's `skuInfos` is only populated by an undocumented, forbidden `ListServerlessModels`), so prices are fetched from the free public **models.dev** catalog (`api.json`), which keys models by their exact API id and carries per-1M rates + context windows. Resolution order: **user override → fetched row cached in `model_prices` → bundled fallback table**. Cost per turn splits prompt tokens into uncached / cache-read / cache-write buckets (no overlap) and is frozen on the message. Fetching happens at startup and when the endpoint/key changes — no background polling and no manual refresh. This mirrors pi (`cost` rates in the model catalog + `usage` from the API).

---

## Failure handling contract

- **Stream error mid-turn:** keep partial text, mark `stopReason: "error"`, show retry. Never lose streamed content.
- **Tool error:** throw in the tool, **convert to a tool-error result** (`{ role: "tool", content, isError: true }`) so the model can self-correct. Don't return throwaway prose.
- **Abort:** signal → close stream → persist partial. Idempotent.
- **429/5xx:** exponential backoff on the *whole* request before any tokens stream; never retry mid-stream.
- **Runaway guard:** hard cap on consecutive tool rounds.

---

## V1 scope (ship it)

Status markers reflect the current state (see AGENTS.md for detail).

1. ✅ Tauri + SolidJS + Tailwind + Kobalte scaffold.
2. ✅ SQLite schema for conversations/messages + `compaction_summary` (column exists; compaction logic is milestone 7).
3. ✅ Settings dialog: `baseUrl`, `apiKey` (keychain), user preferences. ⬜ profile block.
4. ✅ Model catalog from models.dev (cached, name/context/reasoning) with `GET /v1/models` fallback for unmatched endpoints; fetched at startup + on endpoint/key change. Picker lives in the header (no Settings model loader).
5. ✅ Rust streaming loop (OpenAI-compatible SSE) emitting `delta` events; failure-handling contract implemented.
6. ✅ Tool surface: `bash`, `read_file`, `write_file` (approve-gated) plus `save_memory`/`read_memory` (ungated, app-local memory).
7. ✅ Memory as Markdown files + auto-generated `index.md`, Memory tab (list/edit/delete).
8. ✅ Sidebar (conversations, new chat, search) + chat shell (markdown/code render).
9. ✅ Thinking-level selector (per-model supported levels; persisted in config).
10. ◑ Context counter landed (tokens used / window in the header, from the last turn's usage); reserve + near-limit banner ("compact or new chat") and per-conversation compaction remain.
11. ✅ Price meter — per-message usage captured (`stream_options.include_usage`); each turn's cost is computed and **frozen** on the message (uncached/cache-read/cache-write/output buckets), priced with the model that produced it. Rates resolve override → models.dev-fetched `model_prices` → bundled fallback; fetched at startup/endpoint change; shown in the header (sidebar readout not yet).
12. ✅ File attachments — native picker + drag-drop, pending preview, sent files rendered and persisted; text folded in, images as multimodal parts.

**Landed beyond the original scope (M5.5):** dark mode + light/dark toggle, provider reasoning traces (inline bar + popup), and a persistent per-conversation shell for `bash`.

**Explicitly NOT in v1:** vector DB, conversation recall, container isolation, autonomous post-hoc memory extraction.

---

## V2 scope (deferred / future)

- **Conversation recall** via vector DB: sqlite-vec + provider/local embedder over conversation chunks, with provenance (`conversation_id`, `title`, `created_at`). Behind a `MemoryBackend`/retrieval seam so the scorer is swappable. Exposed as a `search_conversations` tool and/or a "query past chats" UI.
- **Container/sandbox tool execution** via `ContainerExecutor` for unattended or multi-user use.
- **Autonomous post-hoc memory extraction** (if `save_memory`-during-convo proves insufficient): a cleanup/consolidation job; not the primary write path.
- **Cross-conversation memory consolidation** and conflict resolution surfaced to the user.
- **Semantic memory upgrades** (embeddings for facts) only if title-routing becomes insufficient.

---

## Open questions

**Resolved:** streaming lib → `async-openai`; v1 tools → `read_file`/`write_file`/`bash` (gated) + `web_search` (MCP, ungated); identity → general-purpose chat; provider → **generic OpenAI-compatible client** with Fireworks as the default/prefilled endpoint (user prompted for key). Compat is a user-set advanced override, nothing provider-hardcoded.

Still open:

(none blocking. When we wire the price meter, I'll need the Fireworks model IDs you'll use + their per-1M rates for the bundled table — I can seed it with common Fireworks models as a starting point.)

Defaults I'll take unless you object:
- **Pricing:** bundled per-model table, resolved **lazily on model selection** and cached in `model_prices`; fetched from models.dev at startup and on endpoint/key change. Each turn's cost is frozen on the message with the model that produced it. Unknown models show "—". (Third-party aggregators like Requesty exist but are fragile — v2 could wire one for auto-refresh.)
- Reserve + banner threshold derived per-model from `contextWindow` (fallback default 200k).
- Scratch workspace = per-conversation dir under app data; `read_file`/`write_file` take real user-approved paths.
- Profile block = a few structured fields via a Settings form.

---

## Build order / milestones

Status as of the latest session (details in AGENTS.md):

1. ✅ **Scaffold:** Tauri + SolidJS project, Tailwind, Kobalte set up, SQLite wiring.
2. ✅ **Loop:** Rust streaming command + events; an OpenAI-compatible model works end-to-end.
3. ✅ **Config + models:** Settings dialog, keychain storage, model fetch + picker.
4. ✅ **Conversations:** SQLite schema, sidebar, new chat, message persistence.
5. ✅ **Tools:** approve-gate + `bash`/`read_file`/`write_file`; `web_search` via `rmcp` MCP client; tool loop. (GUI-exercised.)
6. ✅ **Memory:** MD files + `index.md` + `save_memory`/`read_memory` + Memory tab.
7. ◑ **Context:** header counter done (tokens used / window); reserve/banner + per-conversation compaction remain. ← next
8. ✅ **Price meter:** capture usage (include_usage) + per-model overrides + models.dev fetch cached in `model_prices` (bundled fallback) + header readout (sidebar readout not yet).
9. ◑ **Polish:** markdown/code render (M4), dark mode, reasoning traces, persistent shell (M5.5), thinking-level selector, and file attachments done; virtualized message list remains.
10. ⬜ **V2:** conversation-recall vector index (only when explicitly opted in).
