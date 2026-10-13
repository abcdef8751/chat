import { createEffect, createMemo, createSignal, onCleanup, onMount, Show } from "solid-js";
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
  editUserMessage,
  getConfig,
  getPricing,
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
  setBraveKey,
  setConfig,
  setConversationModel,
  setConversationProvider,
  setModelOverrides as saveModelOverrides,
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
import { isAndroid, onBack, readAttachmentFromUri } from "./lib/platform";
import { acceptsImages, resolveInputModalities } from "./lib/capabilities";
import { Composer } from "./components/Composer";
import { Header } from "./components/Header";
import { MemoryPage } from "./components/MemoryPage";
import { MessageList } from "./components/MessageList";
import { SettingsDialog } from "./components/SettingsDialog";
import { Sidebar } from "./components/Sidebar";
import {
  costOfUsage,
  formatCost,
  formatToolArgs,
  groupTurns,
  parseAttachments,
  parseUsage,
  type LiveTool,
} from "./lib/turns";

const THEME_KEY = "pi-chat-theme";

// Extensions offered by both the composer and the inline editor's file picker.
const ATTACHMENT_FILTERS = [
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
];

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

  // Inline edit of a user message (Item 13). The edited row lives in the DB;
  // saving rewrites it, drops later messages, and re-streams a fresh reply.
  const [editingId, setEditingId] = createSignal<string | null>(null);
  const [editDraft, setEditDraft] = createSignal("");
  const [editAttachments, setEditAttachments] = createSignal<Attachment[]>([]);

  // A monotonic token so a superseded turn's late channel events (after Stop /
  // a re-send) are ignored.
  let turnToken = 0;

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
  let hamburgerEl: HTMLButtonElement | undefined;

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
  // "YOLO mode": auto-approve gated tool calls (Settings toggle; backend gate).
  const [autoApproveTools, setAutoApproveTools] = createSignal(false);
  const [thinkingOpts, setThinkingOpts] = createSignal<ThinkingOptions | null>(null);
  const [models, setModels] = createSignal<ModelInfo[]>([]);
  const [modelsLoading, setModelsLoading] = createSignal(false);
  const [settingsError, setSettingsError] = createSignal<string | null>(null);
  const [braveKeyDraft, setBraveKeyDraft] = createSignal("");
  const [hasBraveKeySaved, setHasBraveKeySaved] = createSignal(false);
  const [reflectionEnabled, setReflectionEnabled] = createSignal(true);
  const [reflectionIdleMinutes, setReflectionIdleMinutes] = createSignal(30);
  const [shellWorkspaceDir, setShellWorkspaceDir] = createSignal("");
  const [reflectionStats, setReflectionStats] = createSignal<ReflectionStats | null>(null);
  const [backfill, setBackfill] = createSignal<BackfillStatus | null>(null);
  const [extractionStats, setExtractionStats] = createSignal<ExtractionStats | null>(null);
  const [importReport, setImportReport] = createSignal<ImportReport | null>(null);
  // Backfill overrides: empty model/thinking = the active provider's default;
  // `backfillThinkingOpts` mirrors the header select's reasoning options but for
  // the chosen backfill model (or the active default when none is chosen).
  const [backfillModel, setBackfillModel] = createSignal("");
  const [backfillThinking, setBackfillThinking] = createSignal("");
  const [backfillThinkingOpts, setBackfillThinkingOpts] = createSignal<ThinkingOptions | null>(
    null,
  );
  // Models offered in the backfill picker, resolved against the ACTIVE provider
  // (the endpoint the map/reduce phases hit). Using the header/chat provider's
  // list here can pitch a model at the wrong endpoint → HTTP 400 "model not
  // found" during backfill.
  const [backfillModels, setBackfillModels] = createSignal<ModelInfo[]>([]);
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

  // Android system-bar insets (px), exposed by the native side via
  // `window.PiInsets` + the `pi-insets` event (see MainActivity.kt). Absent on
  // desktop, where these stay 0. The app reserves the space itself in its flex
  // layout so content never hides under the status / navigation bars.
  const [insets, setInsets] = createSignal({ top: 0, left: 0, right: 0, bottom: 0 });
  createEffect(() => {
    const w = window as unknown as {
      PiInsets?: {
        getTop(): number;
        getBottom(): number;
        getLeft(): number;
        getRight(): number;
      };
    };
    const apply = () => {
      if (w.PiInsets) {
        setInsets({
          top: w.PiInsets.getTop(),
          bottom: w.PiInsets.getBottom(),
          left: w.PiInsets.getLeft(),
          right: w.PiInsets.getRight(),
        });
      }
    };
    apply();
    const onInsets = (e: Event) => {
      const d = (e as CustomEvent<{ top?: number; bottom?: number; left?: number; right?: number }>).detail;
      if (d) {
        setInsets((p) => ({
          top: d.top ?? p.top,
          bottom: d.bottom ?? p.bottom,
          left: d.left ?? p.left,
          right: d.right ?? p.right,
        }));
      }
    };
    window.addEventListener("pi-insets", onInsets);
    window.addEventListener("resize", apply);
    onCleanup(() => {
      window.removeEventListener("pi-insets", onInsets);
      window.removeEventListener("resize", apply);
    });
  });
  // Full-screen (mobile) dialogs render in their own `fixed inset-0` viewport,
  // outside the padded app root, so they must apply the same system-bar insets
  // to their padding themselves. On desktop `insets()` are all 0, making this
  // exactly the base `p-5` (1.25rem) the classes already use.
  const fullscreenInsetStyle = () =>
    ({
      "padding-top": `calc(1.25rem + ${insets().top}px)`,
      "padding-bottom": `calc(1.25rem + ${insets().bottom}px)`,
      "padding-left": `calc(1.25rem + ${insets().left}px)`,
      "padding-right": `calc(1.25rem + ${insets().right}px)`,
    }) as const;


  // Memory tab: reads/edits the Markdown memory files.
  const [memoryOpen, setMemoryOpen] = createSignal(false);
  // Narrow-viewport memory detail page (the full-screen editor opened from the
  // memory list). Owned here so the Android hardware-back handler can pop the
  // detail page before closing the dialog. See MemoryPage.
  const [memoryDetailOpen, setMemoryDetailOpen] = createSignal(false);
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
  // Per-model override editor (Settings). `overrideModel` is the model being
  // edited; `overrideEffective` is what the app currently resolves for it, so
  // the user can see the value their override replaces.
  const [overrideModel, setOverrideModel] = createSignal("");
  const [overrideEffective, setOverrideEffective] = createSignal<Pricing | null>(null);
  let overrideReqSeq = 0;

  // A blank override: everything unset, so resolution falls through to the
  // endpoint's own values and then the bundled defaults.
  const EMPTY_OVERRIDE: ModelOverride = {
    contextWindow: null,
    inputPerMillion: null,
    outputPerMillion: null,
    cacheReadPerMillion: null,
    cacheWritePerMillion: null,
    inputModalities: null,
    outputModalities: null,
    reasoning: null,
  };

  const overrideFor = (id: string): ModelOverride => modelOverrides()[id] ?? EMPTY_OVERRIDE;

  // Persist the override map immediately through the dedicated command (so it
  // never clobbers other unsaved Settings edits with on-disk values). Bump
  // `catalogVersion` only *after* the write lands, so the header meter and the
  // "Currently" preview re-resolve against the new value rather than the old.
  function commitOverrides(next: Record<string, ModelOverride>) {
    setModelOverrides(next);
    void saveModelOverrides(next)
      .then(() => setCatalogVersion((v) => v + 1))
      .catch((e) => setSettingsError(String(e)));
  }

  // Merge one field into a model's override. When every field is blank the
  // entry is dropped entirely, so a cleared override stops shadowing the
  // endpoint/bundled values.
  function setOverrideField<K extends keyof ModelOverride>(
    id: string,
    field: K,
    value: ModelOverride[K],
  ) {
    if (!id) return;
    const next = { ...modelOverrides() };
    const cur: ModelOverride = { ...(next[id] ?? EMPTY_OVERRIDE), [field]: value };
    // Drop the entry once nothing is set, so a cleared override stops shadowing
    // the endpoint/bundled/catalog values. An empty modality list counts as unset.
    const blank = Object.values(cur).every(
      (v) => v == null || (Array.isArray(v) && v.length === 0),
    );
    if (blank) delete next[id];
    else next[id] = cur;
    commitOverrides(next);
  }

  function clearOverride(id: string) {
    if (!id) return;
    const next = { ...modelOverrides() };
    delete next[id];
    commitOverrides(next);
  }

  // Parse a numeric input; blank → null (fall through), otherwise a finite number.
  const numFromInput = (raw: string): number | null => {
    const t = raw.trim();
    if (!t) return null;
    const n = Number(t);
    return Number.isFinite(n) ? n : null;
  };

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

    // Android hardware/gesture back: pop exactly one layer per press, in
    // priority order (memory detail → memory dialog → settings → new chat →
    // reasoning popup → sidebar drawer → inline edit). Returning `false` when
    // nothing is open lets the native side finish the activity, so back exits
    // only on the chat page. Each close goes through the same setter/handler as
    // its visible control (e.g. `closeSettings` saves, the edit branch cancels
    // the inline editor). Inert on desktop, where no `pi-back` event is ever
    // dispatched.
    const offBack = onBack(() => {
      if (memoryOpen() && memoryDetailOpen()) {
        setMemoryDetailOpen(false);
        return true;
      }
      if (memoryOpen()) {
        setMemoryOpen(false);
        return true;
      }
      if (settingsOpen()) {
        void closeSettings();
        return true;
      }
      if (newOpen()) {
        setNewOpen(false);
        return true;
      }
      if (openThinking()) {
        setThinkingOpenId(null);
        return true;
      }
      if (sidebarOpen()) {
        setSidebarOpen(false);
        hamburgerEl?.focus();
        return true;
      }
      if (editingId()) {
        cancelEditMessage();
        return true;
      }
      return false;
    });
    onCleanup(offBack);

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

    // Background/manual sync pull finished and applied remote rows. Refresh the
    // conversation list and the open conversation's messages only when data
    // actually changed, so the 60s idle tick doesn't re-scroll an idle chat.
    const unlistenSync = await listen<{ changed: boolean }>("sync-changed", (event) => {
      if (!event.payload?.changed) return;
      refreshConversations().catch(() => {});
      const id = activeId();
      if (id && !streaming()) listMessages(id).then(setMessages);
    });
    onCleanup(() => unlistenSync);

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
    setAutoApproveTools(cfg.autoApproveTools ?? false);
    setReflectionEnabled(cfg.memoryReflectionEnabled ?? true);
    setReflectionIdleMinutes(cfg.memoryReflectionIdleMinutes ?? 30);
    setShellWorkspaceDir(cfg.shellWorkspaceDir ?? "");
    setSyncEnabled(cfg.syncEnabled ?? false);
    setModelOverrides(cfg.modelOverrides ?? {});
    setActiveProviderId(cfg.activeProviderId ?? "");
    void refreshProviders();
    void refreshReflectionStats();    // Fast path: cached catalog/prices so the picker and meter render at once.
    // Without a key the picker falls back to the configured model. The list is
    // scoped to the active/header provider after refreshProviders() resolves;
    // load the legacy default immediately so the picker renders.
    await loadModels(false, cfg.activeProviderId ?? "").catch(() => {});
    // Background: pull fresh data from models.dev and replace values if they
    // changed, without blocking startup.
    loadModels(true, cfg.activeProviderId ?? "").catch(() => {});
  });

  // Re-resolve pricing whenever the selected model, its provider, or the catalog
  // changes. Keying on the header provider too is essential: the model's fetched
  // context/prices live under *that* provider's cache, and resolving against the
  // active provider instead would silently fall back to a bundled family guess.
  createEffect(() => {
    const id = model();
    const prov = headerProviderId();
    catalogVersion();
    if (!id) {
      setPricing(null);
      return;
    }
    const seq = ++pricingReqSeq;
    getPricing(id, prov)
      .then((p) => {
        // A newer model switch superseded this one — drop the stale result so
        // the context window never shows a previous model's window.
        if (seq !== pricingReqSeq) return;
        setPricing(p);
      })
      .catch(() => {
        if (seq === pricingReqSeq) setPricing(null);
      });
  });

  // Re-resolve the thinking levels whenever the selected model, its provider, or
  // the catalog changes.
  createEffect(() => {
    const id = model();
    const prov = headerProviderId();
    catalogVersion();
    if (!id) {
      setThinkingOpts(null);
      return;
    }
    const seq = ++thinkingReqSeq;
    thinkingOptions(id, prov)
      .then((opts) => {
        if (seq !== thinkingReqSeq) return;
        setThinkingOpts(opts);
      })
      .catch(() => {
        if (seq === thinkingReqSeq) setThinkingOpts(null);
      });
  });

  // Resolve what the app currently uses for the model being edited in Settings,
  // so the override editor can show the value an override would replace. Re-runs
  // on `catalogVersion` so edits (which bump it) reflect immediately.
  createEffect(() => {
    const id = overrideModel();
    const prov = headerProviderId();
    catalogVersion();
    if (!id) {
      setOverrideEffective(null);
      return;
    }
    const seq = ++overrideReqSeq;
    getPricing(id, prov)
      .then((p) => {
        if (seq === overrideReqSeq) setOverrideEffective(p);
      })
      .catch(() => {
        if (seq === overrideReqSeq) setOverrideEffective(null);
      });
  });

  // Price past turns with the model that actually produced them. New turns
  // store a frozen `cost`; only legacy turns without one need a lookup. A row's
  // `provider` is its base URL, so map it back to a provider id (when known) to
  // resolve the correct cache instead of the active provider's.
  createEffect(() => {
    const byModel = pricingByModel();
    const current = model();
    const norm = (u?: string | null) => (u ?? "").trim().replace(/\/+$/, "");
    const provs = providers();
    for (const m of messages()) {
      if (m.role !== "assistant" || !m.model) continue;
      const u = parseUsage(m);
      if (u && typeof u.cost === "number") continue;
      if (m.model === current || byModel[m.model]) continue;
      const pid = provs.find((p) => norm(p.baseUrl) === norm(m.provider))?.id;
      getPricing(m.model, pid)
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
    // Editing a message is scoped to the conversation it lives in; a switch
    // would otherwise let Save target the wrong conversation.
    setEditingId(null);
    setEditDraft("");
    setEditAttachments([]);
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
    // Open the dialog first: none of the async setup below may block it. Some
    // of those round-trips can fail (e.g. the OS keychain is unavailable on
    // Android), and previously the awaited `hasApiKey()` rejecting early meant
    // `setSettingsOpen(true)` never ran, so Settings silently failed to open.
    // Mirror `openMemory()`, which opens its dialog before loading anything.
    setAddOpen(false);
    setEditId(null);
    setProviderError(null);
    setSettingsError(null);
    setSyncError(null);
    setBraveKeyDraft("");
    setSettingsOpen(true);

    try {
      const cfg = await getConfig();
      setBaseUrl(cfg.baseUrl);
      setModel(cfg.model);
      setPreferences(cfg.preferences ?? "");
      setThinkingLevel(cfg.thinkingLevel ?? "");
      setEchoReasoning(cfg.echoReasoningContent ?? true);
      setAutoApproveTools(cfg.autoApproveTools ?? false);
      setReflectionEnabled(cfg.memoryReflectionEnabled ?? true);
      setReflectionIdleMinutes(cfg.memoryReflectionIdleMinutes ?? 30);
      setShellWorkspaceDir(cfg.shellWorkspaceDir ?? "");
      setSyncEnabled(cfg.syncEnabled ?? false);
      setModelOverrides(cfg.modelOverrides ?? {});
      setOverrideModel(model());
      setActiveProviderId(cfg.activeProviderId ?? "");
    } catch (e) {
      // Never block opening Settings; surface the failure as a banner instead.
      setSettingsError(String(e));
    }
    void refreshProviders();
    hasBraveKey()
      .then(setHasBraveKeySaved)
      .catch(() => setHasBraveKeySaved(false)); // never block opening Settings
    void refreshSync();
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
      autoApproveTools: autoApproveTools(),
      memoryReflectionEnabled: reflectionEnabled(),
      memoryReflectionIdleMinutes: reflectionIdleMinutes(),
      modelOverrides: modelOverrides(),
      shellWorkspaceDir: shellWorkspaceDir(),
      syncEnabled: syncEnabled(),
      providers: providerConfig(),
      activeProviderId: activeProviderId(),
    });
    let keyChanged = false;
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

  // Push knows how much is left (pending → total). Pull knows the remote row
  // count only when the backend could read it from the server (`pullTotal` >
  // 0), so the bar is real progress then and falls back to full + pulsing when
  // the count is unknown.
  function syncBarWidth(d: SyncStatus | null): number {
    if (!d) return 100;
    if (d.phase === "push" && d.pushed + d.pending > 0)
      return Math.max(6, Math.min(100, Math.round((d.pushed / (d.pushed + d.pending)) * 100)));
    if (d.phase === "pull" && (d.pullTotal ?? 0) > 0)
      return Math.max(6, Math.min(100, Math.round(((d.pulled ?? 0) / d.pullTotal) * 100)));
    return 100;
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
  // Sequencing guards for the pricing and thinking-options effects. These call
  // `getPricing` / `thinkingOptions` asynchronously; without a guard a slow
  // response for a *previous* model can resolve after a fast response for the
  // *current* one and overwrite it, so the context-window/pricing readouts (and
  // thinking levels) would keep reflecting the earlier model. Only the request
  // issued last is allowed to apply its result — same pattern as `loadModels`.
  let pricingReqSeq = 0;
  let thinkingReqSeq = 0;
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
    // Adopt the new model's thinking levels; drop a level it doesn't accept.
    const opts = await thinkingOptions(id, headerProviderId()).catch(() => null);
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
        autoApproveTools: autoApproveTools(),
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
        autoApproveTools: autoApproveTools(),
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
  // immediately; progress arrives on the `memory-backfill` event. An optional
  // options object carries the chosen backfill model / thinking level and the
  // "all history" flag.
  async function startBackfill(opts?: {
    model?: string | null;
    thinkingLevel?: string | null;
    allHistory?: boolean;
    force?: boolean;
  }) {
    setMemoryError(null);
    try {
      setBackfill(await backfillMemories(opts ?? {}));
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

  // Resolve the backfill thinking-level options for the chosen backfill model.
  // Empty `backfillModel` means "active provider default", so fall back to the
  // header-selected model. Mirrors the header select's reasoning support gating.
  let backfillThinkingReqSeq = 0;
  createEffect(() => {
    const id = backfillModel() === "" ? model() : backfillModel();
    const prov = headerProviderId();
    catalogVersion();
    if (!id) {
      setBackfillThinkingOpts(null);
      return;
    }
    const seq = ++backfillThinkingReqSeq;
    thinkingOptions(id, prov)
      .then((opts) => {
        if (seq !== backfillThinkingReqSeq) return;
        setBackfillThinkingOpts(opts);
      })
      .catch(() => {
        if (seq === backfillThinkingReqSeq) setBackfillThinkingOpts(null);
      });
  });

  // Backfill runs on the ACTIVE provider, so load that provider's model list
  // when the memory dialog opens (or the active provider changes) instead of
  // reusing the header/chat-scoped list. This guarantees any model the user
  // picks is valid against the endpoint the backfill actually hits.
  let backfillModelsReqSeq = 0;
  createEffect(() => {
    const ap = activeProviderId();
    if (!memoryOpen() || !ap) {
      setBackfillModels([]);
      return;
    }
    const seq = ++backfillModelsReqSeq;
    setBackfillModels([]);
    listModels(false, ap)
      .then((rows) => {
        if (seq === backfillModelsReqSeq) setBackfillModels(rows);
      })
      .catch(() => {
        if (seq === backfillModelsReqSeq) setBackfillModels([]);
      });
  });

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
  //
  // Resolved client-side (override → models.dev → text-only) rather than read
  // off `ModelInfo.vision`, because the backend bakes that bit into
  // `list_models`: it goes stale the moment a capability is toggled in Settings
  // (the model list isn't refetched then), so a model the user just marked
  // image-capable kept warning. Returns `null` when the model isn't in the list
  // and has no override, so an unknown model is never warned about.
  const currentModelVision = (): boolean | null => {
    const id = model();
    if (!id) return null;
    const overrideInput = modelOverrides()[id]?.inputModalities;
    const m = models().find((x) => x.id === id);
    if (!m && overrideInput == null) return null;
    return acceptsImages(resolveInputModalities(m?.modalities, m?.attachment, overrideInput));
  };

  // Derived, not event-set: so the warning appears the moment an image is
  // staged and clears on its own when the model is switched or its capability
  // is toggled in Settings.
  const attachmentWarning = createMemo<string | null>(() => {
    const images = pendingAttachments().filter((a) => a.kind === "image").length;
    if (images === 0 || currentModelVision() !== false) return null;
    return `${model()} isn't marked as accepting image input, so ${
      images === 1 ? "this image" : "these images"
    } may be ignored or rejected. Pick a vision model in the header.`;
  });

  // Resolve picked paths into attachments. On Android the dialog plugin returns
  // `content://` URIs, which the path-based `readAttachments` (std::fs) cannot
  // open — so read each through the native ContentResolver bridge instead.
  // Desktop (and any plain filesystem path) keeps the single batched
  // `readAttachments` call unchanged.
  async function readPickedAttachments(paths: string[]): Promise<Attachment[]> {
    if (!isAndroid && !paths.some((p) => p.startsWith("content://"))) {
      return readAttachments(paths);
    }
    const atts: Attachment[] = [];
    for (const p of paths) {
      // One unreadable item must not drop the whole selection: keep the
      // survivors, matching the desktop `readAttachments` per-file behavior.
      try {
        if (isAndroid || p.startsWith("content://")) {
          atts.push(await readAttachmentFromUri(p));
        } else {
          atts.push(...(await readAttachments([p])));
        }
      } catch {
        // Skip this item and continue with the rest.
      }
    }
    return atts;
  }

  async function addAttachmentPaths(paths: string[]) {
    if (paths.length === 0) return;
    try {
      const atts = await readPickedAttachments(paths);
      if (atts.length === 0) {
        // Every item failed (e.g. an unreadable `content://` URI). Surface it
        // instead of silently doing nothing.
        setStreamError(
          paths.length === 1 ? "Could not read the selected file." : "Could not read the selected files.",
        );
        return;
      }
      setPendingAttachments((prev) => [...prev, ...atts]);
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
        filters: ATTACHMENT_FILTERS,
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

  /// Append a temp assistant row and stream a reply for `text`/`staged` into
  /// conversation `id`. Shared by `send()` (which stages a new user row first)
  /// and the inline editor (which rewrites an existing row). `tempUserId` is the
  /// staged user row, if any, so a pre-stream failure can roll it back.
  async function runAssistantTurn(
    id: string,
    text: string,
    staged: Attachment[],
    tempUserId: string | null,
    /// `true` when the user row already exists in the transcript (an edit
    /// re-run): the backend must build the turn without persisting a second
    /// `user` row. Normal sends leave this `false`.
    regenerate = false,
  ) {
    const assistantKey = `tmp-assistant-${tempId++}`;
    const assistantMsg: Message = {
      id: assistantKey,
      conversation_id: id,
      role: "assistant",
      index: messages().length,
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
    setMessages((prev) => [...prev, assistantMsg]);
    setNearBottom(true);
    setStreaming(true);
    setStreamError(null);
    setLiveTools([]);
    setThinkingOpenId(null);

    const token = ++turnToken;

    const received = { current: false };
    const channel = new Channel<StreamEvent>();
    channel.onmessage = (ev) => {
      // Ignore events from a superseded turn (Stop, then a new send).
      if (activeId() !== id || token !== turnToken) return;
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
        // Flush the round's streamed reasoning + preamble text onto this call,
        // then reset the in-flight message so the next round starts clean. The
        // final round emits no `toolCall`, so whatever text remains stays as the
        // answer bubble. `groupTurns` re-emits the flushed preamble before this
        // call, matching the persisted rows after `done`.
        const cur = messages().find((m) => m.id === assistantKey);
        const thinking = cur?.thinking ?? "";
        const preamble = cur?.content ?? "";
        const hasPreamble = thinking.trim().length > 0 || preamble.trim().length > 0;
        if (hasPreamble) {
          setMessages((prev) =>
            prev.map((m) =>
              m.id === assistantKey ? { ...m, content: "", thinking: "" } : m,
            ),
          );
        }
        setLiveTools((prev) => [
          ...prev,
          {
            callId: ev.callId,
            name: ev.name,
            arguments: formatToolArgs(ev.arguments),
            gated: ev.gated,
            state: ev.gated ? "pending" : "running",
            thinking: hasPreamble ? thinking : undefined,
            text: hasPreamble ? preamble : undefined,
          },
        ]);
      } else if (ev.type === "toolResult") {
        received.current = true;
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
      await streamChat(id, text, staged, channel, regenerate);
    } catch (e) {
      if (activeId() !== id || token !== turnToken) return;
      if (!received.current) {
        // Command errored before streaming began (e.g. missing key/match).
        setMessages((prev) =>
          prev.filter(
            (m) => m.id !== assistantKey && (tempUserId === null || m.id !== tempUserId),
          ),
        );
        if (tempUserId !== null) {
          setDraft(text);
          setPendingAttachments(staged);
        }
      }
      setStreaming(false);
      setLiveTools([]);
      setStreamError(String(e));
    }
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

    setMessages((prev) => [...prev, userMsg]);
    setDraft("");
    setPendingAttachments([]);
    await runAssistantTurn(id, text, staged, userMsg.id);
  }

  async function stop() {
    const id = activeId();
    if (!id) return;
    // Close the abort race: keep `streaming` set until `stop_chat` has been
    // delivered. Otherwise the composer is re-enabled while the abort is still
    // in flight and a quick re-send can be cancelled by the late `stop_chat`.
    // The command only raises the backend's cancel flag (it returns
    // immediately), so the composer re-enables as soon as it lands. The
    // stream's own `done` event still reloads the persisted rows (the turn
    // token is deliberately not superseded), so the timeline still reconciles.
    try {
      await stopChat(id);
    } catch {
      // noop — the backend still settles the aborted turn.
    }
    setStreaming(false);
    setLiveTools([]);
    // The user message is persisted by the backend *before* streaming starts, so
    // it is already in the transcript. Handing the draft back here would
    // duplicate it on the next send, so Stop simply returns the composer to its
    // idle state; the message can be edited to re-run the turn.
  }

  // --- Inline editing of user messages (Item 13) --------------------------

  function startEditMessage(m: Message) {
    if (streaming()) return;
    // Only persisted rows are editable; a `tmp-` row hasn't been written yet.
    if (m.id.startsWith("tmp-")) return;
    setEditingId(m.id);
    setEditDraft(m.content);
    setEditAttachments(parseAttachments(m));
  }

  function cancelEditMessage() {
    setEditingId(null);
    setEditDraft("");
    setEditAttachments([]);
  }

  async function addEditAttachmentPaths(paths: string[]) {
    if (paths.length === 0) return;
    try {
      const atts = await readPickedAttachments(paths);
      if (atts.length === 0) return;
      setEditAttachments((prev) => [...prev, ...atts]);
    } catch (e) {
      setStreamError(String(e));
    }
  }

  async function pickEditAttachments() {
    try {
      const picked = await open({
        multiple: true,
        directory: false,
        filters: ATTACHMENT_FILTERS,
      });
      if (!picked) return;
      await addEditAttachmentPaths(Array.isArray(picked) ? picked : [picked]);
    } catch (e) {
      setStreamError(String(e));
    }
  }

  function removeEditAttachment(id: string) {
    setEditAttachments((prev) => prev.filter((a) => a.id !== id));
  }

  async function saveEditMessage() {
    const id = activeId();
    const messageId = editingId();
    if (!id || !messageId || streaming()) return;
    const text = editDraft().trim();
    const atts = editAttachments();
    if (!text && atts.length === 0) return;
    try {
      await editUserMessage(id, messageId, text, atts.length > 0 ? atts : null);
    } catch (e) {
      setStreamError(String(e));
      return;
    }
    cancelEditMessage();
    // The edit rewrote the row and deleted everything after it; reload so the
    // transcript matches, then stream a fresh assistant reply for the edit.
    try {
      const rows = await listMessages(id);
      if (activeId() === id) setMessages(rows);
    } catch {
      // The stream's `done` reload below still reconciles state.
    }
    if (activeId() !== id) return;
    // The edited user row already exists — the backend must not insert a second
    // one, so mark this re-run as a regeneration.
    await runAssistantTurn(id, text, atts, null, true);
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
  // strings (not reactive) to avoid a select/value update race. With no provider
  // configured there are no models, so the selector renders blank.
  const modelOptions = (): ModelInfo[] => {
    if (providers().length === 0) return [];
    const id = model();
    const rest = models().filter((m) => m.id !== id);
    // Keep the *full* catalog entry for the active model — including
    // `modalities`/`attachment`/`reasoning` — so the Settings Capabilities card
    // can render it. Only synthesize a stub when the model isn't in the catalog
    // (no metadata to show). Rebuilding a `{id,name,vision}`-only object here
    // silently dropped the capability fields.
    const current = models().find((m) => m.id === id) ?? { id, name: id, vision: null };
    return [current, ...rest];
  };

  // Resolve the reasoning popup's source from `thinkingOpenId`. A persisted (or
  // current-round) trace lives on a message row; a *flushed* live round's trace
  // lives on its `LiveTool` — its synthetic `live:<callId>:thought` id matches
  // no message, so it must be resolved against `liveTools()`. Returns null when
  // the id is stale/unresolvable, which also serves as the "popup is open"
  // predicate (see the hardware-back handler).
  const openThinking = (): { text: string; live: boolean } | null => {
    const id = thinkingOpenId();
    if (!id) return null;
    if (id.startsWith("live:")) {
      const rest = id.slice("live:".length);
      const callId = rest.endsWith(":thought") ? rest.slice(0, -":thought".length) : rest;
      const t = liveTools().find((x) => x.callId === callId);
      return t?.thinking?.trim() ? { text: t.thinking, live: true } : null;
    }
    const m = messages().find((x) => x.id === id);
    if (!m?.thinking?.trim()) return null;
    const live = Boolean(streaming() && m.id.startsWith("tmp-assistant-") && !m.content);
    return { text: m.thinking, live };
  };

  // Context window + cost readouts for the active conversation. The meter shows
  // the model's actual context window — resolved from a per-model override, the
  // endpoint's `/models`, models.dev, or the bundled table. `pricing()` is null
  // only before it resolves; the resolver already falls back to 200k when the
  // model's window is unknown.
  const contextWindow = () => pricing()?.contextWindow ?? 200_000;

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

  // Background maintenance spend (memory reflection + context compaction), kept
  // separate from conversation cost.
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

  // True once this conversation has had a context compaction: older messages are
  // folded into `compaction_summary`, only the recent tail is sent verbatim.
  const compacted = () => !!activeConversation()?.compaction_through;

  // Non-blocking heads-up when the conversation nears the model's window. The
  // turn that crosses 80% is compacted automatically, so this is informational.
  const contextNote = () => {
    if (!activeId() || contextPct() < 80) return null;
    const pct = Math.round(contextPct());
    return compacted()
      ? `Context ${pct}% — older messages are compacted into a summary; recent turns stay verbatim.`
      : `Context ${pct}% — older messages will be compacted into a summary automatically.`;
  };

  return (
    <div
      class="flex h-screen w-screen overflow-hidden bg-white text-neutral-900 dark:bg-neutral-950 dark:text-neutral-100"
      style={{
        "padding-top": `${insets().top}px`,
        "padding-bottom": `${insets().bottom}px`,
        "padding-left": `${insets().left}px`,
        "padding-right": `${insets().right}px`,
        // Hand horizontal touch drags to JS (for the left-edge drawer swipe)
        // while keeping vertical scrolling native in the chat/message lists.
        "touch-action": "pan-y",
      }}
    >
      <Show when={dragOver()}>
        <div class="pointer-events-none fixed inset-0 z-[60] flex items-center justify-center bg-black/40">
          <div class="rounded-xl border-2 border-dashed border-white/70 px-6 py-4 text-sm font-medium text-white">
            Drop files to attach
          </div>
        </div>
      </Show>

      <Sidebar
        sidebarOpen={sidebarOpen()}
        setSidebarOpen={setSidebarOpen}
        wideViewport={wideViewport()}
        insets={insets()}
        onClosed={() => hamburgerEl?.focus()}
        visibleConversations={visibleConversations()}
        activeId={activeId()}
        setActiveId={setActiveId}
        draftTitle={draftTitle()}
        setDraftTitle={setDraftTitle}
        newOpen={newOpen()}
        setNewOpen={setNewOpen}
        createNewChat={createNewChat}
        searchQuery={searchQuery()}
        setSearchQuery={setSearchQuery}
        renameId={renameId()}
        renameValue={renameValue()}
        setRenameId={setRenameId}
        setRenameValue={setRenameValue}
        commitRename={commitRename}
        confirmDeleteId={confirmDeleteId()}
        setConfirmDeleteId={setConfirmDeleteId}
        removeConversation={removeConversation}
      />

      {/* Main */}
      <main class="flex min-w-0 flex-1 flex-col">
        <Header
          sidebarOpen={sidebarOpen()}
          setSidebarOpen={setSidebarOpen}
          onHamburgerRef={(el) => (hamburgerEl = el)}
          activeTitle={activeTitle()}
          activeId={activeId()}
          contextTokens={contextTokens()}
          contextWindow={contextWindow()}
          pricing={pricing()}
          reflectionLabel={reflectionLabel()}
          compacted={compacted()}
          contextClass={contextClass()}
          costLabel={costLabel()}
          dark={dark()}
          setDark={setDark}
          providers={providers()}
          headerProviderId={headerProviderId()}
          changeHeaderProvider={changeHeaderProvider}
          models={models()}
          modelsLoading={modelsLoading()}
          model={model()}
          changeModel={changeModel}
          modelOptions={modelOptions}
          thinkingOpts={thinkingOpts()}
          thinkingLevel={thinkingLevel()}
          changeThinking={changeThinking}
          openMemory={openMemory}
          openSettings={openSettings}
        />

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
            <MessageList
              turns={turns()}
              empty={messages().length === 0}
              streaming={streaming()}
              thinkingOpenId={thinkingOpenId()}
              onToggleThought={(id) =>
                setThinkingOpenId(thinkingOpenId() === id ? null : id)
              }
              onApprove={approveLiveTool}
              onDeny={denyLiveTool}
              editingId={editingId()}
              editDraft={editDraft()}
              setEditDraft={setEditDraft}
              editAttachments={editAttachments()}
              onStartEdit={startEditMessage}
              onCancelEdit={cancelEditMessage}
              onSaveEdit={() => void saveEditMessage()}
              onRemoveEditAttachment={removeEditAttachment}
              onPickEditAttachments={() => void pickEditAttachments()}
            />
          </Show>

          <Show when={streamError()}>
            <p class="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-400">
              {streamError()}
            </p>
          </Show>
        </section>

        <Composer
          pendingAttachments={pendingAttachments()}
          onRemoveAttachment={removeAttachment}
          contextNote={contextNote()}
          attachmentWarning={attachmentWarning()}
          streaming={streaming()}
          onPickAttachments={pickAttachments}
          draft={draft()}
          setDraft={setDraft}
          onSend={send}
          onStop={stop}
        />
      </main>

      <SettingsDialog
        settingsOpen={settingsOpen()}
        setSettingsOpen={setSettingsOpen}
        closeSettings={closeSettings}
        wideViewport={wideViewport()}
        fullscreenInsetStyle={fullscreenInsetStyle}
        providers={providers()}
        providersLoading={providersLoading()}
        providerError={providerError()}
        setProviderError={setProviderError}
        addOpen={addOpen()}
        setAddOpen={setAddOpen}
        addName={addName()}
        setAddName={setAddName}
        addUrl={addUrl()}
        setAddUrl={setAddUrl}
        addKey={addKey()}
        setAddKey={setAddKey}
        addPick={addPick()}
        setAddPick={setAddPick}
        modelsDevProviders={modelsDevProviders()}
        doAddProvider={doAddProvider}
        editId={editId()}
        setEditId={setEditId}
        editName={editName()}
        setEditName={setEditName}
        editUrl={editUrl()}
        setEditUrl={setEditUrl}
        editKey={editKey()}
        setEditKey={setEditKey}
        startEdit={startEdit}
        cancelEdit={cancelEdit}
        doUpdateProvider={doUpdateProvider}
        doClearProviderKey={doClearProviderKey}
        doRemoveProvider={doRemoveProvider}
        doSetActive={doSetActive}
        modelOptions={modelOptions}
        overrideModel={overrideModel()}
        setOverrideModel={setOverrideModel}
        overrideFor={overrideFor}
        setOverrideField={setOverrideField}
        overrideEffective={overrideEffective()}
        modelOverrides={modelOverrides()}
        clearOverride={clearOverride}
        numFromInput={numFromInput}
        braveKeyDraft={braveKeyDraft()}
        setBraveKeyDraft={setBraveKeyDraft}
        hasBraveKeySaved={hasBraveKeySaved()}
        setHasBraveKeySaved={setHasBraveKeySaved}
        setBraveKey={setBraveKey}
        preferences={preferences()}
        setPreferences={setPreferences}
        echoReasoning={echoReasoning()}
        setEchoReasoning={setEchoReasoning}
        autoApproveTools={autoApproveTools()}
        setAutoApproveTools={setAutoApproveTools}
        reflectionEnabled={reflectionEnabled()}
        setReflectionEnabled={setReflectionEnabled}
        reflectionIdleMinutes={reflectionIdleMinutes()}
        setReflectionIdleMinutes={setReflectionIdleMinutes}
        shellWorkspaceDir={shellWorkspaceDir()}
        setShellWorkspaceDir={setShellWorkspaceDir}
        syncEnabled={syncEnabled()}
        syncBusy={syncBusy()}
        toggleSync={toggleSync}
        syncStatusData={syncStatusData()}
        syncAuthMode={syncAuthMode()}
        setSyncAuthMode={setSyncAuthMode}
        syncEmail={syncEmail()}
        setSyncEmail={setSyncEmail}
        syncPassword={syncPassword()}
        setSyncPassword={setSyncPassword}
        syncError={syncError()}
        setSyncError={setSyncError}
        syncNotice={syncNotice()}
        setSyncNotice={setSyncNotice}
        submitSyncSignIn={submitSyncSignIn}
        submitSyncSignUp={submitSyncSignUp}
        doSyncSignOut={doSyncSignOut}
        doSyncNow={doSyncNow}
        syncEncBusy={syncEncBusy()}
        syncRecoveryInput={syncRecoveryInput()}
        setSyncRecoveryInput={setSyncRecoveryInput}
        syncRecoveryShown={syncRecoveryShown()}
        enableEncryption={enableEncryption}
        unlockEncryption={unlockEncryption}
        showRecoveryCode={showRecoveryCode}
        disableEncryption={disableEncryption}
        syncLabel={syncLabel}
        syncBarWidth={syncBarWidth}
        formatLastSync={formatLastSync}
        settingsError={settingsError()}
        setSettingsError={setSettingsError}
      />

      <MemoryPage
        memoryOpen={memoryOpen()}
        setMemoryOpen={setMemoryOpen}
        memoryDetailOpen={memoryDetailOpen()}
        setMemoryDetailOpen={setMemoryDetailOpen}
        wideViewport={wideViewport()}
        fullscreenInsetStyle={fullscreenInsetStyle}
        memoryFiles={memoryFiles()}
        memorySelected={memorySelected()}
        selectMemoryFile={selectMemoryFile}
        selectedMemoryFile={selectedMemoryFile()}
        memoryDraft={memoryDraft()}
        setMemoryDraft={setMemoryDraft}
        memoryPreview={memoryPreview()}
        setMemoryPreview={setMemoryPreview}
        memoryConfirmDelete={memoryConfirmDelete()}
        setMemoryConfirmDelete={setMemoryConfirmDelete}
        removeMemoryFile={removeMemoryFile}
        saveSelectedMemory={saveSelectedMemory}
        memoryError={memoryError()}
        importReport={importReport()}
        extractionStats={extractionStats()}
        importChats={importChats}
        discardExtractions={discardExtractions}
        backfill={backfill()}
        backfillModels={backfillModels()}
        backfillModel={backfillModel()}
        setBackfillModel={setBackfillModel}
        backfillThinking={backfillThinking()}
        setBackfillThinking={setBackfillThinking}
        backfillThinkingOpts={backfillThinkingOpts()}
        startBackfill={startBackfill}
        stopBackfill={stopBackfill}
        activeId={activeId()}
        consolidating={consolidating()}
        consolidateNow={consolidateNow}
        reflectionStats={reflectionStats()}
      />

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
                <Show when={openThinking()?.live}>
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
                {openThinking()?.text}
              </pre>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}