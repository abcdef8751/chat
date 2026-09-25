import { createEffect, createSignal, For, Index, onCleanup, onMount, Show } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import { Channel } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  approveTool,
  createConversation,
  deleteConversation,
  deleteMemoryFile,
  denyTool,
  getConfig,
  getPricing,
  hasApiKey,
  listConversations,
  listMemoryFiles,
  listMessages,
  listModels,
  readAttachments,
  renameConversation,
  searchConversations,
  setApiKey,
  setConfig,
  stopChat,
  streamChat,
  thinkingOptions,
  writeMemoryFile,
  type Attachment,
  type Conversation,
  type MemoryFile,
  type Message,
  type ModelInfo,
  type ModelOverride,
  type Pricing,
  type StreamEvent,
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

function parseToolResult(m: Message): { name: string; output: string; error: boolean } | null {
  if (m.role !== "tool") return null;
  try {
    const v = JSON.parse(m.content);
    if (v && typeof v === "object" && "tool_call_id" in v && "output" in v) {
      return { name: String(v.name ?? "tool"), output: String(v.output), error: Boolean(v.error) };
    }
  } catch {
    // fall through
  }
  return { name: "tool", output: m.content, error: false };
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

function parseToolCalls(m: Message): { name: string; arguments: string }[] | null {
  if (m.role !== "assistant") return null;
  try {
    const v = JSON.parse(m.content);
    if (v && typeof v === "object" && Array.isArray(v.tool_calls)) {
      return v.tool_calls.map((c: any) => ({
        name: String(c?.name ?? ""),
        arguments: formatToolArgs(c?.arguments),
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
  const [modelsLoading, setModelsLoading] = createSignal(false);
  const [settingsError, setSettingsError] = createSignal<string | null>(null);
  const [keyDraft, setKeyDraft] = createSignal("");
  const [hasKey, setHasKey] = createSignal(false);

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

  // Model pricing/context: resolved rates (models.dev cache + bundled table +
  // user overrides) for the selected model, plus a per-model cache so turns
  // generated on earlier models can be priced with their own rates.
  const [modelOverrides, setModelOverrides] = createSignal<Record<string, ModelOverride>>({});
  const [pricing, setPricing] = createSignal<Pricing | null>(null);
  const [pricingByModel, setPricingByModel] = createSignal<Record<string, Pricing>>({});
  // Bumped after a catalog refresh so pricing/thinking re-resolve.
  const [catalogVersion, setCatalogVersion] = createSignal(0);

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
    setModelOverrides(cfg.modelOverrides ?? {});
    setHasKey(await hasApiKey());
    // Fast path: cached catalog/prices so the picker and meter render at once.
    // Without a key the picker falls back to the configured model.
    await loadModels(false).catch(() => {});
    // Background: pull fresh data from models.dev and replace values if they
    // changed, without blocking startup.
    loadModels(true).catch(() => {});
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
    setModelOverrides(cfg.modelOverrides ?? {});
    setHasKey(await hasApiKey());
    setKeyDraft("");
    setSettingsError(null);
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
      modelOverrides: current.modelOverrides ?? {},
    });
    let keyChanged = false;
    if (keyDraft().trim()) {
      await setApiKey(keyDraft().trim());
      setHasKey(true);
      setKeyDraft("");
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
      if (changed) loadModels(true).catch(() => {});
    } catch (e) {
      setSettingsError(String(e));
    }
  }

  // Pull the model catalog (models.dev when the endpoint matches, else the
  // endpoint's own `/models`). Prices ride along in the same cache. Cached
  // values render first; a forced refresh replaces them once it resolves.
  async function loadModels(refresh: boolean) {
    setModelsLoading(true);
    try {
      const rows = await listModels(refresh);
      setModels(rows);
      setCatalogVersion((v) => v + 1);
    } finally {
      setModelsLoading(false);
    }
  }

  async function changeModel(id: string) {
    if (!id) return;
    setModel(id);
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
        modelOverrides: modelOverrides(),
      });
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
        modelOverrides: modelOverrides(),
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

  // Reload the file list, keeping (or choosing) a selection.
  async function reloadMemory(prefer?: string) {
    try {
      const files = await listMemoryFiles();
      setMemoryFiles(files);
      const wanted =
        files.find((f) => f.name === (prefer ?? memorySelected())) ??
        files.find((f) => !f.generated) ??
        files[0];
      if (wanted) selectMemoryFile(wanted);
    } catch (e) {
      setMemoryError(String(e));
    }
  }

  async function openMemory() {
    setMemoryError(null);
    setMemoryOpen(true);
    await reloadMemory();
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

  async function addAttachmentPaths(paths: string[]) {
    if (paths.length === 0) return;
    try {
      const atts = await readAttachments(paths);
      if (atts.length > 0) setPendingAttachments((prev) => [...prev, ...atts]);
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

  const activeTitle = () => {
    const id = activeId();
    return conversations().find((c) => c.id === id)?.title ?? "New chat";
  };

  // The active model is always the first option, so the picker can never lose
  // its selection when the catalog is replaced. Option values are plain
  // strings (not reactive) to avoid a select/value update race.
  const modelOptions = (): ModelInfo[] => {
    const id = model();
    const rest = models().filter((m) => m.id !== id);
    const current = models().find((m) => m.id === id);
    return [{ id, name: current?.name ?? id }, ...rest];
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
      {/* Sidebar */}
      <aside class="flex w-72 shrink-0 flex-col border-r border-neutral-200 bg-neutral-50 dark:border-neutral-800 dark:bg-neutral-900">
        <div class="flex items-center justify-between gap-2 border-b border-neutral-200 p-3 dark:border-neutral-800">
          <span class="pl-1 text-sm font-semibold tracking-tight">Pi Chat</span>

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
        </div>

        <div class="border-b border-neutral-200 px-3 py-2 dark:border-neutral-800">
          <input
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
        <header class="flex items-center justify-between gap-3 border-b border-neutral-200 px-6 py-3 dark:border-neutral-800">
          <h1 class="truncate text-sm font-semibold">{activeTitle()}</h1>
          <div class="flex shrink-0 items-center gap-3">
            <Show when={activeId()}>
              <div
                class="hidden items-center gap-1.5 rounded-md px-2 py-1 text-[11px] text-neutral-500 sm:flex dark:text-neutral-400"
                title={`Context: ${Math.min(contextTokens(), contextWindow()).toLocaleString()} / ${contextWindow().toLocaleString()} tokens\nInput / output: ${
                  pricing()?.inputPerMillion ?? "—"
                } / ${pricing()?.outputPerMillion ?? "—"} USD per 1M\nCache read / write: ${
                  pricing()?.cacheReadPerMillion ?? "—"
                } / ${pricing()?.cacheWritePerMillion ?? "—"} USD per 1M${
                  pricing()?.overridden ? "\n(using your overrides)" : ""
                }`}
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
              class="rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              {dark() ? "Light" : "Dark"}
            </button>
            <select
              class="max-w-56 truncate rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 disabled:opacity-50 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
              title="Model"
              aria-busy={modelsLoading()}
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
            <Show
              when={thinkingOpts()?.supportsReasoning && thinkingOpts()!.options.length > 0}
            >
              <select
                class="rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
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
              class="rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              Memory
            </button>
            <button
              onClick={openSettings}
              class="rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              Settings
            </button>
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
              <Index each={messages()}>
                {(m) => {
                  // Hide persisted whitespace-only assistant rows (old turns),
                  // but keep the live streaming bubble.
                  const hidden = () =>
                    m().role === "assistant" &&
                    !m().content.trim() &&
                    !m().thinking?.trim() &&
                    !(streaming() && m().id.startsWith("tmp-assistant-"));
                  const toolResult = () => parseToolResult(m());
                  const toolCalls = () => (toolResult() ? null : parseToolCalls(m()));
                  const atts = () => parseAttachments(m());
                  return (
                    <Show when={!hidden()}>
                      <Show when={toolResult()} keyed>
                        {(tr) => (
                          <div class="flex justify-start">
                            <details class="max-w-[80%] rounded-xl border border-neutral-200 bg-white px-4 py-2 text-sm dark:border-neutral-800 dark:bg-neutral-900">
                              <summary class="cursor-pointer text-[11px] text-neutral-500 dark:text-neutral-400">
                                tool · {tr.name} {tr.error ? "· error" : ""}
                              </summary>
                              <pre class="mt-2 max-h-60 overflow-auto whitespace-pre-wrap break-words text-xs text-neutral-700 dark:text-neutral-300">
                                {tr.output}
                              </pre>
                            </details>
                          </div>
                        )}
                      </Show>
                      <Show when={toolCalls()} keyed>
                        {(calls) => (
                          <For each={calls}>
                            {(c) => (
                              <div class="flex justify-start">
                                <div class="max-w-[80%] rounded-xl border border-neutral-200 bg-white px-4 py-2 text-xs text-neutral-500 dark:border-neutral-800 dark:bg-neutral-900 dark:text-neutral-400">
                                  <div>
                                    assistant wants{" "}
                                    <span class="font-medium text-neutral-700 dark:text-neutral-200">
                                      {c.name}
                                    </span>
                                  </div>
                                  <pre class="mt-1 whitespace-pre-wrap break-words text-neutral-500 dark:text-neutral-400">
                                    {c.arguments}
                                  </pre>
                                </div>
                              </div>
                            )}
                          </For>
                        )}
                      </Show>
                      <Show when={!toolResult() && !toolCalls()}>
                        <div
                          class={`flex ${m().role === "user" ? "justify-end" : "justify-start"}`}
                        >
                          <div class="max-w-[80%]">
                            <Show when={m().role === "assistant" && m().thinking?.trim()}>
                              <button
                                onClick={() =>
                                  setThinkingOpenId(
                                    thinkingOpenId() === m().id ? null : m().id,
                                  )
                                }
                                title="Show the reasoning trace"
                                class={`mb-1 flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
                                  thinkingOpenId() === m().id
                                    ? "bg-neutral-200 text-neutral-700 dark:bg-neutral-700 dark:text-neutral-100"
                                    : "text-neutral-500 hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
                                }`}
                              >
                                <span
                                  class={`inline-block h-1.5 w-1.5 rounded-full ${
                                    m().role === "assistant" &&
                                    streaming() &&
                                    m().content === ""
                                      ? "animate-pulse bg-amber-500"
                                      : "bg-neutral-400 dark:bg-neutral-500"
                                  }`}
                                />
                                {m().role === "assistant" &&
                                streaming() &&
                                m().content === ""
                                  ? "Thinking…"
                                  : "Thought"}
                              </button>
                            </Show>
                            <div
                              class={`rounded-xl px-4 py-2.5 text-sm ${
                                m().role === "assistant"
                                  ? "bg-neutral-100 dark:bg-neutral-800"
                                  : "bg-neutral-900 text-white dark:bg-white dark:text-neutral-900"
                              }`}
                            >
                              <div class="mb-0.5 text-[11px] opacity-60">
                                {m().role === "user" ? "you" : "assistant"}
                                {m().role === "assistant" &&
                                  m().stop_reason === "aborted" &&
                                  " (aborted)"}
                                {m().role === "assistant" &&
                                  m().stop_reason === "error" &&
                                  " (error)"}
                              </div>
                              <Show when={atts().length > 0}>
                                <div class="mb-2 flex flex-wrap items-end gap-2">
                                  <For each={atts()}>
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
                              <Show
                                when={m().role === "assistant" && m().content}
                                fallback={
                                  <div class="whitespace-pre-wrap break-words">
                                    {m().content}
                                  </div>
                                }
                              >
                                <Markdown text={m().content} />
                              </Show>
                              {m().role === "assistant" &&
                                streaming() &&
                                m().id.startsWith("tmp-assistant-") && (
                                  <span class="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-neutral-400 align-text-bottom" />
                                )}
                            </div>
                          </div>
                        </div>
                      </Show>
                    </Show>
                  );
                }}
              </Index>

              <For each={liveTools()}>
                {(t) => (
                  <div class="flex justify-start">
                    <div class="max-w-[80%] rounded-xl border border-neutral-300 bg-white px-4 py-2.5 text-sm shadow-sm dark:border-neutral-700 dark:bg-neutral-900">
                      <div class="mb-1 text-[11px] text-neutral-500 dark:text-neutral-400">
                        {t.state === "pending" && "approval needed · "}
                        {t.state === "running" && "running · "}
                        {t.state === "done" && (t.ok ? "done · " : "error · ")}
                        <span class="font-medium text-neutral-700 dark:text-neutral-200">
                          {t.name}
                        </span>
                      </div>
                      <pre class="max-h-40 overflow-auto whitespace-pre-wrap break-words text-xs text-neutral-600 dark:text-neutral-400">
                        {t.arguments}
                      </pre>
                      <Show when={t.state === "pending"}>
                        <div class="mt-2 flex gap-2">
                          <button
                            class="rounded-md bg-neutral-900 px-3 py-1 text-xs font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                            onClick={() => {
                              setLiveTools((prev) =>
                                prev.map((x) =>
                                  x.callId === t.callId ? { ...x, state: "running" } : x,
                                ),
                              );
                              approveTool(t.callId).catch(() => {});
                            }}
                          >
                            Approve
                          </button>
                          <button
                            class="rounded-md border border-neutral-300 px-3 py-1 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                            onClick={() => {
                              setLiveTools((prev) =>
                                prev.map((x) =>
                                  x.callId === t.callId ? { ...x, state: "running" } : x,
                                ),
                              );
                              denyTool(t.callId).catch(() => {});
                            }}
                          >
                            Deny
                          </button>
                        </div>
                      </Show>
                      <Show when={t.state === "done" && t.output}>
                        <pre class="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words text-xs text-neutral-600 dark:text-neutral-400">
                          {t.output}
                        </pre>
                      </Show>
                    </div>
                  </div>
                )}
              </For>

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
          <Dialog.Content class="fixed left-1/2 top-1/2 z-50 max-h-[85vh] w-full max-w-md -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900">
            <Dialog.Title class="text-base font-semibold">Settings</Dialog.Title>
            <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
              OpenAI-compatible endpoint. Fireworks is prefilled.
            </Dialog.Description>

            <div class="mt-4 space-y-3">
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

              <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                User preferences
                <textarea
                  class="mt-1 min-h-20 w-full resize-y rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                  placeholder="e.g. Prefer concise answers. Call me Sam. Always show units."
                  value={preferences()}
                  onInput={(e) => setPreferences(e.currentTarget.value)}
                />
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Injected at the top of the system prompt for every conversation.
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
          <Dialog.Content class="fixed left-1/2 top-1/2 z-50 flex h-[70vh] w-full max-w-3xl -translate-x-1/2 -translate-y-1/2 flex-col rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900">
            <Dialog.Title class="text-base font-semibold">Memory</Dialog.Title>
            <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
              Long-term memory is stored as Markdown files in the app data dir. index.md is
              generated automatically and is read-only.
            </Dialog.Description>

            <div class="mt-4 flex min-h-0 flex-1 gap-4">
              <div class="flex w-44 shrink-0 flex-col gap-1 overflow-y-auto">
                <For each={memoryFiles()}>
                  {(f) => (
                    <button
                      onClick={() => selectMemoryFile(f)}
                      class={`truncate rounded-md px-2 py-1.5 text-left text-xs transition ${
                        memorySelected() === f.name
                          ? "bg-neutral-200 font-medium dark:bg-neutral-800"
                          : "text-neutral-600 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      }`}
                    >
                      {f.name}
                      {f.generated ? " · auto" : ""}
                    </button>
                  )}
                </For>
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
                  <textarea
                    class="min-h-0 flex-1 resize-none rounded-lg border border-neutral-300 bg-white p-3 font-mono text-xs outline-none focus:border-neutral-500 disabled:opacity-70 dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
                    spellcheck={false}
                    readonly={selectedMemoryFile()?.generated}
                    value={memoryDraft()}
                    onInput={(e) => setMemoryDraft(e.currentTarget.value)}
                  />
                  <div class="mt-3 flex items-center justify-between gap-3">
                    <Show
                      when={!selectedMemoryFile()?.generated}
                      fallback={
                        <span class="text-[11px] text-neutral-400 dark:text-neutral-500">
                          Generated from the memory entries above — read only.
                        </span>
                      }
                    >
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
                    </Show>

                    <div class="flex items-center gap-2">
                      <Show when={memoryError()}>
                        <span class="max-w-56 truncate text-[11px] text-red-600 dark:text-red-400">
                          {memoryError()}
                        </span>
                      </Show>
                      <button
                        onClick={() => void saveSelectedMemory()}
                        disabled={selectedMemoryFile()?.generated}
                        class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:cursor-not-allowed disabled:opacity-40 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                      >
                        Save
                      </button>
                    </div>
                  </div>
                </Show>
              </div>
            </div>

            <div class="mt-4 flex justify-end">
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