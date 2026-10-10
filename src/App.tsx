import {
  createEffect,
  createSignal,
  For,
  Index,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
} from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import { DropdownMenu } from "@kobalte/core/dropdown-menu";
import { Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  addProvider,
  approveTool,
  backfillMemories,
  backfillStatus,
  cancelBackfill,
  clearExtractions,
  createConversation,
  deleteConversation,
  deleteMemoryFile,
  denyTool,
  getConfig,
  getPricing,
  hasApiKey,
  hasBraveKey,
  importConversations,
  listConversations,
  listMemoryFiles,
  listMessages,
  listModels,
  listModelsDevProviders,
  listProviders,
  memoryExtractionStats,
  memoryReflectionStats,
  readAttachments,
  reflectNow,
  removeProvider,
  renameConversation,
  searchConversations,
  setActiveProvider,
  setApiKey,
  setBraveKey,
  setConfig,
  setConversationModel,
  setConversationProvider,
  stopChat,
  streamChat,
  updateProvider,
  syncNow,
  syncSignIn,
  syncSignOut,
  syncSignUp,
  syncSetEncryption,
  syncRemoveEncryption,
  syncImportKey,
  syncRecoveryCode,
  syncStatus,
  syncToggle,
  thinkingOptions,
  writeMemoryFile,
  type Attachment,
  type BackfillStatus,
  type Conversation,
  type ExtractionStats,
  type ImportReport,
  type MemoryFile,
  type Message,
  type ModelInfo,
  type ModelOverride,
  type ModelsDevProvider,
  type Pricing,
  type Provider,
  type ProviderInfo,
  type ReflectionStats,
  type StreamEvent,
  type SyncStatus,
  type ThinkingOptions,
} from "./lib/api";
import Markdown from "./lib/Markdown";

const THEME_KEY = "pi-chat-theme";

interface LiveTool {
  callId: string;
  name: string;
  arguments: string;
  gated: boolean;
  state: "pending" | "running" | "done";
  ok?: boolean;
  output?: string;
}

function parseToolResult(
  m: Message,
): { name: string; output: string; error: boolean; callId: string | null } | null {
  if (m.role !== "tool") return null;
  try {
    const v = JSON.parse(m.content);
    if (v && typeof v === "object" && "tool_call_id" in v && "output" in v) {
      return {
        name: String(v.name ?? "tool"),
        output: String(v.output),
        error: Boolean(v.error),
        callId: v.tool_call_id != null ? String(v.tool_call_id) : null,
      };
    }
  } catch {
    // fall through
  }
  return { name: "tool", output: m.content, error: false, callId: null };
}

function formatToolArgs(raw: unknown): string {
  let value = raw;
  if (typeof raw === "string") {
    try {
      value = JSON.parse(raw);
    } catch {
      return raw; // partial/invalid JSON while streaming
    }
  }
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const entries = Object.entries(value as Record<string, unknown>);
    // Single string field (e.g. bash's `{command}`) → show just the value.
    if (entries.length === 1 && typeof entries[0][1] === "string") {
      return entries[0][1] as string;
    }
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }
  return String(value ?? "");
}

function parseToolCallList(
  m: Message,
): { id: string; name: string; arguments: unknown }[] | null {
  if (m.role !== "assistant") return null;
  try {
    const v = JSON.parse(m.content);
    if (v && typeof v === "object" && Array.isArray(v.tool_calls)) {
      return v.tool_calls.map((c: any) => ({
        id: String(c?.id ?? c?.name ?? ""),
        name: String(c?.name ?? ""),
        arguments: c?.arguments,
      }));
    }
  } catch {
    // fall through
  }
  return null;
}

function parseUsage(m: Message): Record<string, number> | null {
  if (!m.usage) return null;
  try {
    const v = JSON.parse(m.usage);
    return v && typeof v === "object" ? (v as Record<string, number>) : null;
  } catch {
    return null;
  }
}

function parseAttachments(m: Message): Attachment[] {
  if (!m.attachments) return [];
  try {
    const v = JSON.parse(m.attachments);
    return Array.isArray(v) ? (v as Attachment[]) : [];
  } catch {
    return [];
  }
}

function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function DocIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      class="h-4 w-4 shrink-0"
      fill="none"
      stroke="currentColor"
      stroke-width="1.7"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M14 3v4a1 1 0 0 0 1 1h4" />
      <path d="M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2Z" />
    </svg>
  );
}

function PaperclipIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      class="h-5 w-5"
      fill="none"
      stroke="currentColor"
      stroke-width="1.7"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M21.44 11.05 12.25 20.24a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.49" />
    </svg>
  );
}

function MenuIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      class="h-5 w-5"
      fill="none"
      stroke="currentColor"
      stroke-width="1.7"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M4 6h16" />
      <path d="M4 12h16" />
      <path d="M4 18h16" />
    </svg>
  );
}

function MoreIcon() {
  return (
    <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
      <circle cx="5" cy="12" r="1.7" />
      <circle cx="12" cy="12" r="1.7" />
      <circle cx="19" cy="12" r="1.7" />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      class="h-5 w-5"
      fill="none"
      stroke="currentColor"
      stroke-width="1.7"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M18 6 6 18" />
      <path d="m6 6 12 12" />
    </svg>
  );
}

function BackArrowIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      class="h-5 w-5"
      fill="none"
      stroke="currentColor"
      stroke-width="1.7"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M19 12H5" />
      <path d="m12 19-7-7 7-7" />
    </svg>
  );
}

// Shared styling for dropdown-menu items (header overflow menu). Kept as a
// literal here so Tailwind's scanner picks up the `data-[highlighted]` variant.
const MENU_ITEM_CLASS =
  "flex cursor-pointer select-none items-center rounded-md px-3 py-2 text-neutral-700 outline-none data-[highlighted]:bg-neutral-100 dark:text-neutral-200 dark:data-[highlighted]:bg-neutral-800";

function formatTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

function formatCost(cost: number): string {
  if (!Number.isFinite(cost) || cost <= 0) return "$0";
  if (cost < 0.01) return `$${cost.toFixed(4)}`;
  if (cost < 1) return `$${cost.toFixed(3)}`;
  return `$${cost.toFixed(2)}`;
}

/// Cost of one turn's summed usage at the given model rates (mirrors
/// `pricing::cost_of_usage`). Returns null when the model has no rates.
function costOfUsage(u: Record<string, number>, p: Pricing): number | null {
  if (p.inputPerMillion == null || p.outputPerMillion == null) return null;
  const input = p.inputPerMillion;
  const output = p.outputPerMillion;
  const cacheRead = p.cacheReadPerMillion ?? input;
  const cacheWrite = p.cacheWritePerMillion ?? input;
  const prompt = u.prompt_tokens ?? 0;
  const cached = Math.min(u.cached_tokens ?? 0, prompt);
  const cacheWriteTokens = Math.min(u.cache_write_tokens ?? 0, Math.max(0, prompt - cached));
  const uncached = Math.max(0, prompt - cached - cacheWriteTokens);
  return (
    (uncached * input +
      cached * cacheRead +
      cacheWriteTokens * cacheWrite +
      (u.completion_tokens ?? 0) * output) /
    1_000_000
  );
}

type ProcessEntry =
  | { kind: "thought"; id: string; text: string }
  | { kind: "text"; id: string; text: string }
  | {
      kind: "call";
      id: string;
      callId: string;
      name: string;
      arguments: string;
      result: { output: string; error: boolean } | null;
      live?: LiveTool;
    };

type CallEntry = Extract<ProcessEntry, { kind: "call" }>;

interface Turn {
  key: string;
  user: Message | null;
  entries: ProcessEntry[];
  answer: Message | null;
  live: boolean;
  /// A memory-consolidation note attached to this turn (background bookkeeping).
  note: Message | null;
}

/// Turn a turn's process rows into an ordered list of timeline entries,
/// pairing each tool call with the result row that follows it.
function buildEntries(rows: Message[]): ProcessEntry[] {
  const entries: ProcessEntry[] = [];
  const calls = new Map<string, CallEntry>();
  for (const m of rows) {
    if (m.thinking?.trim()) entries.push({ kind: "thought", id: m.id, text: m.thinking });
    const toolCalls = parseToolCallList(m);
    if (toolCalls) {
      for (const c of toolCalls) {
        const entry: CallEntry = {
          kind: "call",
          id: `${m.id}:${c.id}`,
          callId: c.id,
          name: c.name,
          arguments: formatToolArgs(c.arguments),
          result: null,
        };
        entries.push(entry);
        calls.set(c.id, entry);
      }
      continue;
    }
    const tr = parseToolResult(m);
    if (tr) {
      const entry = tr.callId ? calls.get(tr.callId) : undefined;
      if (entry) entry.result = { output: tr.output, error: tr.error };
      else if (tr.output.trim()) entries.push({ kind: "text", id: m.id, text: tr.output });
      continue;
    }
    if (m.content.trim()) entries.push({ kind: "text", id: m.id, text: m.content });
  }
  return entries;
}

/// Group a flat message list into turns: a user row opens a turn, the final
/// non-user row is the answer, and everything before it is process activity.
/// While streaming, the live turn's entries come from `liveTools` plus the
/// in-flight assistant's reasoning instead.
function groupTurns(messages: Message[], liveTools: LiveTool[], streaming: boolean): Turn[] {
  const turns: Turn[] = [];
  const rowsByTurn = new Map<Turn, Message[]>();
  let current: Turn | null = null;
  const flush = () => {
    if (current) turns.push(current);
    current = null;
  };
  for (const m of messages) {
    if (m.role === "user") {
      flush();
      current = { key: m.id, user: m, entries: [], answer: null, live: false, note: null };
      rowsByTurn.set(current, []);
      continue;
    }
    if (m.role === "memory") {
      if (!current) {
        current = { key: m.id, user: null, entries: [], answer: null, live: false, note: m };
        rowsByTurn.set(current, []);
      } else {
        current.note = m;
      }
      continue;
    }
    if (!current) {
      current = { key: m.id, user: null, entries: [], answer: null, live: false, note: null };
      rowsByTurn.set(current, []);
    }
    rowsByTurn.get(current)!.push(m);
  }
  flush();

  for (const turn of turns) {
    const rows = rowsByTurn.get(turn) ?? [];
    const last = rows[rows.length - 1];
    let answer: Message | null = null;
    let processRows = rows;
    if (last && last.role === "assistant" && !parseToolCallList(last) && !parseToolResult(last)) {
      answer = last;
      processRows = rows.slice(0, -1);
    }
    turn.answer = answer;
    turn.entries = buildEntries(processRows);
  }

  const live = turns[turns.length - 1];
  if (live && streaming && messages.some((m) => m.id.startsWith("tmp-assistant-"))) {
    live.live = true;
    // Keep the live thought first so its DOM node survives streamed updates as
    // tool entries append after it.
    const thought = live.answer?.id.startsWith("tmp-assistant-")
      ? live.answer?.thinking?.trim()
      : null;
    live.entries = [];
    if (thought && live.answer) {
      live.entries.push({ kind: "thought", id: live.answer.id, text: thought });
    }
    for (const t of liveTools) {
      live.entries.push({
        kind: "call",
        id: `live:${t.callId}`,
        callId: t.callId,
        name: t.name,
        arguments: t.arguments,
        result: t.state === "done" ? { output: t.output ?? "", error: !t.ok } : null,
        live: t,
      });
    }
  } else {
    // Persisted turns: the answer row's reasoning is a trailing timeline entry.
    for (const turn of turns) {
      if (turn.answer?.thinking?.trim()) {
        turn.entries.push({ kind: "thought", id: turn.answer.id, text: turn.answer.thinking });
      }
    }
  }
  return turns;
}

function ThoughtRow(props: {
  id: string;
  live: boolean;
  open: boolean;
  onToggle: (id: string) => void;
}) {
  return (
    <button
      onClick={() => props.onToggle(props.id)}
      title="Show the reasoning trace"
      class={`flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
        props.open
          ? "bg-neutral-200 text-neutral-700 dark:bg-neutral-700 dark:text-neutral-100"
          : "text-neutral-500 hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
      }`}
    >
      <span
        class={`inline-block h-1.5 w-1.5 rounded-full ${
          props.live ? "animate-pulse bg-amber-500" : "bg-neutral-400 dark:bg-neutral-500"
        }`}
      />
      {props.live ? "Thinking…" : "Thought"}
    </button>
  );
}

function CallRow(props: {
  entry: CallEntry;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const live = () => props.entry.live;
  const state = () => live()?.state;
  return (
    <div class="rounded-lg border border-neutral-200 bg-white px-3 py-2 text-xs dark:border-neutral-800 dark:bg-neutral-900">
      <div class="mb-1 text-[11px] text-neutral-500 dark:text-neutral-400">
        {state() === "pending" && "approval needed · "}
        {state() === "running" && "running · "}
        {state() === "done" && (live()?.ok ? "done · " : "error · ")}
        <span class="font-medium text-neutral-700 dark:text-neutral-200">{props.entry.name}</span>
      </div>
      <pre class="max-h-40 overflow-auto whitespace-pre-wrap break-words text-neutral-600 dark:text-neutral-400">
        {props.entry.arguments}
      </pre>
      <Show when={state() === "pending"}>
        <div class="mt-2 flex gap-2">
          <button
            class="rounded-md bg-neutral-900 px-3 py-1 text-xs font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
            onClick={() => props.onApprove(props.entry.callId)}
          >
            Approve
          </button>
          <button
            class="rounded-md border border-neutral-300 px-3 py-1 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
            onClick={() => props.onDeny(props.entry.callId)}
          >
            Deny
          </button>
        </div>
      </Show>
      <Show when={props.entry.result}>
        <div class="mt-2">
          <div class="text-[11px] text-neutral-500 dark:text-neutral-400">
            {props.entry.result!.error ? "error" : "result"}
          </div>
          <pre class="mt-0.5 max-h-60 overflow-auto whitespace-pre-wrap break-words text-neutral-700 dark:text-neutral-300">
            {props.entry.result!.output}
          </pre>
        </div>
      </Show>
    </div>
  );
}

/// One collapsible activity timeline for a turn: reasoning, preamble text and
/// tool calls/results in order. Expands while live and collapses when done.
function Timeline(props: {
  entries: ProcessEntry[];
  live: boolean;
  openThoughtId: string | null;
  onToggleThought: (id: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const [open, setOpen] = createSignal(props.live);
  let prevLive = false;
  createEffect(() => {
    const live = props.live;
    if (live) setOpen(true);
    else if (prevLive) setOpen(false);
    prevLive = live;
  });

  const summary = () => {
    const thoughts = props.entries.filter((e) => e.kind === "thought").length;
    const calls = props.entries.filter((e) => e.kind === "call").length;
    const parts: string[] = [];
    if (thoughts) parts.push("Thought");
    if (calls) parts.push(`${calls} tool${calls === 1 ? "" : "s"}`);
    return parts.join(" · ") || "Activity";
  };

  return (
    <div class="flex justify-start">
      <div class="w-full max-w-[80%]">
        <button
          onClick={() => setOpen((o) => !o)}
          title="Show activity timeline"
          class={`mb-1 flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
            open()
              ? "text-neutral-600 dark:text-neutral-300"
              : "text-neutral-500 hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
          }`}
        >
          <span
            class={`inline-block h-1.5 w-1.5 rounded-full ${
              props.live ? "animate-pulse bg-amber-500" : "bg-neutral-400 dark:bg-neutral-500"
            }`}
          />
          <span>{summary()}</span>
          <span class="opacity-60">{open() ? "▾" : "▸"}</span>
        </button>
        <Show when={open()}>
          <div class="space-y-2 border-l-2 border-neutral-200 pl-3 dark:border-neutral-800">
            <Index each={props.entries}>
              {(entry) => (
                <Switch>
                  <Match when={entry().kind === "thought"}>
                    <ThoughtRow
                      id={entry().id}
                      live={props.live}
                      open={props.openThoughtId === entry().id}
                      onToggle={props.onToggleThought}
                    />
                  </Match>
                  <Match when={entry().kind === "text"}>
                    <Markdown
                      text={(entry() as Extract<ProcessEntry, { kind: "text" }>).text}
                      class="prose-sm"
                    />
                  </Match>
                  <Match when={entry().kind === "call"}>
                    <CallRow
                      entry={entry() as CallEntry}
                      onApprove={props.onApprove}
                      onDeny={props.onDeny}
                    />
                  </Match>
                </Switch>
              )}
            </Index>
          </div>
        </Show>
      </div>
    </div>
  );
}

export default function App() {
  const [conversations, setConversations] = createSignal<Conversation[]>([]);
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Message[]>([]);
  const [draft, setDraft] = createSignal("");
  const [streaming, setStreaming] = createSignal(false);
  const [streamError, setStreamError] = createSignal<string | null>(null);
  const [liveTools, setLiveTools] = createSignal<LiveTool[]>([]);
  const [nearBottom, setNearBottom] = createSignal(true);
  let messagesScrollEl: HTMLElement | undefined;

  // Files staged for the next message (picked or dropped).
  const [pendingAttachments, setPendingAttachments] = createSignal<Attachment[]>([]);
  const [dragOver, setDragOver] = createSignal(false);
  // Narrow-viewport drawer state. At >=lg the sidebar is always in flow, so
  // this only matters below the breakpoint (see the `lg:` overrides on <aside>).
  const [sidebarOpen, setSidebarOpen] = createSignal(false);
  // Mirrors Tailwind's `lg` (64rem) so the off-screen drawer can be made inert
  // below it — a merely translated drawer stays focusable and in the a11y tree.
  const [wideViewport, setWideViewport] = createSignal(
    typeof window === "undefined" ? true : window.matchMedia("(min-width: 64rem)").matches,
  );
  const drawerHidden = () => !wideViewport() && !sidebarOpen();
  let hamburgerEl: HTMLButtonElement | undefined;
  let sidebarSearchEl: HTMLInputElement | undefined;

  // Move focus into the drawer when it opens (it is only openable below `lg`).
  createEffect(() => {
    if (sidebarOpen()) sidebarSearchEl?.focus();
  });

  const [draftTitle, setDraftTitle] = createSignal("");
  const [newOpen, setNewOpen] = createSignal(false);

  const [searchQuery, setSearchQuery] = createSignal("");
  const [visibleConversations, setVisibleConversations] = createSignal<Conversation[]>([]);
  const [renameId, setRenameId] = createSignal<string | null>(null);
  const [renameValue, setRenameValue] = createSignal("");
  const [confirmDeleteId, setConfirmDeleteId] = createSignal<string | null>(null);

  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [baseUrl, setBaseUrl] = createSignal("");
  const [model, setModel] = createSignal("");
  const [preferences, setPreferences] = createSignal("");
  const [thinkingLevel, setThinkingLevel] = createSignal("");
  const [echoReasoning, setEchoReasoning] = createSignal(true);
  const [thinkingOpts, setThinkingOpts] = createSignal<ThinkingOptions | null>(null);
  const [models, setModels] = createSignal<ModelInfo[]>([]);
  const [attachmentWarning, setAttachmentWarning] = createSignal<string | null>(null);
  const [modelsLoading, setModelsLoading] = createSignal(false);
  const [settingsError, setSettingsError] = createSignal<string | null>(null);
  const [keyDraft, setKeyDraft] = createSignal("");
  const [hasKey, setHasKey] = createSignal(false);
  const [braveKeyDraft, setBraveKeyDraft] = createSignal("");
  const [hasBraveKeySaved, setHasBraveKeySaved] = createSignal(false);
  const [reflectionEnabled, setReflectionEnabled] = createSignal(true);
  const [reflectionIdleMinutes, setReflectionIdleMinutes] = createSignal(30);
  const [shellWorkspaceDir, setShellWorkspaceDir] = createSignal("");
  const [reflectionStats, setReflectionStats] = createSignal<ReflectionStats | null>(null);
  const [backfill, setBackfill] = createSignal<BackfillStatus | null>(null);
  const [extractionStats, setExtractionStats] = createSignal<ExtractionStats | null>(null);
  const [importReport, setImportReport] = createSignal<ImportReport | null>(null);
  const [syncEnabled, setSyncEnabled] = createSignal(false);
  const [syncStatusData, setSyncStatusData] = createSignal<SyncStatus | null>(null);
  const [syncEmail, setSyncEmail] = createSignal("");
  const [syncPassword, setSyncPassword] = createSignal("");
  const [syncError, setSyncError] = createSignal<string | null>(null);
  const [syncBusy, setSyncBusy] = createSignal(false);
  // "signin" | "signup" — which mode the auth form is in; a confirmation notice.
  const [syncAuthMode, setSyncAuthMode] = createSignal<"signin" | "signup">("signin");
  const [syncNotice, setSyncNotice] = createSignal<string | null>(null);
  // Client-side encryption: recovery-code entry (unlock) + revealed code.
  const [syncRecoveryInput, setSyncRecoveryInput] = createSignal("");
  const [syncRecoveryShown, setSyncRecoveryShown] = createSignal<string | null>(null);
  const [syncEncBusy, setSyncEncBusy] = createSignal(false);

  // Theme: dark by default, persisted across launches, toggled from the header.
  const [dark, setDark] = createSignal(true);
  const [thinkingOpenId, setThinkingOpenId] = createSignal<string | null>(null);

  // Memory tab: reads/edits the Markdown memory files.
  const [memoryOpen, setMemoryOpen] = createSignal(false);
  const [memoryFiles, setMemoryFiles] = createSignal<MemoryFile[]>([]);
  const [memorySelected, setMemorySelected] = createSignal<string | null>(null);
  const [memoryDraft, setMemoryDraft] = createSignal("");
  const [memoryError, setMemoryError] = createSignal<string | null>(null);
  const [memoryConfirmDelete, setMemoryConfirmDelete] = createSignal<string | null>(null);
  const [memoryPreview, setMemoryPreview] = createSignal(true);

  // Model pricing/context: resolved rates (models.dev cache + bundled table +
  // user overrides) for the selected model, plus a per-model cache so turns
  // generated on earlier models can be priced with their own rates.
  const [modelOverrides, setModelOverrides] = createSignal<Record<string, ModelOverride>>({});
  const [pricing, setPricing] = createSignal<Pricing | null>(null);
  const [pricingByModel, setPricingByModel] = createSignal<Record<string, Pricing>>({});
  // Bumped after a catalog refresh so pricing/thinking re-resolve.
  const [catalogVersion, setCatalogVersion] = createSignal(0);

  // --- Multi-provider state ---
  const [providers, setProviders] = createSignal<ProviderInfo[]>([]);
  const [activeProviderId, setActiveProviderId] = createSignal("");
  const [providersLoading, setProvidersLoading] = createSignal(false);
  const [modelsDevProviders, setModelsDevProviders] = createSignal<ModelsDevProvider[]>([]);
  // Provider shown in the header for the active conversation (or active default).
  const [headerProviderId, setHeaderProviderId] = createSignal("");
  // Provider manager form state.
  const [addOpen, setAddOpen] = createSignal(false);
  const [addName, setAddName] = createSignal("");
  const [addUrl, setAddUrl] = createSignal("");
  const [addKey, setAddKey] = createSignal("");
  const [addPick, setAddPick] = createSignal("");
  const [editId, setEditId] = createSignal<string | null>(null);
  const [editName, setEditName] = createSignal("");
  const [editUrl, setEditUrl] = createSignal("");
  const [editKey, setEditKey] = createSignal("");
  const [providerError, setProviderError] = createSignal<string | null>(null);

  createEffect(() => {
    const el = document.documentElement;
    const isDark = dark();
    el.classList.toggle("dark", isDark);
    try {
      localStorage.setItem(THEME_KEY, isDark ? "dark" : "light");
    } catch {
      // ignore (storage unavailable)
    }
  });

  let tempId = 0;

  onMount(async () => {
    try {
      const saved = localStorage.getItem(THEME_KEY);
      if (saved === "light") setDark(false);
    } catch {
      // ignore
    }

    // Narrow-viewport drawer: Escape closes it and returns focus to the
    // hamburger (no-op on wide viewports where the sidebar is always in flow).
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && sidebarOpen()) {
        setSidebarOpen(false);
        hamburgerEl?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));

    const mq = window.matchMedia("(min-width: 64rem)");
    const onMq = (e: MediaQueryListEvent) => setWideViewport(e.matches);
    setWideViewport(mq.matches);
    mq.addEventListener("change", onMq);
    onCleanup(() => mq.removeEventListener("change", onMq));

    // Native OS drag-and-drop (HTML5 drops are intercepted by Tauri).
    const unlistenDrop = await getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") {
        setDragOver(true);
      } else if (payload.type === "leave") {
        setDragOver(false);
      } else if (payload.type === "drop") {
        setDragOver(false);
        if (payload.paths.length > 0) void addAttachmentPaths(payload.paths);
      }
    });
    onCleanup(() => unlistenDrop());

    // Background memory consolidation finished: reveal its note + refresh spend.
    const unlistenReflect = await listen<{ conversationId: string }>(
      "memory-reflection",
      (event) => {
        void refreshReflectionStats();
        const id = activeId();
        if (id && event.payload.conversationId === id && !streaming()) {
          listMessages(id).then(setMessages);
        }
        refreshConversations().catch(() => {});
      },
    );
    onCleanup(() => unlistenReflect());

    // Backfill progress: extraction passes, then the consolidation batches.
    let backfillTick = 0;
    const unlistenBackfill = await listen<BackfillStatus>(
      "memory-backfill",
      (event) => {
        setBackfill(event.payload);
        if (!event.payload.running) {
          void refreshExtractionStats();
          void refreshReflectionStats();
          void reloadMemory();
        } else if (++backfillTick % 25 === 0) {
          // The staged/pending line is otherwise frozen at whatever it was when
          // the dialog opened, for the whole run.
          void refreshExtractionStats();
        }
      },
    );
    onCleanup(() => unlistenBackfill());

    const rows = await listConversations();
    setConversations(rows);
    setVisibleConversations(rows);
    if (rows.length > 0) setActiveId(rows[0].id);

    const cfg = await getConfig();
    setBaseUrl(cfg.baseUrl);
    setModel(cfg.model);
    setPreferences(cfg.preferences ?? "");
    setThinkingLevel(cfg.thinkingLevel ?? "");
    setEchoReasoning(cfg.echoReasoningContent ?? true);
    setReflectionEnabled(cfg.memoryReflectionEnabled ?? true);
    setReflectionIdleMinutes(cfg.memoryReflectionIdleMinutes ?? 30);
    setShellWorkspaceDir(cfg.shellWorkspaceDir ?? "");
    setSyncEnabled(cfg.syncEnabled ?? false);
    setModelOverrides(cfg.modelOverrides ?? {});
    setActiveProviderId(cfg.activeProviderId ?? "");
    void refreshProviders();
    setHasKey(await hasApiKey());
    void refreshReflectionStats();    // Fast path: cached catalog/prices so the picker and meter render at once.
    // Without a key the picker falls back to the configured model. The list is
    // scoped to the active/header provider after refreshProviders() resolves;
    // load the legacy default immediately so the picker renders.
    await loadModels(false, cfg.activeProviderId ?? "").catch(() => {});
    // Background: pull fresh data from models.dev and replace values if they
    // changed, without blocking startup.
    loadModels(true, cfg.activeProviderId ?? "").catch(() => {});
  });

  // Re-resolve pricing whenever the selected model or the catalog changes.
  createEffect(() => {
    const id = model();
    catalogVersion();
    if (!id) {
      setPricing(null);
      return;
    }
    getPricing(id)
      .then(setPricing)
      .catch(() => setPricing(null));
  });

  // Re-resolve the thinking levels whenever the selected model or catalog changes.
  createEffect(() => {
    const id = model();
    catalogVersion();
    if (!id) {
      setThinkingOpts(null);
      return;
    }
    thinkingOptions(id)
      .then(setThinkingOpts)
      .catch(() => setThinkingOpts(null));
  });

  // Price past turns with the model that actually produced them. New turns
  // store a frozen `cost`; only legacy turns without one need a lookup.
  createEffect(() => {
    const byModel = pricingByModel();
    const current = model();
    for (const m of messages()) {
      if (m.role !== "assistant" || !m.model) continue;
      const u = parseUsage(m);
      if (u && typeof u.cost === "number") continue;
      if (m.model === current || byModel[m.model]) continue;
      getPricing(m.model)
        .then((p) => setPricingByModel((prev) => ({ ...prev, [m.model as string]: p })))
        .catch(() => {});
    }
  });

  async function refreshConversations() {
    const q = searchQuery().trim();
    const rows = q ? await searchConversations(q) : await listConversations();
    setConversations(rows);
    setVisibleConversations(rows);
  }

  // Sidebar search: query the backend (title + message text) when non-empty.
  createEffect(() => {
    const q = searchQuery().trim();
    const t = setTimeout(() => void refreshConversations(), q ? 200 : 0);
    onCleanup(() => clearTimeout(t));
  });

  // Auto-load messages whenever the active conversation changes.
  createEffect(() => {
    const id = activeId();
    setThinkingOpenId(null);
    setNearBottom(true);
    if (!id) {
      setMessages([]);
      return;
    }
    listMessages(id).then(setMessages);
  });

  // Keep the newest message in view unless the user has scrolled up.
  createEffect(() => {
    messages();
    liveTools();
    if (!nearBottom()) return;
    const el = messagesScrollEl;
    if (el) el.scrollTop = el.scrollHeight;
  });

  async function createNewChat() {
    const created = await createConversation(draftTitle() || undefined);
    setConversations([created, ...conversations()]);
    setVisibleConversations([created, ...visibleConversations()]);
    setActiveId(created.id);
    setDraftTitle("");
    setNewOpen(false);
  }

  async function commitRename(id: string) {
    const title = renameValue().trim();
    setRenameId(null);
    if (!title) return;
    await renameConversation(id, title);
    await refreshConversations();
  }

  async function removeConversation(id: string) {
    setConfirmDeleteId(null);
    await deleteConversation(id);
    await refreshConversations();
    if (activeId() === id) {
      const next = conversations()[0];
      setActiveId(next ? next.id : null);
    }
  }

  async function openSettings() {
    const cfg = await getConfig();
    setBaseUrl(cfg.baseUrl);
    setModel(cfg.model);
    setPreferences(cfg.preferences ?? "");
    setThinkingLevel(cfg.thinkingLevel ?? "");
    setEchoReasoning(cfg.echoReasoningContent ?? true);
    setReflectionEnabled(cfg.memoryReflectionEnabled ?? true);
    setReflectionIdleMinutes(cfg.memoryReflectionIdleMinutes ?? 30);
    setShellWorkspaceDir(cfg.shellWorkspaceDir ?? "");
    setSyncEnabled(cfg.syncEnabled ?? false);
    setModelOverrides(cfg.modelOverrides ?? {});
    setActiveProviderId(cfg.activeProviderId ?? "");
    setHasKey(await hasApiKey());
    setKeyDraft("");
    void refreshProviders();
    setAddOpen(false);
    setEditId(null);
    setProviderError(null);
    hasBraveKey()
      .then(setHasBraveKeySaved)
      .catch(() => setHasBraveKeySaved(false)); // never block opening Settings
    setBraveKeyDraft("");
    setSettingsError(null);
    setSyncError(null);
    void refreshSync();
    setSettingsOpen(true);
  }

  // Persist the endpoint + preferences (and a typed key, if any). The active
  // model is owned by the header picker, which persists immediately; read it
  // back from the backend so pressing Done never changes it. Returns true when
  // the endpoint or key changed.
  async function persistSettings(): Promise<boolean> {
    const current = await getConfig();
    const norm = (u: string) => u.trim().replace(/\/+$/, "");
    const endpointChanged = norm(current.baseUrl) !== norm(baseUrl());
    await setConfig({
      baseUrl: baseUrl(),
      model: current.model,
      preferences: preferences(),
      thinkingLevel: current.thinkingLevel,
      echoReasoningContent: echoReasoning(),
      memoryReflectionEnabled: reflectionEnabled(),
      memoryReflectionIdleMinutes: reflectionIdleMinutes(),
      modelOverrides: current.modelOverrides ?? {},
      shellWorkspaceDir: shellWorkspaceDir(),
      syncEnabled: syncEnabled(),
      providers: providerConfig(),
      activeProviderId: activeProviderId(),
    });
    let keyChanged = false;
    if (keyDraft().trim()) {
      await setApiKey(keyDraft().trim());
      setHasKey(true);
      setKeyDraft("");
      keyChanged = true;
    }
    if (braveKeyDraft().trim()) {
      await setBraveKey(braveKeyDraft().trim());
      setHasBraveKeySaved(true);
      setBraveKeyDraft("");
      keyChanged = true;
    }
    return endpointChanged || keyChanged;
  }

  // Dialog close = save (milestone 3 contract). Errors surface as a banner.
  async function closeSettings() {
    setSettingsOpen(false);
    try {
      const changed = await persistSettings();
      // Only re-pull the catalog when the endpoint or key changed (or on app
      // start); otherwise keep the list the user is working with.
      if (changed) loadModels(true, headerProviderId() ?? null).catch(() => {});
    } catch (e) {
      setSettingsError(String(e));
    }
  }

  // --- Cloud backup + sync helpers ---

  async function refreshSync() {
    try {
      setSyncStatusData(await syncStatus());
    } catch {
      // Feature unavailable/offline: leave status as-is; never block Settings.
    }
  }

  async function toggleSync(on: boolean) {
    setSyncBusy(true);
    setSyncError(null);
    try {
      const status = await syncToggle(on);
      setSyncStatusData(status);
      setSyncEnabled(status.enabled);
      if (!status.enabled) {
        // Left the feature entirely: clear any signed-in form state.
        setSyncEmail("");
        setSyncPassword("");
        setSyncNotice(null);
      }
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncBusy(false);
    }
  }

  async function submitSyncSignIn() {
    const email = syncEmail().trim();
    const password = syncPassword();
    if (!email || !password) {
      setSyncError("Enter your email and password.");
      return;
    }
    setSyncBusy(true);
    setSyncError(null);
    try {
      // On error (e.g. "confirm your email") the backend message surfaces inline.
      const status = await syncSignIn(email, password);
      setSyncStatusData(status);
      setSyncPassword("");
      setSyncEmail("");
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncBusy(false);
    }
  }

  async function submitSyncSignUp() {
    const email = syncEmail().trim();
    const password = syncPassword();
    if (!email || !password) {
      setSyncError("Enter your email and password.");
      return;
    }
    setSyncBusy(true);
    setSyncError(null);
    setSyncNotice(null);
    try {
      const status = await syncSignUp(email, password);
      if (status.loggedIn) {
        // Confirm-email off (current setup): account is active immediately.
        setSyncStatusData(status);
        setSyncPassword("");
        setSyncEmail("");
      } else {
        // Confirm-email on: account created but not yet active.
        setSyncAuthMode("signin");
        setSyncEmail("");
        setSyncPassword("");
        setSyncNotice(
          `Account created — check ${email} for a confirmation link, then sign in.`,
        );
      }
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncBusy(false);
    }
  }

  async function enableEncryption() {
    setSyncEncBusy(true);
    setSyncError(null);
    try {
      const status = await syncSetEncryption();
      setSyncStatusData(status);
      setSyncNotice("Encryption on — your data is now sent to Supabase as ciphertext.");
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncEncBusy(false);
    }
  }

  async function unlockEncryption() {
    const code = syncRecoveryInput().trim();
    if (!code) {
      setSyncError("Paste your recovery code.");
      return;
    }
    setSyncEncBusy(true);
    setSyncError(null);
    try {
      const status = await syncImportKey(code);
      setSyncStatusData(status);
      setSyncRecoveryInput("");
      setSyncNotice("Unlocked — this device can now decrypt your backup.");
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncEncBusy(false);
    }
  }

  async function showRecoveryCode() {
    try {
      setSyncRecoveryShown(await syncRecoveryCode());
    } catch (e) {
      setSyncError(String(e));
    }
  }

  async function disableEncryption() {
    if (!confirm("Disable end-to-end encryption? Sync will re-upload all data as plaintext.")) {
      return;
    }
    setSyncEncBusy(true);
    setSyncError(null);
    try {
      const status = await syncRemoveEncryption();
      setSyncStatusData(status);
      setSyncNotice("Encryption off — sync now sends plaintext.");
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncEncBusy(false);
    }
  }

  async function doSyncSignOut() {
    setSyncBusy(true);
    setSyncError(null);
    try {
      await syncSignOut();
      setSyncPassword("");
      setSyncError(null);
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncBusy(false);
      await refreshSync();
    }
  }

  async function doSyncNow() {
    setSyncBusy(true);
    setSyncError(null);
    try {
      setSyncStatusData(await syncNow());
    } catch (e) {
      setSyncError(String(e));
      await refreshSync();
    } finally {
      setSyncBusy(false);
    }
  }

  function formatLastSync(ms: number | null): string {
    if (!ms) return "never";
    return new Date(ms).toLocaleString();
  }

  function syncLabel(d: SyncStatus | null): string {
    if (d?.phase === "pull") return "Pulling changes…";
    if (d?.phase === "push") return "Pushing changes…";
    return "Syncing…";
  }

  // During the push phase we know how much is left, so the bar is real progress
  // (pushed / pushed+pending). Pull has no total, so it stays full + pulsing.
  function syncBarWidth(d: SyncStatus | null): number {
    if (!d || d.phase !== "push" || d.pushed + d.pending === 0) return 100;
    return Math.max(6, Math.min(100, Math.round((d.pushed / (d.pushed + d.pending)) * 100)));
  }

  // Keep the Cloud sync + memory backfill panels live while Settings is open,
  // instead of a one-shot snapshot that only refreshes on open/action.
  createEffect(() => {
    if (!settingsOpen()) return;
    const tick = () => {
      syncStatus().then(setSyncStatusData).catch(() => {});
      if (backfill()?.running) {
        backfillStatus().then(setBackfill).catch(() => {});
      }
    };
    tick();
    const id = window.setInterval(tick, 1000);
    onCleanup(() => window.clearInterval(id));
  });

  // Pull the model catalog (models.dev when the endpoint matches, else the
  // endpoint's own `/models`). Prices ride along in the same cache. Cached
  // values render first; a forced refresh replaces them once it resolves.
  // `providerId` scopes the call to a specific provider; when omitted it falls
  // back to the header/active provider.
  //
  // Because the backend caches model lists per base URL, concurrent `listModels`
  // calls can resolve out of order — a fast cached response for a *previous*
  // provider can land after a slow network fetch for the *current* one and
  // overwrite it, so the list "falls behind" to the old provider. `seq` guards
  // against that: only the request that was issued last is allowed to apply its
  // result, so the model list always reflects the most recent selection.
  let modelsReqSeq = 0;
  // Non-reactive bookkeeping for the header-sync effect (below). Tracked via
  // these plain variables instead of signals so the effect doesn't retrigger on
  // changes it itself causes (setModel / loadModels) — see the effect comment.
  let lastSyncId: string | null = null;
  let lastSyncAp = "";
  let lastModelsProv: string | null = null;
  async function loadModels(refresh: boolean, providerId?: string | null) {
    const prov = providerId ?? headerProviderId() ?? activeProviderId() ?? null;
    const seq = ++modelsReqSeq;
    setModelsLoading(true);
    try {
      const rows = await listModels(refresh, prov);
      // A newer request superseded this one — drop the stale result.
      if (seq !== modelsReqSeq) return;
      setModels(rows);
      lastModelsProv = prov;
      setCatalogVersion((v) => v + 1);
    } finally {
      if (seq === modelsReqSeq) setModelsLoading(false);
    }
  }

  async function changeModel(id: string) {
    if (!id) return;
    setModel(id);
    // The warning is about the previous model; re-check against the new one.
    setAttachmentWarning(null);
    // Adopt the new model's thinking levels; drop a level it doesn't accept.
    const opts = await thinkingOptions(id).catch(() => null);
    setThinkingOpts(opts);
    let level = thinkingLevel();
    if (opts && level && !opts.options.includes(level)) {
      level = "";
      setThinkingLevel("");
    }
    try {
      await setConfig({
        baseUrl: baseUrl(),
        model: id,
        preferences: preferences(),
        thinkingLevel: level,
        echoReasoningContent: echoReasoning(),
        memoryReflectionEnabled: reflectionEnabled(),
        memoryReflectionIdleMinutes: reflectionIdleMinutes(),
        modelOverrides: modelOverrides(),
        shellWorkspaceDir: shellWorkspaceDir(),
        syncEnabled: syncEnabled(),
        providers: providerConfig(),
        activeProviderId: activeProviderId(),
      });
      // With a conversation active, persist the per-conversation model so the
      // stream uses it (the backend defaults to the active provider's model
      // otherwise).
      const conv = activeConversation();
      if (conv) await setConversationModel(conv.id, id);
    } catch (e) {
      setSettingsError(String(e));
    }
  }

  async function changeThinking(level: string) {
    setThinkingLevel(level);
    try {
      await setConfig({
        baseUrl: baseUrl(),
        model: model(),
        preferences: preferences(),
        thinkingLevel: level,
        echoReasoningContent: echoReasoning(),
        memoryReflectionEnabled: reflectionEnabled(),
        memoryReflectionIdleMinutes: reflectionIdleMinutes(),
        modelOverrides: modelOverrides(),
        shellWorkspaceDir: shellWorkspaceDir(),
        syncEnabled: syncEnabled(),
        providers: providerConfig(),
        activeProviderId: activeProviderId(),
      });
    } catch (e) {
      setSettingsError(String(e));
    }
  }

  function selectMemoryFile(file: MemoryFile) {
    setMemorySelected(file.name);
    setMemoryDraft(file.content);
    setMemoryConfirmDelete(null);
    setMemoryError(null);
  }

  async function refreshReflectionStats() {
    try {
      setReflectionStats(await memoryReflectionStats());
    } catch {
      // non-fatal
    }
  }

  async function refreshExtractionStats() {
    try {
      setExtractionStats(await memoryExtractionStats());
    } catch {
      // non-fatal
    }
  }

  // Backfill: extract from every conversation in parallel, then consolidate the
  // staged summaries into memory in one serial pass. The command returns
  // immediately; progress arrives on the `memory-backfill` event.
  async function startBackfill() {
    setMemoryError(null);
    try {
      setBackfill(await backfillMemories());
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  async function stopBackfill() {
    try {
      await cancelBackfill();
    } catch {
      // non-fatal
    }
  }

  async function discardExtractions() {
    try {
      await clearExtractions();
      await refreshExtractionStats();
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  // Import an Anthropic-format export: a single `conversations.json`.
  async function importChats() {
    setMemoryError(null);
    try {
      const picked = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "Anthropic export", extensions: ["json"] }],
      });
      if (typeof picked !== "string") return;
      setImportReport(await importConversations(picked));
      await refreshConversations();
      await refreshExtractionStats();
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  // Reload the file list, keeping (or choosing) a selection.
  async function reloadMemory(prefer?: string) {
    try {
      const files = await listMemoryFiles();
      setMemoryFiles(files);
      const wanted =
        files.find((f) => f.name === (prefer ?? memorySelected())) ??
        files.find((f) => f.core) ??
        files[0];
      if (wanted) selectMemoryFile(wanted);
      void refreshReflectionStats();
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  async function openMemory() {
    setMemoryError(null);
    setMemoryOpen(true);
    await reloadMemory();
    void refreshExtractionStats();
    try {
      setBackfill(await backfillStatus());
    } catch {
      // non-fatal
    }
  }

  // Manually run memory consolidation over the active conversation.
  const [consolidating, setConsolidating] = createSignal(false);
  async function consolidateNow() {
    const id = activeId();
    if (!id || consolidating()) return;
    setConsolidating(true);
    try {
      await reflectNow(id);
      await reloadMemory(memorySelected() ?? undefined);
      if (activeId() === id) listMessages(id).then(setMessages);
    } catch (e) {
      setMemoryError(String(e));
    } finally {
      setConsolidating(false);
    }
  }

  async function saveSelectedMemory() {
    const name = memorySelected();
    if (!name) return;
    try {
      await writeMemoryFile(name, memoryDraft());
      await reloadMemory(name);
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  async function removeMemoryFile(name: string) {
    setMemoryConfirmDelete(null);
    try {
      await deleteMemoryFile(name);
      setMemorySelected(null);
      await reloadMemory();
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  const selectedMemoryFile = () =>
    memoryFiles().find((f) => f.name === memorySelected()) ?? null;

  // models.dev knows which models accept image input. Warn rather than let the
  // provider silently drop the image or reject the whole turn.
  const currentModelVision = () => {
    const id = model();
    if (!id) return null;
    return models().find((m) => m.id === id)?.vision ?? null;
  };

  async function addAttachmentPaths(paths: string[]) {
    if (paths.length === 0) return;
    try {
      const atts = await readAttachments(paths);
      if (atts.length === 0) return;
      setPendingAttachments((prev) => [...prev, ...atts]);
      const images = atts.filter((a) => a.kind === "image").length;
      if (images > 0 && currentModelVision() === false) {
        setAttachmentWarning(
          `${model()} isn't marked as accepting image input, so ${
            images === 1 ? "this image" : "these images"
          } may be ignored or rejected. Pick a vision model in the header.`,
        );
      } else {
        setAttachmentWarning(null);
      }
    } catch (e) {
      setStreamError(String(e));
    }
  }

  // File explorer button: native open dialog via the Tauri dialog plugin.
  async function pickAttachments() {
    try {
      const picked = await open({
        multiple: true,
        directory: false,
        filters: [
          {
            name: "Images & documents",
            extensions: [
              "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "pdf",
              "txt", "md", "json", "csv", "log", "yaml", "yml", "toml",
              "xml", "html", "css", "js", "ts", "tsx", "jsx", "rs", "py",
              "go", "java", "c", "cpp", "h",
            ],
          },
          { name: "All files", extensions: ["*"] },
        ],
      });
      if (!picked) return;
      await addAttachmentPaths(Array.isArray(picked) ? picked : [picked]);
    } catch (e) {
      setStreamError(String(e));
    }
  }

  function removeAttachment(id: string) {
    setPendingAttachments((prev) => prev.filter((a) => a.id !== id));
    setAttachmentWarning(null);
  }

  async function send() {
    const staged = pendingAttachments();
    if (streaming() || (!draft().trim() && staged.length === 0)) return;
    let id = activeId();
    if (!id) {
      const created = await createConversation();
      setConversations([created, ...conversations()]);
      setVisibleConversations([created, ...visibleConversations()]);
      setActiveId(created.id);
      id = created.id;
    }
    const text = draft().trim();

    const userMsg: Message = {
      id: `tmp-user-${tempId++}`,
      conversation_id: id,
      role: "user",
      index: messages().length,
      content: text,
      model: null,
      provider: null,
      thinking_level: null,
      thinking: null,
      usage: null,
      stop_reason: null,
      attachments: staged.length > 0 ? JSON.stringify(staged) : null,
      created_at: Date.now(),
    };
    const assistantKey = `tmp-assistant-${tempId++}`;
    const assistantMsg: Message = {
      id: assistantKey,
      conversation_id: id,
      role: "assistant",
      index: messages().length + 1,
      content: "",
      model: null,
      provider: null,
      thinking_level: null,
      thinking: "",
      usage: null,
      stop_reason: null,
      attachments: null,
      created_at: Date.now(),
    };

    setMessages([...messages(), userMsg, assistantMsg]);
    setNearBottom(true);
    setDraft("");
    setPendingAttachments([]);
    setStreaming(true);
    setStreamError(null);
    setLiveTools([]);
    setThinkingOpenId(null);

    const received = { current: false };
    const channel = new Channel<StreamEvent>();
    channel.onmessage = (ev) => {
      if (activeId() !== id) return;
      if (ev.type === "delta") {
        received.current = true;
        setMessages((prev) =>
          prev.map((m) =>
            m.id === assistantKey ? { ...m, content: m.content + ev.text } : m,
          ),
        );
      } else if (ev.type === "thinkingDelta") {
        received.current = true;
        setMessages((prev) =>
          prev.map((m) =>
            m.id === assistantKey ? { ...m, thinking: (m.thinking ?? "") + ev.text } : m,
          ),
        );
      } else if (ev.type === "toolCall") {
        received.current = true;
        setLiveTools((prev) => [
          ...prev,
          {
            callId: ev.callId,
            name: ev.name,
            arguments: formatToolArgs(ev.arguments),
            gated: ev.gated,
            state: ev.gated ? "pending" : "running",
          },
        ]);
      } else if (ev.type === "toolResult") {
        setLiveTools((prev) =>
          prev.map((t) =>
            t.callId === ev.callId
              ? { ...t, state: "done", ok: ev.ok, output: ev.output }
              : t,
          ),
        );
      } else if (ev.type === "done" || ev.type === "error") {
        if (ev.type === "error") setStreamError(ev.message);
        setStreaming(false);
        setLiveTools([]);
        listMessages(id).then(setMessages);
        refreshConversations().catch(() => {});
      }
    };

    try {
      await streamChat(id, text, staged, channel);
    } catch (e) {
      if (activeId() !== id) return;
      if (!received.current) {
        // Command errored before streaming began (e.g. missing key/match).
        setMessages((prev) =>
          prev.filter((m) => m.id !== assistantKey && m.id !== userMsg.id),
        );
        setDraft(text);
        setPendingAttachments(staged);
      }
      setStreaming(false);
      setLiveTools([]);
      setStreamError(String(e));
    }
  }

  async function stop() {
    const id = activeId();
    if (!id) return;
    try {
      await stopChat(id);
    } catch {
      // noop — the stream will settle via its done event
    }
  }

  function approveLiveTool(callId: string) {
    setLiveTools((prev) =>
      prev.map((x) => (x.callId === callId ? { ...x, state: "running" } : x)),
    );
    approveTool(callId).catch(() => {});
  }

  function denyLiveTool(callId: string) {
    setLiveTools((prev) =>
      prev.map((x) => (x.callId === callId ? { ...x, state: "running" } : x)),
    );
    denyTool(callId).catch(() => {});
  }

  // Group messages into per-turn timelines, augmented with live tool activity.
  const turns = () => groupTurns(messages(), liveTools(), streaming());

  const activeTitle = () => {
    const id = activeId();
    return conversations().find((c) => c.id === id)?.title ?? "New chat";
  };

  const activeConversation = () => {
    const id = activeId();
    return id ? conversations().find((c) => c.id === id) ?? null : null;
  };

  // Config's `providers` field is the slimmer Provider[] shape; map the rich
  // ProviderInfo list (with live key/active flags) down to what setConfig needs.
  const providerConfig = (): Provider[] =>
    providers().map((p) => ({
      id: p.id,
      name: p.name,
      baseUrl: p.baseUrl,
      defaultModel: p.defaultModel ?? null,
      catalogId: p.catalogId ?? null,
    }));

  // The header's provider selector. A non-empty list is the real provider set;
  // an empty list is a legacy single-provider config, exposed as one implicit
  // entry so the existing header/settings behavior is preserved.
  const providerOptions = (): ProviderInfo[] => {
    if (providers().length > 0) return providers();
    return [
      {
        id: "",
        name: "Default",
        baseUrl: baseUrl(),
        defaultModel: model(),
        catalogId: null,
        hasKey: hasKey(),
        active: true,
      },
    ];
  };

  async function refreshProviders() {
    setProvidersLoading(true);
    try {
      const rows = await listProviders();
      setProviders(rows);
      const active = rows.find((p) => p.active);
      if (active) setActiveProviderId(active.id);
      setModelsDevProviders(await listModelsDevProviders().catch(() => []));
    } catch (e) {
      setProviderError(String(e));
    } finally {
      setProvidersLoading(false);
    }
  }

  async function doAddProvider() {
    let name = addName().trim();
    let url = addUrl().trim();
    const pick = addPick();
    if (pick) {
      const chosen = modelsDevProviders().find((p) => p.id === pick);
      if (chosen) {
        if (!name) name = chosen.name;
        if (!url) url = chosen.baseUrl;
      }
    }
    if (!name || !url) {
      setProviderError("Enter a name and a base URL.");
      return;
    }
    setProviderError(null);
    try {
      // catalogId is the models.dev provider id only when the user picked one
      // from the catalog (enables models.dev listing/pricing); a manual add
      // (pick "") keeps it null so models.dev is never used for it.
      await addProvider(name, url, addKey().trim() || null, pick || null);
      await refreshProviders();
      setAddOpen(false);
      setAddName("");
      setAddUrl("");
      setAddKey("");
      setAddPick("");
    } catch (e) {
      setProviderError(String(e));
    }
  }

  function startEdit(p: ProviderInfo) {
    setEditId(p.id);
    setEditName(p.name);
    setEditUrl(p.baseUrl);
    setEditKey("");
    setProviderError(null);
  }

  function cancelEdit() {
    setEditId(null);
    setProviderError(null);
  }

  async function doUpdateProvider() {
    const id = editId();
    if (!id) return;
    const name = editName().trim();
    const url = editUrl().trim();
    if (!name || !url) {
      setProviderError("Enter a name and a base URL.");
      return;
    }
    setProviderError(null);
    try {
      // A non-empty edit sets the key; an empty edit leaves it unchanged.
      // A separate "Clear key" action removes it.
      const key = editKey().trim();
      await updateProvider(id, name, url, key ? key : undefined);
      await refreshProviders();
      setEditId(null);
    } catch (e) {
      setProviderError(String(e));
    }
  }

  async function doClearProviderKey(id: string) {
    setProviderError(null);
    try {
      await updateProvider(id, undefined, undefined, "");
      await refreshProviders();
    } catch (e) {
      setProviderError(String(e));
    }
  }

  async function doSetActive(id: string) {
    setProviderError(null);
    try {
      await setActiveProvider(id);
      await refreshProviders();
      const conv = activeConversation();
      if (conv && !conv.provider_id) {
        // New chats/current defaulted chat now resolves to the new active one.
        setHeaderProviderId(id);
      }
    } catch (e) {
      setProviderError(String(e));
    }
  }

  async function doRemoveProvider(id: string) {
    setProviderError(null);
    try {
      const rows = await removeProvider(id);
      setProviders(rows);
      const active = rows.find((p) => p.active);
      if (active) setActiveProviderId(active.id);
    } catch (e) {
      setProviderError(String(e));
    }
  }

  // Header: switching the active conversation's provider. With a conversation
  // active we persist the choice (null reverts it to the active provider),
  // reload that provider's model list and adopt a default model for it.
  async function changeHeaderProvider(id: string) {
    const conv = activeConversation();
    // The provider in effect before this switch (the conversation's explicit
    // override, or failing that the active provider) — used to roll back the
    // header + conversation if the new provider's model list fails to load.
    const target = !conv
      ? null
      : conv.provider_id === null || conv.provider_id === ""
        ? null
        : conv.provider_id;
    const prevEff = target ?? activeProviderId();
    setHeaderProviderId(id);
    if (conv) {
      const next = target === id ? null : id;
      try {
        await setConversationProvider(conv.id, next);
        await refreshConversations();
        setProviderError(null);
      } catch (e) {
        setProviderError(String(e));
        return;
      }
    }
    try {
      await loadModels(true, id || null);
    } catch (e) {
      // The new provider couldn't list models (bad key, unreachable endpoint,
      // ...): snap back to the previous provider and conversation state.
      setProviderError(String(e));
      setHeaderProviderId(prevEff);
      if (conv) {
        try {
          await setConversationProvider(conv.id, target);
          await refreshConversations();
        } catch {
          // best-effort rollback
        }
      }
      return;
    }
    const prov = providers().find((p) => p.id === id);
    const fallback = prov?.defaultModel ?? models()[0]?.id ?? "";
    if (fallback && fallback !== model()) void changeModel(fallback);
  }

  // When the active conversation (or the active provider) changes, reflect the
  // conversation's provider + model in the header and load that provider's
  // model list. New chats carry no explicit provider/model, so they resolve to
  // the active provider + its default model.
  //
  // This effect previously also read `model()` and `modelsProviderId()` to
  // compare, which made it re-run the moment changeModel/changeHeaderProvider
  // updated them — and it would then snap the pickers back to a stale
  // conversation record (the "can't change the model / provider snaps back"
  // bug). It must key ONLY on the active conversation id and the active
  // provider id, neither of which changes from a manual selection, and use the
  // plain `lastModelsProv` variable (not a signal) for its load bookkeeping.
  createEffect(() => {
    const convId = activeId();
    const ap = activeProviderId();
    const conv = convId ? activeConversation() : null;
    const prov = conv?.provider_id ?? ap;
    if (convId === lastSyncId && ap === lastSyncAp) return;
    lastSyncId = convId;
    lastSyncAp = ap;
    setHeaderProviderId(prov);
    if (conv?.model) setModel(conv.model);
    if (prov !== lastModelsProv) {
      lastModelsProv = prov;
      void loadModels(false, prov || null);
    }
  });


  // The active model is always the first option, so the picker can never lose
  // its selection when the catalog is replaced. Option values are plain
  // strings (not reactive) to avoid a select/value update race.
  const modelOptions = (): ModelInfo[] => {
    const id = model();
    const rest = models().filter((m) => m.id !== id);
    const current = models().find((m) => m.id === id);
    return [{ id, name: current?.name ?? id, vision: current?.vision ?? null }, ...rest];
  };

  // Re-apply the selection after the option list is rebuilt. WebKit clears a
  // <select> when its selected <option> is removed during a refresh, and the
  // `value` binding alone won't restore it because `model()` is unchanged.
  let modelSelectEl: HTMLSelectElement | undefined;
  createEffect(() => {
    const value = model();
    models();
    if (modelSelectEl && modelSelectEl.value !== value) {
      modelSelectEl.value = value;
    }
  });

  // Same WebKit quirk for the provider select: when the neighbor model select
  // rebuilds its options (e.g. the new provider's model list lands), WebKit
  // resets this select to its first option — which is the active/old provider —
  // and the `value` binding won't restore it because `headerProviderId()` is
  // unchanged. Force the DOM value back to the selected provider whenever the
  // provider selection, the option list, or the model-list (neighbor) rebuild
  // changes.
  let providerSelectEl: HTMLSelectElement | undefined;
  createEffect(() => {
    const value = headerProviderId();
    providers();
    models();
    modelsLoading();
    if (providerSelectEl && providerSelectEl.value !== value) {
      providerSelectEl.value = value;
    }
  });

  const openThinking = () => {
    const id = thinkingOpenId();
    if (!id) return null;
    const m = messages().find((x) => x.id === id);
    return m && m.thinking?.trim() ? m : null;
  };

  const thinkingLive = () => {
    const m = openThinking();
    return Boolean(streaming() && m && m.id.startsWith("tmp-assistant-") && !m.content);
  };

  // Context window + cost readouts for the active conversation. The usable
  // budget is 80% of the model's window, capped at 200k; unknown → 200k.
  const contextWindow = () => {
    const ctx = pricing()?.contextWindow;
    return ctx ? Math.min(ctx * 0.8, 200_000) : 200_000;
  };

  const contextTokens = () => {
    const assistants = messages().filter((m) => m.role === "assistant");
    for (let i = assistants.length - 1; i >= 0; i--) {
      const u = parseUsage(assistants[i]);
      if (!u) continue;
      if (typeof u.context_tokens === "number") return u.context_tokens;
      const p = u.prompt_tokens ?? 0;
      const c = u.completion_tokens ?? 0;
      if (p || c) return p + c;
    }
    // No reported usage yet: rough estimate (~4 chars/token).
    if (messages().length === 0) return 0;
    const chars = messages().reduce(
      (n, m) => n + m.content.length + (m.thinking?.length ?? 0),
      0,
    );
    return Math.ceil(chars / 4);
  };

  // Conversation cost: each turn's frozen `cost` when present (so switching
  // models never retroactively re-prices past turns); legacy turns are priced
  // with the model that produced them.
  const conversationCost = () => {
    let cost = 0;
    let known = false;
    const byModel = pricingByModel();
    for (const m of messages()) {
      if (m.role !== "assistant") continue;
      const u = parseUsage(m);
      if (!u) continue;
      if (typeof u.cost === "number") {
        cost += u.cost;
        known = true;
        continue;
      }
      const p = m.model ? byModel[m.model] : undefined;
      const eff = p ?? (m.model === model() ? pricing() : null);
      if (!eff) continue;
      const c = costOfUsage(u, eff);
      if (c != null) {
        cost += c;
        known = true;
      }
    }
    return known ? cost : null;
  };

  const costLabel = () => {
    const c = conversationCost();
    return c == null ? "—" : formatCost(c);
  };

  // Reflection spend (kept separate from conversation cost).
  const reflectionLabel = () => {
    const s = reflectionStats();
    if (!s || s.count <= 0) return null;
    return `${formatCost(s.cost)} across ${s.count} ${s.count === 1 ? "run" : "runs"}`;
  };

  const contextPct = () => {
    const win = contextWindow();
    return win > 0 ? Math.min(100, (contextTokens() / win) * 100) : 0;
  };

  const contextClass = () => {
    if (contextPct() >= 90) return "text-red-600 dark:text-red-400";
    if (contextPct() >= 75) return "text-amber-600 dark:text-amber-400";
    return "";
  };

  return (
    <div class="flex h-screen w-screen overflow-hidden bg-white text-neutral-900 dark:bg-neutral-950 dark:text-neutral-100">
      <Show when={dragOver()}>
        <div class="pointer-events-none fixed inset-0 z-[60] flex items-center justify-center bg-black/40">
          <div class="rounded-xl border-2 border-dashed border-white/70 px-6 py-4 text-sm font-medium text-white">
            Drop files to attach
          </div>
        </div>
      </Show>
      {/* Sidebar backdrop — narrow viewports only, closes the drawer on tap */}
      <Show when={sidebarOpen()}>
        <div
          class="fixed inset-0 z-30 bg-black/40 lg:hidden"
          onClick={() => setSidebarOpen(false)}
          aria-hidden="true"
        />
      </Show>
      {/* Sidebar — a slide-in drawer below `lg`, always in flow at `lg` and up */}
      <aside
        id="app-sidebar"
        inert={drawerHidden()}
        aria-hidden={drawerHidden() ? "true" : undefined}
        class={`fixed inset-y-0 left-0 z-40 flex w-72 shrink-0 flex-col border-r border-neutral-200 bg-neutral-50 transition-transform lg:static lg:z-auto lg:translate-x-0 lg:transition-none dark:border-neutral-800 dark:bg-neutral-900 ${
          sidebarOpen() ? "translate-x-0" : "-translate-x-full"
        }`}
      >
        <div class="flex items-center gap-2 border-b border-neutral-200 p-3 dark:border-neutral-800">
          <span class="mr-auto pl-1 text-sm font-semibold tracking-tight">Pi Chat</span>

          <Dialog open={newOpen()} onOpenChange={setNewOpen}>
            <Dialog.Trigger class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300">
              + New
            </Dialog.Trigger>
            <Dialog.Portal>
              <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
              <Dialog.Content class="fixed left-1/2 top-1/2 z-50 w-full max-w-sm -translate-x-1/2 -translate-y-1/2 rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900">
                <Dialog.Title class="text-base font-semibold">New chat</Dialog.Title>
                <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
                  Give the conversation a name (optional).
                </Dialog.Description>
                <input
                  class="mt-4 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                  placeholder="Untitled chat"
                  value={draftTitle()}
                  onInput={(e) => setDraftTitle(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") createNewChat();
                  }}
                />
                <div class="mt-4 flex justify-end gap-2">
                  <Dialog.CloseButton class="rounded-md px-3 py-1.5 text-sm text-neutral-600 transition hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800">
                    Cancel
                  </Dialog.CloseButton>
                  <button
                    onClick={() => createNewChat()}
                    class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                  >
                    Create
                  </button>
                </div>
              </Dialog.Content>
            </Dialog.Portal>
          </Dialog>
          <button
            onClick={() => {
              setSidebarOpen(false);
              hamburgerEl?.focus();
            }}
            title="Close chats"
            aria-label="Close chats"
            class="rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-200 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
          >
            <CloseIcon />
          </button>
        </div>

        <div class="border-b border-neutral-200 px-3 py-2 dark:border-neutral-800">
          <input
            ref={(el) => (sidebarSearchEl = el)}
            class="w-full rounded-lg border border-neutral-300 bg-white px-3 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
            placeholder="Search chats…"
            value={searchQuery()}
            onInput={(e) => setSearchQuery(e.currentTarget.value)}
          />
        </div>

        <nav class="flex-1 overflow-y-auto px-2 pb-2 pt-2">
          <ul class="space-y-1">
            <For each={visibleConversations()}>
              {(c) => (
                <li class="group relative">
                  <Show
                    when={renameId() !== c.id}
                    fallback={
                      <input
                        class="w-full rounded-lg border border-neutral-400 bg-white px-3 py-2 text-sm outline-none dark:border-neutral-600 dark:bg-neutral-800"
                        value={renameValue()}
                        onInput={(e) => setRenameValue(e.currentTarget.value)}
                        onBlur={() => void commitRename(c.id)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void commitRename(c.id);
                          if (e.key === "Escape") setRenameId(null);
                        }}
                      />
                    }
                  >
                    <button
                      onClick={() => {
                        setActiveId(c.id);
                        setConfirmDeleteId(null);
                        setSidebarOpen(false);
                      }}
                      class={`w-full truncate rounded-lg px-3 py-2 pr-14 text-left text-sm transition ${
                        activeId() === c.id
                          ? "bg-neutral-200/80 font-medium dark:bg-neutral-800 dark:text-neutral-100"
                          : "text-neutral-700 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      }`}
                    >
                      {c.title}
                    </button>
                    <div class="absolute right-1.5 top-1/2 hidden -translate-y-1/2 gap-0.5 group-hover:flex">
                      <button
                        title="Rename"
                        class="rounded px-1.5 py-1 text-xs text-neutral-400 transition hover:bg-neutral-200 hover:text-neutral-700 dark:text-neutral-500 dark:hover:bg-neutral-800 dark:hover:text-neutral-200"
                        onClick={() => {
                          setRenameId(c.id);
                          setRenameValue(c.title);
                        }}
                      >
                        ✎
                      </button>
                      <Show
                        when={confirmDeleteId() === c.id}
                        fallback={
                          <button
                            title="Delete"
                            class="rounded px-1.5 py-1 text-xs text-neutral-400 transition hover:bg-red-100 hover:text-red-600 dark:text-neutral-500 dark:hover:bg-red-950 dark:hover:text-red-400"
                            onClick={() => setConfirmDeleteId(c.id)}
                          >
                            ✕
                          </button>
                        }
                      >
                        <button
                          title="Confirm delete"
                          class="rounded bg-red-600 px-1.5 py-1 text-xs text-white"
                          onClick={() => void removeConversation(c.id)}
                        >
                          Delete?
                        </button>
                      </Show>
                    </div>
                  </Show>
                </li>
              )}
            </For>
            <Show when={visibleConversations().length === 0}>
              <li class="px-3 py-2 text-sm text-neutral-400 dark:text-neutral-500">
                No chats found.
              </li>
            </Show>
          </ul>
        </nav>
      </aside>

      {/* Main */}
      <main class="flex min-w-0 flex-1 flex-col">
        <header class="flex items-center justify-between gap-3 border-b border-neutral-200 px-4 py-3 sm:px-6 dark:border-neutral-800">
          <div class="flex min-w-0 items-center gap-2">
            <button
              ref={(el) => (hamburgerEl = el)}
              onClick={() => setSidebarOpen(true)}
              title="Show chats"
              aria-label="Show chats"
              aria-controls="app-sidebar"
              aria-expanded={sidebarOpen()}
              class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              <MenuIcon />
            </button>
            <h1 class="truncate text-sm font-semibold">{activeTitle()}</h1>
          </div>
          <div class="flex min-w-0 flex-wrap items-center justify-end gap-2 sm:gap-3">
            <Show when={activeId()}>
              <div
                class="hidden items-center gap-1.5 rounded-md px-2 py-1 text-[11px] text-neutral-500 sm:flex dark:text-neutral-400"
                title={`Context: ${Math.min(contextTokens(), contextWindow()).toLocaleString()} / ${contextWindow().toLocaleString()} tokens\nInput / output: ${
                  pricing()?.inputPerMillion ?? "—"
                } / ${pricing()?.outputPerMillion ?? "—"} USD per 1M\nCache read / write: ${
                  pricing()?.cacheReadPerMillion ?? "—"
                } / ${pricing()?.cacheWritePerMillion ?? "—"} USD per 1M${
                  pricing()?.overridden ? "\n(using your overrides)" : ""
                }${reflectionLabel() ? `\nMemory reflection: ${reflectionLabel()}` : ""}`}
              >
                <span class={contextClass()}>
                  {formatTokens(Math.min(contextTokens(), contextWindow()))} /{" "}
                  {formatTokens(contextWindow())} tok
                </span>
                <span class="text-neutral-300 dark:text-neutral-600">·</span>
                <span>{costLabel()}</span>
              </div>
            </Show>
            <button
              onClick={() => setDark(!dark())}
              title="Toggle light/dark theme"
              aria-pressed={dark()}
              class="hidden rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 lg:inline-block dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              {dark() ? "Light" : "Dark"}
            </button>
            <Show when={providers().length > 0 && activeId()}>
              <select
                ref={(el) => (providerSelectEl = el)}
                class="hidden max-w-28 truncate rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 disabled:opacity-50 sm:block md:max-w-36 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
                title="Provider"
                value={headerProviderId()}
                onChange={(e) => void changeHeaderProvider(e.currentTarget.value)}
              >
                <For each={providerOptions()}>
                  {(p) => (
                    <option value={p.id}>
                      {p.name}
                      {!p.hasKey ? " (no key)" : ""}
                    </option>
                  )}
                </For>
              </select>
            </Show>
            <div class="relative">
              <select
                class="max-w-[9rem] pr-7 truncate rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 disabled:opacity-60 sm:max-w-56 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
                title="Model"
                aria-busy={modelsLoading()}
                disabled={modelsLoading()}
                ref={(el) => (modelSelectEl = el)}
                value={model()}
                onChange={(e) => {
                  const next = e.currentTarget.value;
                  // Ignore spurious events from the list refreshing, only
                  // persist a real user selection.
                  if (next && next !== model()) void changeModel(next);
                }}
              >
                <For each={modelOptions()}>
                  {(m) => <option value={m.id}>{m.name?.trim() || m.id}</option>}
                </For>
              </select>
              {/* Overlay the spinner on the select so it doesn't take layout
                  width — an inline sibling here pushes the neighbor controls
                  out and makes them clip on narrower windows. */}
              <Show when={modelsLoading()}>
                <span
                  role="status"
                  aria-label="Loading models"
                  title="Loading models…"
                  class="pointer-events-none absolute right-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 animate-spin rounded-full border-2 border-neutral-400 border-t-transparent"
                />
              </Show>
            </div>
            <Show
              when={thinkingOpts()?.supportsReasoning && thinkingOpts()!.options.length > 0}
            >
              <select
                class="hidden rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 lg:block dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
                title={
                  thinkingOpts()!.source === "models.dev"
                    ? "Thinking level (from models.dev)"
                    : "Thinking level (OpenAI reasoning_effort)"
                }
                value={thinkingLevel()}
                onChange={(e) => changeThinking(e.currentTarget.value)}
              >
                <option value="">Thinking: default</option>
                <For each={thinkingOpts()!.options}>
                  {(o) => <option value={o}>Thinking: {o}</option>}
                </For>
              </select>
            </Show>
            <button
              onClick={openMemory}
              class="hidden rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 lg:inline-block dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              Memory
            </button>
            <button
              onClick={openSettings}
              class="hidden rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 lg:inline-block dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              Settings
            </button>
            {/* Overflow menu: the header controls above collapse into this below
                `lg`, so nothing becomes unreachable when the window is narrow. */}
            <DropdownMenu>
              <DropdownMenu.Trigger
                title="More actions"
                aria-label="More actions"
                class="rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 data-[expanded]:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800 dark:data-[expanded]:bg-neutral-800"
              >
                <MoreIcon />
              </DropdownMenu.Trigger>
              <DropdownMenu.Portal>
                <DropdownMenu.Content
                  class="z-50 min-w-44 rounded-lg border border-neutral-200 bg-white p-1 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
                >
                  <DropdownMenu.Item class={MENU_ITEM_CLASS} onSelect={openMemory}>
                    Memory
                  </DropdownMenu.Item>
                  <DropdownMenu.Item class={MENU_ITEM_CLASS} onSelect={openSettings}>
                    Settings
                  </DropdownMenu.Item>
                  <DropdownMenu.Item
                    class={MENU_ITEM_CLASS}
                    onSelect={() => setDark(!dark())}
                  >
                    {dark() ? "Switch to light" : "Switch to dark"}
                  </DropdownMenu.Item>
                  <Show
                    when={
                      thinkingOpts()?.supportsReasoning && thinkingOpts()!.options.length > 0
                    }
                  >
                    <DropdownMenu.Separator class="my-1 h-px bg-neutral-200 dark:bg-neutral-800" />
                    <DropdownMenu.Sub>
                      <DropdownMenu.SubTrigger
                        class={`${MENU_ITEM_CLASS} justify-between gap-3`}
                      >
                        <span>Thinking: {thinkingLevel() || "default"}</span>
                        <span class="text-neutral-400">›</span>
                      </DropdownMenu.SubTrigger>
                      <DropdownMenu.Portal>
                        <DropdownMenu.SubContent
                          class="z-50 min-w-40 rounded-lg border border-neutral-200 bg-white p-1 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
                        >
                          <DropdownMenu.RadioGroup
                            value={thinkingLevel()}
                            onChange={(value) => changeThinking(value)}
                          >
                            <DropdownMenu.RadioItem value="" class={MENU_ITEM_CLASS}>
                              <span class="inline-block w-4">
                                {thinkingLevel() === "" ? "✓" : ""}
                              </span>
                              Default
                            </DropdownMenu.RadioItem>
                            <For each={thinkingOpts()!.options}>
                              {(o) => (
                                <DropdownMenu.RadioItem value={o} class={MENU_ITEM_CLASS}>
                                  <span class="inline-block w-4">
                                    {thinkingLevel() === o ? "✓" : ""}
                                  </span>
                                  Thinking: {o}
                                </DropdownMenu.RadioItem>
                              )}
                            </For>
                          </DropdownMenu.RadioGroup>
                        </DropdownMenu.SubContent>
                      </DropdownMenu.Portal>
                    </DropdownMenu.Sub>
                  </Show>
                </DropdownMenu.Content>
              </DropdownMenu.Portal>
            </DropdownMenu>
          </div>
        </header>

        <Show when={settingsError() && !settingsOpen()}>
          <p class="border-b border-neutral-200 bg-red-50 px-6 py-2 text-sm text-red-700 dark:border-neutral-800 dark:bg-red-950/40 dark:text-red-400">
            {settingsError()}
          </p>
        </Show>

        <section
          ref={(el) => (messagesScrollEl = el)}
          onScroll={(e) => {
            const el = e.currentTarget;
            setNearBottom(el.scrollHeight - el.scrollTop - el.clientHeight <= 80);
          }}
          class="flex-1 overflow-y-auto px-6 py-4"
        >
          <Show
            when={activeId()}
            fallback={
              <p class="text-sm text-neutral-400 dark:text-neutral-500">
                Select or create a conversation.
              </p>
            }
          >
            <div class="flex min-h-full flex-col justify-end space-y-3">
              <Index each={turns()}>
                {(turn) => {
                  const t = () => turn();
                  const answer = () => t().answer;
                  const showAnswer = () =>
                    !!answer() &&
                    (!!answer()!.content.trim() ||
                      (t().live && !!answer()!.id.startsWith("tmp-assistant-")));
                  return (
                    <div class="space-y-3">
                      <Show when={t().user}>
                        {(u) => (
                          <div class="flex justify-end">
                            <div class="max-w-[80%]">
                              <div class="rounded-xl bg-neutral-900 px-4 py-2.5 text-sm text-white dark:bg-white dark:text-neutral-900">
                                <div class="mb-0.5 text-[11px] opacity-60">you</div>
                                <Show when={parseAttachments(u()).length > 0}>
                                  <div class="mb-2 flex flex-wrap items-end gap-2">
                                    <For each={parseAttachments(u())}>
                                      {(a) =>
                                        a.kind === "image" && a.dataUrl ? (
                                          <img
                                            src={a.dataUrl}
                                            alt={a.name}
                                            class="max-h-64 max-w-full rounded-lg border border-white/20 dark:border-neutral-900/20"
                                          />
                                        ) : (
                                          <div class="flex items-center gap-2 rounded-lg border border-white/25 bg-white/10 px-2.5 py-1.5 text-xs dark:border-neutral-900/20 dark:bg-neutral-900/10">
                                            <DocIcon />
                                            <span class="max-w-52 truncate">{a.name}</span>
                                            <span class="opacity-60">{formatBytes(a.size)}</span>
                                          </div>
                                        )
                                      }
                                    </For>
                                  </div>
                                </Show>
                                <div class="whitespace-pre-wrap break-words">{u().content}</div>
                              </div>
                            </div>
                          </div>
                        )}
                      </Show>

                      <Show when={t().entries.length > 0}>
                        <Timeline
                          entries={t().entries}
                          live={t().live}
                          openThoughtId={thinkingOpenId()}
                          onToggleThought={(id) =>
                            setThinkingOpenId(thinkingOpenId() === id ? null : id)
                          }
                          onApprove={approveLiveTool}
                          onDeny={denyLiveTool}
                        />
                      </Show>

                      <Show when={showAnswer()}>
                        <div class="flex justify-start">
                          <div class="max-w-[80%]">
                            <div class="rounded-xl bg-neutral-100 px-4 py-2.5 text-sm dark:bg-neutral-800">
                              <div class="mb-0.5 text-[11px] opacity-60">
                                assistant
                                {answer()!.stop_reason === "aborted" && " (aborted)"}
                                {answer()!.stop_reason === "error" && " (error)"}
                              </div>
                              <Show when={parseAttachments(answer()!).length > 0}>
                                <div class="mb-2 flex flex-wrap items-end gap-2">
                                  <For each={parseAttachments(answer()!)}>
                                    {(a) =>
                                      a.kind === "image" && a.dataUrl ? (
                                        <img
                                          src={a.dataUrl}
                                          alt={a.name}
                                          class="max-h-64 max-w-full rounded-lg border border-neutral-300 dark:border-neutral-700"
                                        />
                                      ) : (
                                        <div class="flex items-center gap-2 rounded-lg border border-neutral-300 bg-neutral-100 px-2.5 py-1.5 text-xs dark:border-neutral-700 dark:bg-neutral-800">
                                          <DocIcon />
                                          <span class="max-w-52 truncate">{a.name}</span>
                                          <span class="opacity-60">{formatBytes(a.size)}</span>
                                        </div>
                                      )
                                    }
                                  </For>
                                </div>
                              </Show>
                              <Show
                                when={answer()!.content}
                                fallback={
                                  <div class="whitespace-pre-wrap break-words">
                                    {answer()!.content}
                                  </div>
                                }
                              >
                                <Markdown text={answer()!.content} />
                              </Show>
                              {t().live && answer()!.id.startsWith("tmp-assistant-") && (
                                <span class="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-neutral-400 align-text-bottom" />
                              )}
                            </div>
                          </div>
                        </div>
                      </Show>

                      <Show when={t().note}>
                        {(n) => (
                          <div class="flex justify-center">
                            <span class="rounded-full bg-neutral-100 px-3 py-1 text-[11px] text-neutral-500 dark:bg-neutral-900 dark:text-neutral-400">
                              {n().content}
                            </span>
                          </div>
                        )}
                      </Show>
                    </div>
                  );
                }}
              </Index>

              <Show when={messages().length === 0}>
                <p class="text-sm text-neutral-400 dark:text-neutral-500">No messages yet.</p>
              </Show>
            </div>
          </Show>

          <Show when={streamError()}>
            <p class="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-400">
              {streamError()}
            </p>
          </Show>
        </section>

        <footer class="border-t border-neutral-200 p-4 dark:border-neutral-800">
          <Show when={pendingAttachments().length > 0}>
            <div class="mb-2 flex flex-wrap items-center gap-2">
              <For each={pendingAttachments()}>
                {(a) => (
                  <div class="relative">
                    <Show
                      when={a.kind === "image" && a.dataUrl}
                      fallback={
                        <div class="flex items-center gap-2 rounded-lg border border-neutral-300 bg-neutral-100 px-2.5 py-1.5 text-xs dark:border-neutral-700 dark:bg-neutral-800">
                          <DocIcon />
                          <span class="max-w-52 truncate">{a.name}</span>
                          <span class="opacity-60">{formatBytes(a.size)}</span>
                        </div>
                      }
                    >
                      <img
                        src={a.dataUrl ?? ""}
                        alt={a.name}
                        class="h-16 w-16 rounded-lg border border-neutral-300 object-cover dark:border-neutral-700"
                      />
                    </Show>
                    <button
                      onClick={() => removeAttachment(a.id)}
                      title="Remove attachment"
                      class="absolute -right-1.5 -top-1.5 flex h-5 w-5 items-center justify-center rounded-full bg-neutral-900 text-xs leading-none text-white shadow transition hover:bg-red-600 dark:bg-white dark:text-neutral-900 dark:hover:bg-red-400"
                    >
                      ×
                    </button>
                  </div>
                )}
              </For>
            </div>
          </Show>
          <Show when={attachmentWarning()}>
            <p class="mb-2 rounded-lg border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-700 dark:bg-amber-950/40 dark:text-amber-300">
              {attachmentWarning()}
            </p>
          </Show>
          <div class="flex items-end gap-2">
            <button
              onClick={() => void pickAttachments()}
              disabled={streaming()}
              title="Attach files"
              class="flex h-[3rem] w-[3rem] shrink-0 items-center justify-center rounded-xl border border-neutral-300 text-neutral-500 transition hover:bg-neutral-100 disabled:opacity-40 dark:border-neutral-700 dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              <PaperclipIcon />
            </button>
            <textarea
              class="min-h-[3rem] flex-1 resize-none rounded-xl border border-neutral-300 px-4 py-3 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-900 dark:focus:border-neutral-500"
              rows={3}
              placeholder={streaming() ? "Assistant is replying…" : "Message Pi Chat…"}
              value={draft()}
              disabled={streaming()}
              onInput={(e) => setDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  send();
                }
              }}
            />
            <Show
              when={streaming()}
              fallback={
                <button
                  onClick={send}
                  disabled={!draft().trim() && pendingAttachments().length === 0}
                  class="h-[3rem] shrink-0 rounded-xl bg-neutral-900 px-5 text-sm font-medium text-white transition hover:bg-neutral-700 disabled:cursor-not-allowed disabled:opacity-40 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                >
                  Send
                </button>
              }
            >
              <button
                onClick={stop}
                class="h-[3rem] shrink-0 rounded-xl bg-red-600 px-5 text-sm font-medium text-white transition hover:bg-red-500"
              >
                Stop
              </button>
            </Show>
          </div>
        </footer>
      </main>

      {/* Settings dialog */}
      <Dialog
        open={settingsOpen()}
        onOpenChange={(open) => {
          if (open) setSettingsOpen(true);
          else void closeSettings();
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
          <Dialog.Content
            class={
              // On narrow (mobile) viewports — the same `lg` threshold that turns
              // the chat list into a drawer — expand to cover the whole viewport
              // so these pages don't render as a cramped centered card.
              wideViewport()
                ? "fixed left-1/2 top-1/2 z-50 max-h-[85vh] w-full max-w-md -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
                : "fixed inset-0 z-50 h-full w-full overflow-y-auto bg-white p-5 focus:outline-none dark:bg-neutral-900"
            }
          >
            <div class="flex items-center gap-2">
              {/* Mobile (full-screen) exit arrow — hidden on wide viewports, where
                  the centered card keeps its Done button. */}
              <Dialog.CloseButton
                title="Close settings"
                aria-label="Close settings"
                class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
              >
                <BackArrowIcon />
              </Dialog.CloseButton>
              <Dialog.Title class="text-base font-semibold">Settings</Dialog.Title>
            </div>
            <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
              OpenAI-compatible endpoint. Fireworks is prefilled.
            </Dialog.Description>

            <div class="mt-4 space-y-3">
              <Show when={providers().length === 0}>
                <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
                  <span class="block text-xs font-semibold text-neutral-600 dark:text-neutral-300">
                    Default provider
                  </span>
                  <p class="mt-0.5 mb-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                    No providers configured yet. Edit the built-in default below, or add a
                    provider with the button above.
                  </p>
                  <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                    Base URL
                    <input
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      value={baseUrl()}
                      onInput={(e) => setBaseUrl(e.currentTarget.value)}
                    />
                  </label>

                  <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                    API key
                    <input
                      type="password"
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder={
                        hasKey()
                          ? "••••••  saved in keychain — type to replace"
                          : "sk-… / fw_…"
                      }
                      value={keyDraft()}
                      onInput={(e) => {
                        setKeyDraft(e.currentTarget.value);
                      }}
                    />
                    <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                      Stored in the OS keychain, never on disk or in the app config.
                    </span>
                    <Show when={hasKey()}>
                      <button
                        onClick={async () => {
                          try {
                            await setApiKey(null);
                            setHasKey(false);
                            setKeyDraft("");
                          } catch (e) {
                            setSettingsError(String(e));
                          }
                        }}
                        class="mt-1 text-[11px] font-normal text-red-600 transition hover:underline dark:text-red-400"
                      >
                        Remove saved key
                      </button>
                    </Show>
                  </label>
                </div>
              </Show>

              <div>
                  <div class="mb-2 flex items-center justify-between">
                    <span class="text-xs font-semibold text-neutral-600 dark:text-neutral-300">
                      Providers
                    </span>
                    <button
                      onClick={() => {
                        setAddOpen(!addOpen());
                        setEditId(null);
                        setProviderError(null);
                      }}
                      class="rounded-md bg-neutral-900 px-2 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                    >
                      {addOpen() ? "Cancel" : "+ Add provider"}
                    </button>
                  </div>

                  <div class="space-y-2">
                    <Show when={providersLoading()}>
                      <p class="text-xs text-neutral-400 dark:text-neutral-500">
                        Loading providers…
                      </p>
                    </Show>
                    <For each={providers()}>
                      {(p) => (
                        <div class="rounded-lg border border-neutral-200 p-2 dark:border-neutral-700">
                          <Show
                            when={editId() === p.id}
                            fallback={
                              <div class="flex items-start gap-2">
                                <div class="min-w-0 flex-1">
                                  <div class="flex items-center gap-2">
                                    <span class="truncate text-xs font-medium text-neutral-700 dark:text-neutral-200">
                                      {p.name}
                                    </span>
                                    {p.active && (
                                      <span class="shrink-0 rounded-full bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-400">
                                        Active
                                      </span>
                                    )}
                                  </div>
                                  <div class="truncate text-[11px] text-neutral-400 dark:text-neutral-500">
                                    {p.baseUrl}
                                  </div>
                                  <div class="text-[11px] text-neutral-400 dark:text-neutral-500">
                                    {p.hasKey ? "Key saved" : "No key"}
                                    {p.defaultModel ? ` · ${p.defaultModel}` : ""}
                                  </div>
                                </div>
                                <div class="flex shrink-0 flex-col gap-1 text-right">
                                  {!p.active && (
                                    <button
                                      onClick={() => void doSetActive(p.id)}
                                      class="text-[11px] font-normal text-neutral-500 transition hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                                    >
                                      Set active
                                    </button>
                                  )}
                                  <button
                                    onClick={() => startEdit(p)}
                                    class="text-[11px] font-normal text-neutral-500 transition hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                                  >
                                    Edit
                                  </button>
                                  <button
                                    onClick={() => void doRemoveProvider(p.id)}
                                    class="text-[11px] font-normal text-red-600 transition hover:underline disabled:opacity-50 dark:text-red-400"
                                    disabled={p.active}
                                    title={p.active ? "Remove the active provider first" : undefined}
                                  >
                                    Remove
                                  </button>
                                </div>
                              </div>
                            }
                          >
                            <div class="space-y-2">
                              <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                                Name
                                <input
                                  class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                                  value={editName()}
                                  onInput={(e) => setEditName(e.currentTarget.value)}
                                />
                              </label>
                              <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                                Base URL
                                <input
                                  class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                                  value={editUrl()}
                                  onInput={(e) => setEditUrl(e.currentTarget.value)}
                                />
                              </label>
                              <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                                API key
                                <input
                                  type="password"
                                  class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                                  placeholder={
                                    p.hasKey
                                      ? "saved — blank keeps it, type to replace"
                                      : "sk-… / fw_…"
                                  }
                                  value={editKey()}
                                  onInput={(e) => setEditKey(e.currentTarget.value)}
                                />
                                <span class="mt-1 block text-[10px] font-normal text-neutral-400 dark:text-neutral-500">
                                  Stored in the OS keychain.
                                </span>
                              </label>
                              <Show when={p.hasKey}>
                                <button
                                  onClick={() => void doClearProviderKey(p.id)}
                                  class="text-[11px] font-normal text-red-600 transition hover:underline dark:text-red-400"
                                >
                                  Clear saved key
                                </button>
                              </Show>
                              <div class="flex gap-2">
                                <button
                                  onClick={() => void doUpdateProvider()}
                                  class="rounded-md bg-neutral-900 px-3 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                                >
                                  Save
                                </button>
                                <button
                                  onClick={cancelEdit}
                                  class="rounded-md border border-neutral-300 px-3 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                                >
                                  Cancel
                                </button>
                              </div>
                            </div>
                          </Show>
                        </div>
                      )}
                    </For>
                  </div>

                  <Show when={addOpen()}>
                    <div class="mt-2 space-y-2 rounded-lg border border-neutral-200 p-2 dark:border-neutral-700">
                      <Show when={modelsDevProviders().length > 0}>
                        <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                          Pick from catalog (optional)
                          <select
                            class="mt-1 w-full rounded-lg border border-neutral-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200"
                            value={addPick()}
                            onChange={(e) => {
                              const pick = e.currentTarget.value;
                              setAddPick(pick);
                              const chosen = modelsDevProviders().find((p) => p.id === pick);
                              if (chosen) {
                                // The pick is the source of truth: always overwrite the
                                // name/URL so switching catalog entries updates them.
                                // (Use "— custom —" to type a manual endpoint instead.)
                                setAddName(chosen.name);
                                setAddUrl(chosen.baseUrl);
                              }
                            }}
                          >
                            <option value="">— custom —</option>
                            <For each={modelsDevProviders()}>
                              {(p) => <option value={p.id}>{p.name}</option>}
                            </For>
                          </select>
                        </label>
                      </Show>
                      <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        Name
                        <input
                          class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                          placeholder="My provider"
                          value={addName()}
                          onInput={(e) => setAddName(e.currentTarget.value)}
                        />
                      </label>
                      <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        Base URL
                        <input
                          class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                          placeholder="https://api.example.com/inference/v1"
                          value={addUrl()}
                          onInput={(e) => setAddUrl(e.currentTarget.value)}
                        />
                      </label>
                      <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        API key (optional)
                        <input
                          type="password"
                          class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                          placeholder="sk-… / fw_…"
                          value={addKey()}
                          onInput={(e) => setAddKey(e.currentTarget.value)}
                        />
                      </label>
                      <div class="flex gap-2">
                        <button
                          onClick={() => void doAddProvider()}
                          class="rounded-md bg-neutral-900 px-3 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                        >
                          Add
                        </button>
                        <button
                          onClick={() => {
                            setAddOpen(false);
                            setAddPick("");
                          }}
                          class="rounded-md border border-neutral-300 px-3 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                        >
                          Cancel
                        </button>
                      </div>
                    </div>
                  </Show>

                  <Show when={providerError()}>
                    <p class="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-400">
                      {providerError()}
                    </p>
                  </Show>
                  <p class="mt-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                    Each provider has its own base URL, API key, and model list. The active one
                    is used for new chats.
                  </p>
                </div>

              <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Brave Search API key
                <input
                  type="password"
                  class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                  placeholder={
                    hasBraveKeySaved()
                      ? "••••••  saved in keychain — type to replace"
                      : "BSA… (optional — enables web search tools)"
                  }
                  value={braveKeyDraft()}
                  onInput={(e) => setBraveKeyDraft(e.currentTarget.value)}
                />
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Enables Brave's native search tools (web / news / images / videos / local /
                  summarizer) — handled directly, no external server, so it works on Android too.
                  Stored in the OS keychain.
                </span>
                <Show when={hasBraveKeySaved()}>
                  <button
                    onClick={async () => {
                      try {
                        await setBraveKey(null);
                        setHasBraveKeySaved(false);
                        setBraveKeyDraft("");
                      } catch (e) {
                        setSettingsError(String(e));
                      }
                    }}
                    class="mt-1 text-[11px] font-normal text-red-600 transition hover:underline dark:text-red-400"
                  >
                    Remove saved key
                  </button>
                </Show>
              </label>

              <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Explicit user preferences
                <textarea
                  class="mt-1 min-h-20 w-full resize-y rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                  placeholder="e.g. Prefer concise answers. Call me Sam. Always show units."
                  value={preferences()}
                  onInput={(e) => setPreferences(e.currentTarget.value)}
                />
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Injected at the top of every system prompt and treated as authoritative.
                  The assistant's inferred long-term memory lives in the Memory tab.
                </span>
              </label>

              <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                <span class="flex items-center gap-2">
                  <input
                    type="checkbox"
                    class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                    checked={echoReasoning()}
                    onChange={(e) => setEchoReasoning(e.currentTarget.checked)}
                  />
                  Send reasoning back to the model between tool calls
                </span>
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Required by DeepSeek thinking models; turn off if your provider rejects
                  unknown fields.
                </span>
              </label>

              <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
                <label class="flex items-center gap-2 text-xs font-medium text-neutral-600 dark:text-neutral-300">
                  <input
                    type="checkbox"
                    class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                    checked={reflectionEnabled()}
                    onChange={(e) => setReflectionEnabled(e.currentTarget.checked)}
                  />
                  Consolidate memory after conversations go idle
                </label>
                <div class="mt-2 flex items-center gap-2">
                  <span class="text-xs text-neutral-500 dark:text-neutral-400">Idle for</span>
                  <input
                    type="number"
                    min="1"
                    class="w-20 rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 disabled:opacity-50 dark:border-neutral-700 dark:bg-neutral-800"
                    disabled={!reflectionEnabled()}
                    value={reflectionIdleMinutes()}
                    onInput={(e) =>
                      setReflectionIdleMinutes(
                        Math.max(1, Number(e.currentTarget.value) || 1),
                      )
                    }
                  />
                  <span class="text-xs text-neutral-500 dark:text-neutral-400">minutes</span>
                </div>
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  A background pass reads the conversation and rewrites the Markdown memory
                  files. Spends tokens; cost is tracked separately.
                </span>
              </div>

              <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Shell working directory
                <input
                  class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                  placeholder="(default) /storage/emulated/0/PiChat"
                  value={shellWorkspaceDir()}
                  onInput={(e) => setShellWorkspaceDir(e.currentTarget.value)}
                />
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Where the bash / file tools work on your files. On Android this is a directory
                  under shared storage that the agent drives through Termux; on desktop the shell
                  inherits the app's working directory unless you set one here. Each tool call
                  runs in a fresh shell (no persistent <code>cd</code> / <code>export</code>).
                </span>
              </label>

              <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
                <label class="flex items-center gap-2 text-xs font-medium text-neutral-600 dark:text-neutral-300">
                  <input
                    type="checkbox"
                    class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                    checked={syncEnabled()}
                    disabled={syncBusy()}
                    onChange={(e) => void toggleSync(e.currentTarget.checked)}
                  />
                  Cloud backup &amp; sync
                </label>
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Back up and keep your chats, memory, and settings in sync across devices via
                  Supabase. Off by default — the app stays fully local until you sign in. Your
                  data is scoped to your account.
                </span>

                <Show when={syncEnabled()}>
                  <Show
                    when={syncStatusData()?.loggedIn}
                    fallback={
                      <div class="mt-3 space-y-2">
                        <div class="flex rounded-lg border border-neutral-300 p-0.5 dark:border-neutral-700">
                          {(["signin", "signup"] as const).map((m) => (
                            <button
                              type="button"
                              onClick={() => {
                                setSyncAuthMode(m);
                                setSyncError(null);
                                setSyncNotice(null);
                              }}
                              class={`flex-1 rounded-md px-2 py-1 text-[11px] font-medium transition ${
                                syncAuthMode() === m
                                  ? "bg-neutral-900 text-white dark:bg-white dark:text-neutral-900"
                                  : "text-neutral-500 hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                              }`}
                            >
                              {m === "signin" ? "Sign in" : "Create account"}
                            </button>
                          ))}
                        </div>
                        <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                          Email
                          <input
                            type="email"
                            class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            placeholder="you@example.com"
                            value={syncEmail()}
                            onInput={(e) => setSyncEmail(e.currentTarget.value)}
                          />
                        </label>
                        <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                          Password
                          <input
                            type="password"
                            class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            placeholder="••••••••"
                            value={syncPassword()}
                            onInput={(e) => setSyncPassword(e.currentTarget.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                void (syncAuthMode() === "signin"
                                  ? submitSyncSignIn()
                                  : submitSyncSignUp());
                              }
                            }}
                          />
                        </label>
                        <button
                          onClick={() =>
                            void (syncAuthMode() === "signin"
                              ? submitSyncSignIn()
                              : submitSyncSignUp())
                          }
                          disabled={syncBusy()}
                          class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                        >
                          {syncAuthMode() === "signin" ? "Sign in" : "Create account"}
                        </button>
                        <Show when={syncNotice()}>
                          <p class="mt-2 rounded-lg bg-emerald-50 px-3 py-2 text-xs text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-400">
                            {syncNotice()}
                          </p>
                        </Show>
                      </div>
                    }
                  >
                    <div class="mt-3 flex items-center gap-2">
                      <span class="truncate text-xs text-neutral-600 dark:text-neutral-300">
                        Signed in as <span class="font-medium">{syncStatusData()?.email}</span>
                      </span>
                      <button
                        onClick={() => void doSyncSignOut()}
                        disabled={syncBusy()}
                        class="ml-auto shrink-0 text-[11px] font-normal text-red-600 transition hover:underline disabled:opacity-50 dark:text-red-400"
                      >
                        Sign out
                      </button>
                    </div>
                    <button
                      onClick={() => void doSyncNow()}
                      disabled={syncBusy() || syncStatusData()?.syncing}
                      class="mt-2 rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                    >
                      Sync now
                    </button>

                    <div class="mt-4 border-t border-neutral-200 pt-3 dark:border-neutral-700">
                      <div class="flex items-center gap-2">
                        <span class="text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                          End-to-end encryption
                        </span>
                        <span
                          class={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${
                            syncStatusData()?.encryption
                              ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-400"
                              : syncStatusData()?.locked
                                ? "bg-amber-100 text-amber-700 dark:bg-amber-950/50 dark:text-amber-400"
                                : "bg-neutral-200 text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400"
                          }`}
                        >
                          {syncStatusData()?.encryption
                            ? "On"
                            : syncStatusData()?.locked
                              ? "Locked"
                              : "Off"}
                        </span>
                      </div>
                      <p class="mt-1 text-[11px] leading-snug text-neutral-400 dark:text-neutral-500">
                        {syncStatusData()?.encryption
                          ? "Data sent to Supabase is ciphertext — only this device can read it."
                          : syncStatusData()?.locked
                            ? "This backup is encrypted. Paste its recovery code to unlock it on this device."
                            : "Optional: encrypt the sync mirror so Supabase only ever sees ciphertext."}
                      </p>
                      <Show
                        when={syncStatusData()?.encryption}
                        fallback={
                          <Show
                            when={syncStatusData()?.locked}
                            fallback={
                              <div class="mt-2">
                                <button
                                  onClick={() => void enableEncryption()}
                                  disabled={syncEncBusy()}
                                  class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                                >
                                  {syncEncBusy() ? "Encrypting…" : "Enable encryption"}
                                </button>
                                <p class="mt-1 text-[10px] leading-snug text-neutral-400 dark:text-neutral-500">
                                  A key is generated and kept in your OS keychain. You'll get a
                                  recovery code to use on other devices.
                                </p>
                              </div>
                            }
                          >
                            <div class="mt-2 space-y-2">
                              <p class="text-[10px] leading-snug text-neutral-400 dark:text-neutral-500">
                                This backup is encrypted by another device. Paste its recovery
                                code to read it here.
                              </p>
                              <input
                                type="password"
                                placeholder="Recovery code"
                                value={syncRecoveryInput()}
                                onInput={(e) => setSyncRecoveryInput(e.currentTarget.value)}
                                class="w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                              />
                              <button
                                onClick={() => void unlockEncryption()}
                                disabled={syncEncBusy()}
                                class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                              >
                                {syncEncBusy() ? "Unlocking…" : "Unlock"}
                              </button>
                            </div>
                          </Show>
                        }
                      >
                        <div class="mt-2 space-y-2">
                          <Show
                            when={syncRecoveryShown()}
                            fallback={
                              <button
                                onClick={() => void showRecoveryCode()}
                                class="text-[11px] font-normal text-neutral-500 underline-offset-2 transition hover:text-neutral-800 disabled:opacity-50 dark:text-neutral-400 dark:hover:text-neutral-200"
                              >
                                Show recovery code
                              </button>
                            }
                          >
                            <p class="text-[10px] leading-snug text-neutral-400 dark:text-neutral-500">
                              Save this to unlock the backup on another device:
                            </p>
                            <code class="block select-all break-all rounded-lg bg-neutral-100 px-2 py-1.5 font-mono text-[10px] text-neutral-700 dark:bg-neutral-800 dark:text-neutral-300">
                              {syncRecoveryShown()}
                            </code>
                          </Show>
                          <button
                            onClick={() => void disableEncryption()}
                            disabled={syncEncBusy()}
                            class="block text-[11px] font-normal text-neutral-500 underline-offset-2 transition hover:text-red-600 disabled:opacity-50 dark:text-neutral-400 dark:hover:text-red-400"
                          >
                            Disable
                          </button>
                        </div>
                      </Show>
                    </div>
                  </Show>

                  <Show when={syncStatusData()?.syncing || syncStatusData()?.phase}>
                    <div class="mt-2">
                      <div class="flex items-baseline justify-between gap-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                        <span>{syncLabel(syncStatusData())}</span>
                        <span class="tabular-nums">
                          ↑{syncStatusData()?.pushed ?? 0} · ↓{syncStatusData()?.pulled ?? 0}{" "}
                          · {syncStatusData()?.pending ?? 0} pending
                        </span>
                      </div>
                      <div class="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-neutral-200 dark:bg-neutral-700">
                        <div
                          class={`h-full rounded-full bg-neutral-500 transition-all duration-500 dark:bg-neutral-400 ${
                            syncStatusData()?.phase === "pull" ? "animate-pulse" : ""
                          }`}
                          style={{ width: `${syncBarWidth(syncStatusData())}%` }}
                        />
                      </div>
                    </div>
                  </Show>

                  <p class="mt-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                    Last sync: {formatLastSync(syncStatusData()?.lastSyncAt ?? null)} ·{" "}
                    {syncStatusData()?.pending ?? 0} pending
                  </p>

                  <Show when={syncStatusData()?.lastError}>
                    <p class="mt-1 text-[11px] font-normal text-red-600 dark:text-red-400">
                      {syncStatusData()?.lastError}
                    </p>
                  </Show>

                  <Show when={syncError()}>
                    <p class="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-400">
                      {syncError()}
                    </p>
                  </Show>
                </Show>
              </div>

              <Show when={settingsError()}>
                <p class="rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-400">
                  {settingsError()}
                </p>
              </Show>
            </div>
            <p class="mt-5 text-right text-[11px] text-neutral-400 dark:text-neutral-500">
              Changes are saved when the dialog closes.
            </p>
            <div class="mt-2 flex justify-end">
              <Dialog.CloseButton class="rounded-md bg-neutral-900 px-4 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300">
                Done
              </Dialog.CloseButton>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog>

      {/* Memory tab */}
      <Dialog open={memoryOpen()} onOpenChange={setMemoryOpen}>
        <Dialog.Portal>
          <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
          <Dialog.Content
            class={
              // On narrow (mobile) viewports — the same `lg` threshold that turns
              // the chat list into a drawer — expand to cover the whole viewport
              // so these pages don't render as a cramped centered card.
              wideViewport()
                ? "fixed left-1/2 top-1/2 z-50 flex h-[70vh] w-full max-w-3xl -translate-x-1/2 -translate-y-1/2 flex-col rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
                : "fixed inset-0 z-50 flex h-full w-full flex-col overflow-y-auto bg-white p-5 focus:outline-none dark:bg-neutral-900"
            }
          >
            <div class="flex items-center gap-2">
              {/* Mobile (full-screen) exit arrow — hidden on wide viewports, where
                  the centered card keeps its Close button. */}
              <Dialog.CloseButton
                title="Close memory"
                aria-label="Close memory"
                class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
              >
                <BackArrowIcon />
              </Dialog.CloseButton>
              <Dialog.Title class="text-base font-semibold">Memory</Dialog.Title>
            </div>
            <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
              Long-term memory is plain Markdown files. Core files (profile, preferences,
              goals) are always shown to the assistant; other files are listed and read on
              demand. The idle reflection pass curates these files. Explicit user preferences
              are set in Settings and kept separate.
            </Dialog.Description>

            <div class="mt-4 flex min-h-0 flex-1 gap-4">
              <div class="flex w-44 shrink-0 flex-col gap-1 overflow-y-auto">
                <p class="shrink-0 px-2 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                  Core
                </p>
                <For each={memoryFiles().filter((f) => f.core)}>
                  {(f) => (
                    <button
                      onClick={() => selectMemoryFile(f)}
                      class={`truncate shrink-0 rounded-md px-2 py-1.5 text-left text-xs transition ${
                        memorySelected() === f.name
                          ? "bg-neutral-200 font-medium dark:bg-neutral-800"
                          : "text-neutral-600 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      }`}
                    >
                      {f.name}
                    </button>
                  )}
                </For>
                <Show when={memoryFiles().some((f) => !f.core)}>
                  <p class="mt-3 shrink-0 px-2 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                    Other
                  </p>
                  <For each={memoryFiles().filter((f) => !f.core)}>
                    {(f) => (
                      <button
                        onClick={() => selectMemoryFile(f)}
                        class={`truncate shrink-0 rounded-md px-2 py-1.5 text-left text-xs transition ${
                          memorySelected() === f.name
                            ? "bg-neutral-200 font-medium dark:bg-neutral-800"
                            : "text-neutral-600 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                        }`}
                      >
                        {f.name}
                      </button>
                    )}
                  </For>
                </Show>
              </div>

              <div class="flex min-w-0 flex-1 flex-col">
                <Show
                  when={selectedMemoryFile()}
                  fallback={
                    <p class="text-sm text-neutral-400 dark:text-neutral-500">
                      No memory file selected.
                    </p>
                  }
                >
                  <Show
                    when={!memoryPreview()}
                    fallback={
                      <div class="min-h-0 flex-1 overflow-y-auto rounded-lg border border-neutral-300 bg-white p-4 dark:border-neutral-700 dark:bg-neutral-950">
                        <Show
                          when={memoryDraft().trim()}
                          fallback={
                            <p class="text-xs text-neutral-400 dark:text-neutral-500">
                              (empty)
                            </p>
                          }
                        >
                          <Markdown text={memoryDraft()} class="prose-sm" />
                        </Show>
                      </div>
                    }
                  >
                    <textarea
                      class="min-h-0 flex-1 resize-none rounded-lg border border-neutral-300 bg-white p-3 font-mono text-xs outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
                      spellcheck={false}
                      value={memoryDraft()}
                      onInput={(e) => setMemoryDraft(e.currentTarget.value)}
                    />
                  </Show>
                  <div class="mt-3 flex items-center justify-between gap-3">
                    <Show
                      when={memoryConfirmDelete() === selectedMemoryFile()?.name}
                      fallback={
                        <button
                          onClick={() =>
                            setMemoryConfirmDelete(selectedMemoryFile()?.name ?? null)
                          }
                          class="text-[11px] text-red-600 transition hover:underline dark:text-red-400"
                        >
                          Delete file
                        </button>
                      }
                    >
                      <div class="flex items-center gap-2">
                        <span class="text-[11px] text-neutral-500 dark:text-neutral-400">
                          Delete {selectedMemoryFile()?.name}?
                        </span>
                        <button
                          onClick={() =>
                            void removeMemoryFile(selectedMemoryFile()?.name ?? "")
                          }
                          class="rounded bg-red-600 px-2 py-1 text-[11px] text-white transition hover:bg-red-500"
                        >
                          Delete
                        </button>
                        <button
                          onClick={() => setMemoryConfirmDelete(null)}
                          class="text-[11px] text-neutral-500 transition hover:underline dark:text-neutral-400"
                        >
                          Cancel
                        </button>
                      </div>
                    </Show>

                    <div class="flex items-center gap-2">
                      <Show when={memoryError()}>
                        <span class="max-w-56 truncate text-[11px] text-red-600 dark:text-red-400">
                          {memoryError()}
                        </span>
                      </Show>
                      <button
                        onClick={() => setMemoryPreview((p) => !p)}
                        class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                      >
                        {memoryPreview() ? "Edit" : "Preview"}
                      </button>
                      <button
                        onClick={() => void saveSelectedMemory()}
                        class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                      >
                        Save
                      </button>
                    </div>
                  </div>
                </Show>
              </div>
            </div>

            {/* Import an archive, then backfill it: map (parallel extraction)
                then reduce (one writer). */}
            <div class="mt-4 rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <div class="flex items-start justify-between gap-3">
                <div class="min-w-0">
                  <p class="text-xs font-medium">Imported chats</p>
                  <p class="mt-0.5 text-[11px] leading-relaxed text-neutral-500 dark:text-neutral-400">
                    Import an Anthropic export — a <code>conversations.json</code> file.
                    Imported chats keep their original dates and are kept out of the
                    idle reflection sweep. Backfill then extracts durable facts from
                    them in parallel and consolidates the summaries into memory in one
                    pass; extraction never writes to memory, only the final pass does.
                    Backfill covers only your most recent import.
                  </p>
                  <Show when={importReport()}>
                    <p class="mt-1 text-[11px] text-neutral-400 dark:text-neutral-500">
                      Imported {importReport()!.conversations} chats ·{' '}
                      {importReport()!.messages} messages
                      <Show when={importReport()!.skipped > 0}>
                        {' '}
                        · {importReport()!.skipped} skipped
                      </Show>
                    </p>
                  </Show>
                  <Show when={extractionStats()}>
                    <p class="mt-1 text-[11px] text-neutral-400 dark:text-neutral-500">
                      {extractionStats()!.staged} staged · {extractionStats()!.pending}{' '}
                      awaiting extraction
                    </p>
                  </Show>
                </div>
                <div class="flex shrink-0 flex-wrap items-center justify-end gap-2">
                  <button
                    onClick={() => void importChats()}
                    disabled={backfill()?.running}
                    class="rounded-md border border-neutral-300 px-2.5 py-1.5 text-[11px] text-neutral-600 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-300 dark:hover:bg-neutral-800"
                  >
                    Import file…
                  </button>
                  <Show when={extractionStats()?.staged}>
                    <button
                      onClick={() => void discardExtractions()}
                      disabled={backfill()?.running}
                      class="rounded-md border border-neutral-300 px-2.5 py-1.5 text-[11px] text-neutral-600 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-300 dark:hover:bg-neutral-800"
                    >
                      Discard staged
                    </button>
                  </Show>
                  <Show
                    when={backfill()?.running}
                    fallback={
                      <button
                        onClick={() => void startBackfill()}
                        class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                      >
                        Backfill
                      </button>
                    }
                  >
                    <button
                      onClick={() => void stopBackfill()}
                      class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                    >
                      Cancel
                    </button>
                  </Show>
                </div>
              </div>

              <Show when={backfill()?.running}>
                <div class="mt-2">
                  <div class="h-1 w-full overflow-hidden rounded-full bg-neutral-200 dark:bg-neutral-700">
                    <div
                      class="h-full rounded-full bg-neutral-900 transition-all dark:bg-white"
                      style={{
                        width: `${
                          backfill()!.total > 0
                            ? Math.round((backfill()!.done / backfill()!.total) * 100)
                            : 0
                        }%`,
                      }}
                    />
                  </div>
                  <p class="mt-1 text-[11px] text-neutral-500 dark:text-neutral-400">
                    {backfill()!.phase} — {backfill()!.done}/{backfill()!.total}
                    <Show when={backfill()!.concurrency > 0}>
                      {' '}
                      · {backfill()!.concurrency} in flight
                    </Show>
                    <Show when={backfill()!.failed > 0}>
                      {' '}
                      · {backfill()!.failed} failed
                    </Show>
                  </p>
                </div>
              </Show>
              <Show when={backfill()?.lastError}>
                <p
                  class="mt-1 truncate text-[11px] text-red-600 dark:text-red-400"
                  title={backfill()!.lastError ?? ""}
                >
                  last error: {backfill()!.lastError}
                </p>
              </Show>
            </div>

            <div class="mt-4 flex items-center justify-between gap-3">
              <div class="flex items-center gap-3">
                <button
                  onClick={() => void consolidateNow()}
                  disabled={!activeId() || consolidating()}
                  title={
                    activeId()
                      ? "Run the memory consolidation pass over the open conversation now"
                      : "Open a conversation first"
                  }
                  class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                >
                  {consolidating() ? "Consolidating…" : "Consolidate this chat"}
                </button>
                <Show when={reflectionStats() && reflectionStats()!.count > 0}>
                  <span class="text-[11px] text-neutral-400 dark:text-neutral-500">
                    Reflection spend: {formatCost(reflectionStats()!.cost)} across{" "}
                    {reflectionStats()!.count}{" "}
                    {reflectionStats()!.count === 1 ? "run" : "runs"}
                  </span>
                </Show>
              </div>
              <Dialog.CloseButton class="rounded-md border border-neutral-300 px-4 py-1.5 text-sm text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800">
                Close
              </Dialog.CloseButton>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog>

      {/* Reasoning trace popup */}
      <Show when={openThinking()}>
        <div
          class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-6"
          onClick={() => setThinkingOpenId(null)}
        >
          <div
            class="flex max-h-[75vh] w-full max-w-2xl flex-col rounded-xl border border-neutral-200 bg-white shadow-xl dark:border-neutral-700 dark:bg-neutral-900"
            onClick={(e) => e.stopPropagation()}
            role="dialog"
            aria-label="Reasoning trace"
          >
            <div class="flex items-center justify-between border-b border-neutral-200 px-5 py-3 dark:border-neutral-800">
              <p class="text-sm font-medium">
                Reasoning trace
                <Show when={thinkingLive()}>
                  <span class="ml-2 text-xs text-neutral-400 dark:text-neutral-500">
                    (live)
                  </span>
                </Show>
              </p>
              <button
                onClick={() => setThinkingOpenId(null)}
                class="rounded-md px-2 py-1 text-xs text-neutral-500 transition hover:bg-neutral-100 dark:text-neutral-400 dark:hover:bg-neutral-800"
                title="Close"
              >
                ✕
              </button>
            </div>
            <div class="overflow-y-auto px-5 py-4">
              <pre class="whitespace-pre-wrap break-words text-sm leading-relaxed text-neutral-700 dark:text-neutral-300">
                {openThinking()?.thinking}
              </pre>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}