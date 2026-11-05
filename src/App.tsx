import { createEffect, createMemo, createSignal, onCleanup, onMount, Show, untrack } from "solid-js";
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
  compactConversation,
  createConversation,
  createCron,
  cronSetActive,
  deleteConversation,
  deleteCron,
  deleteMemoryFile,
  denyTool,
  editUserMessage,
  generateConversationTitle,
  getConfig,
  getConversationAutoApprove,
  getPricing,
  hasBraveKey,
  importConversations,
  listConversations,
  listCronRuns,
  listCrons,
  listCronTriggers,
  listMemoryFiles,
  listMessages,
  listModels,
  listModelsDevProviders,
  listProviders,
  listSkills,
  memoryExtractionStats,
  memoryReflectionStats,
  pickAttachmentUris,
  readAttachments,
  reflectNow,
  removeProvider,
  renameConversation,
  retryAttachmentUpload,
  runCronNow,
  searchConversations,
  setActiveProvider,
  setBraveKey,
  setConfig,
  setConversationAutoApprove,
  setConversationPinned,
  setConversationModel,
  setConversationProvider,
  setConversationThinkingLevel,
  setModelOverrides as saveModelOverrides,
  stopChat,
  streamChat,
  updateCron,
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
  type CreateCronPayload,
  type CronJob,
  type CronRun,
  type CronUpdateEvent,
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
  type SkillInfo,
  type SlashItem,
  type StreamEvent,
  type SyncStatus,
  type ThinkingOptions,
  type UpdateCronPayload,
} from "./lib/api";
import { isAndroidDevice, onBack, readAttachmentFromUri } from "./lib/platform";
import { acceptsImages, resolveInputModalities } from "./lib/capabilities";
import { createKeyboardInset } from "./lib/viewport";
import { Composer } from "./components/Composer";
import { CronPage } from "./components/CronPage";
import { Header } from "./components/Header";
import { ImageLightbox } from "./components/ImageLightbox";
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

// Photo / Camera picks are image-only. (A desktop webcam cannot be opened by
// the OS file dialog, so Camera falls back to the image filter there.)
const IMAGE_FILTERS = [
  { name: "Images", extensions: ["png", "jpg", "jpeg", "gif", "webp", "bmp"] },
];

/**
 * Pick attachment paths. Android: native picker bridge (returns `content://`
 * URIs the content-URI reader already handles); `kind` selects any / image /
 * camera. Desktop: the dialog plugin, where Camera falls back to images.
 * `null` = cancelled.
 */
async function pickAttachmentPaths(
  kind: "any" | "image" | "camera" = "any",
): Promise<string[] | null> {
  if (isAndroidDevice()) {
    const uris = await pickAttachmentUris(kind);
    return uris.length > 0 ? uris : null;
  }
  const filters = kind === "any" ? ATTACHMENT_FILTERS : IMAGE_FILTERS;
  const picked = await open({ multiple: true, directory: false, filters });
  if (!picked) return null;
  return Array.isArray(picked) ? picked : [picked];
}

/**
 * Conversations whose title was already generated this session. A chat is
 * named at most once, so a streamed `done` (which also tries, idempotently)
 * cannot re-issue the request. An empty/failed attempt is removed again so a
 * later settled turn can retry.
 */
const autoTitleRequested = new Set<string>();

/**
 * Merge a freshly fetched conversation list into the previous one, keeping the
 * SAME object reference for every row whose fields did not change.
 *
 * `<For>` diffs by identity, so replacing every object on each background
 * refresh (a turn finishing, cron tick, sync pull, reflection note) re-created
 * every sidebar row — flicker, lost hover, lost focus. Returning the previous
 * array unchanged when nothing moved also skips the whole diff.
 */
function mergeConversations(prev: Conversation[], next: Conversation[]): Conversation[] {
  const prevById = new Map(prev.map((c) => [c.id, c]));
  let changed = prev.length !== next.length;
  const merged = next.map((row, i) => {
    const old = prevById.get(row.id);
    if (old && sameConversation(old, row)) {
      if (prev[i] !== old) changed = true; // same rows, different order
      return old;
    }
    changed = true;
    return row;
  });
  return changed ? merged : prev;
}

function sameConversation(a: Conversation, b: Conversation): boolean {
  return (
    a.id === b.id &&
    a.title === b.title &&
    a.kind === b.kind &&
    a.model === b.model &&
    a.provider_id === b.provider_id &&
    (a.thinking_level ?? null) === (b.thinking_level ?? null) &&
    !!a.pinned === !!b.pinned &&
    a.system_prompt === b.system_prompt &&
    a.compaction_summary === b.compaction_summary &&
    a.compaction_through === b.compaction_through &&
    a.created_at === b.created_at &&
    a.updated_at === b.updated_at
  );
}

export default function App() {
  const [conversations, setConversations] = createSignal<Conversation[]>([]);
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Message[]>([]);
  const [draft, setDraft] = createSignal("");
  // Turn/attachment failures are **conversation-scoped**: an error raised in one
  // chat must not stay on screen after switching to another. Keyed by
  // conversation id; `""` is the key for failures raised with no conversation
  // open (a failed attachment pick before a chat exists). See the helpers below.
  const [streamErrors, setStreamErrors] = createSignal<Record<string, string>>({});
  const [nearBottom, setNearBottom] = createSignal(true);
  let messagesScrollEl: HTMLElement | undefined;

  // Auto-naming: the model used to title a chat from its first turn (empty =
  // the chat's own model).
  const [titleModel, setTitleModel] = createSignal("");
  // Per-chat "YOLO" overrides, keyed by conversation id. `null` (or a missing
  // key) inherits the device default (`autoApproveTools`); `true`/`false` pins
  // this chat explicitly.
  const [chatYolo, setChatYolo] = createSignal<Record<string, boolean | null>>({});
  // User-authored skills, offered as `/` commands in the composer.
  const [skills, setSkills] = createSignal<SkillInfo[]>([]);
  // Transient, non-blocking status line above the composer (`/compact`).
  const [notice, setNotice] = createSignal<string | null>(null);
  // Soft-keyboard height in px (0 on desktop): lifts the composer and shrinks
  // the transcript while the mobile keyboard is open.
  const keyboardInset = createKeyboardInset();

  // Editing a user message happens in the CENTRAL composer: the message's text
  // and attachments are loaded into the ordinary `draft`/`pendingAttachments`
  // state and `editingId` records which row a send should rewrite. There is no
  // separate inline editor, so there is one place where a turn's content lives
  // (and one Send/Cancel pair).
  const [editingId, setEditingId] = createSignal<string | null>(null);
  // The composer's own contents from before the pen loaded a message for
  // editing. Loading an edit reuses `draft`/`pendingAttachments`, so without
  // this stash both Send-edit and Cancel would silently destroy what the user
  // had typed (or attached). Deliberately NON-reactive: it is written on the
  // pen click and read only on cancel, so making it a signal would re-render
  // the whole tree for state nothing renders. `preEditStashed` is the single
  // source of truth for "an edit is loaded and owns the composer"; a second pen
  // click while editing replaces the loaded text but never the stash.
  let preEditDraft = "";
  let preEditAttachments: Attachment[] = [];
  let preEditStashed = false;
  const dropEditStash = () => {
    preEditDraft = "";
    preEditAttachments = [];
    preEditStashed = false;
  };
  // Bumped to ask the composer to focus its textarea (after the edit pen loads
  // a message); the number itself carries no meaning.
  const [composerFocusNonce, setComposerFocusNonce] = createSignal(0);

  // Conversations whose turn (live or scheduled) settled while the user was
  // looking at another chat. Drives the per-row sidebar dot and the hamburger
  // dot; cleared when that conversation becomes active. Indicators only — OS
  // notifications are the backend's job.
  const [completedUnseen, setCompletedUnseen] = createSignal<ReadonlySet<string>>(new Set());
  const markUnseen = (id: string) =>
    setCompletedUnseen((prev) => (prev.has(id) ? prev : new Set(prev).add(id)));
  const clearUnseen = (id: string) =>
    setCompletedUnseen((prev) => {
      if (!prev.has(id)) return prev;
      const next = new Set(prev);
      next.delete(id);
      return next;
    });

  // --- Per-conversation streaming state ---
  // In-flight turn bookkeeping per conversation. `messages()` only holds the
  // visible conversation's rows; a chat switched away from drops its `tmp-` rows
  // on reload. This record preserves the accumulating answer (plus live tool
  // cards and the current round's preamble/thinking) so switching back can
  // re-attach the live row and keep rendering streamed output. Keyed by
  // conversation id; an entry exists only while a turn is in flight.
  interface LiveTurn {
    token: number;            // this turn's per-conversation token (turnTokens[id])
    assistantKey: string;     // "tmp-assistant-N"
    startedAt: number;        // `Date.now()` when the turn was launched
    content: string;          // accumulated assistant text
    thinking: string;         // accumulated reasoning
    liveTools: LiveTool[];
  }
  const [streamingIds, setStreamingIds] = createSignal<ReadonlySet<string>>(new Set());
  // Conversations with a **scheduled run** in flight. A cron run has no UI
  // channel, so `streamingIds` (populated by live stream events) cannot know
  // about it; the `cron-updated` started/finished/error events drive this set.
  // It is part of `isBusy`, so Stop, send and edit are gated on it — without
  // that, a scheduled run is invisible and un-abortable (and editing mid-run
  // would fork the transcript).
  const [cronRunning, setCronRunning] = createSignal<ReadonlySet<string>>(new Set());
  // Conversations with an in-flight `/compact` pass. The backend holds an
  // exclusive StreamRegistry claim, so the composer must show busy/Stop rather
  // than let a send race the summarizer.
  const [compactingIds, setCompactingIds] = createSignal<ReadonlySet<string>>(new Set());
  const [liveTurns, setLiveTurns] = createSignal<Record<string, LiveTurn>>({});
  // Per-conversation turn tokens: a resend/Stop→resend only supersedes the same
  // conversation's events, never another conversation's stream.
  const turnTokens: Record<string, number> = {};

  const addStreaming = (id: string) =>
    setStreamingIds((prev) => (prev.has(id) ? prev : new Set(prev).add(id)));
  const removeStreaming = (id: string) =>
    setStreamingIds((prev) =>
      prev.has(id) ? new Set([...prev].filter((x) => x !== id)) : prev,
    );
  const isStreaming = (id: string | null | undefined) => !!id && streamingIds().has(id);
  const setCronRun = (id: string, running: boolean) =>
    setCronRunning((prev) => {
      if (running === prev.has(id)) return prev;
      const next = new Set(prev);
      if (running) next.add(id);
      else next.delete(id);
      return next;
    });
  // "A turn of any kind is using this chat": live streaming OR a scheduled run.
  // UI gates (send / Stop / inline edit) must use this, not `isStreaming`, or a
  // cron run becomes invisible and un-abortable.
  const isBusy = (id: string | null | undefined) =>
    isStreaming(id) || (!!id && cronRunning().has(id)) ||
    (!!id && compactingIds().has(id));
  const setCompacting = (id: string, on: boolean) =>
    setCompactingIds((prev) => {
      if (on === prev.has(id)) return prev;
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  // Set (or clear, with `null`) the error shown for a conversation. A `null` key
  // is the "no conversation open" bucket. Returning `prev` unchanged when nothing
  // changes keeps the banner from re-rendering on every harmless clear.
  const setStreamErrorFor = (id: string | null | undefined, message: string | null) =>
    setStreamErrors((prev) => {
      const key = id ?? "";
      if (message === null) {
        if (!(key in prev)) return prev;
        const next = { ...prev };
        delete next[key];
        return next;
      }
      if (prev[key] === message) return prev;
      return { ...prev, [key]: message };
    });
  const clearStreamError = (id: string | null | undefined) => setStreamErrorFor(id, null);
  // The banner renders only the OPEN conversation's error.
  const streamError = () => streamErrors()[activeId() ?? ""] ?? null;
  // Live tool cards for the ACTIVE conversation (derived, so no shared array):
  const liveTools = () => liveTurns()[activeId() ?? ""]?.liveTools ?? [];

  function patchLiveTurn(id: string, patch: (live: LiveTurn) => void) {
    setLiveTurns((prev) => {
      const cur = prev[id];
      if (!cur) return prev;
      const next = { ...cur };
      patch(next);
      return { ...prev, [id]: next };
    });
  }
  function setLiveTurn(id: string, turn: LiveTurn) {
    setLiveTurns((prev) => ({ ...prev, [id]: turn }));
  }
  // Token-guarded drop: a superseded (Stop→resend) turn's terminal event must not
  // delete the NEW turn's live record — same pattern as the backend's registry.
  function dropLiveTurn(id: string, token: number) {
    setLiveTurns((prev) => {
      if (prev[id]?.token !== token) return prev;
      const next = { ...prev };
      delete next[id];
      return next;
    });
  }

  // A staged (`tmp-user-*`) row is the local preview of a turn the backend
  // persists just before streaming starts. Two rows are the same turn when the
  // text matches and they carry the same attachment ids (unique per picked
  // file), AND the persisted row is not older than the staged one — the backend
  // inserts it milliseconds after the preview exists. The timestamp is what
  // keeps a repeated identical send (or a cron thread replaying its prompt) from
  // matching the PREVIOUS turn's identical user row and dropping the bubble.
  function sameStagedUserRow(persisted: Message, staged: Message): boolean {
    if (persisted.role !== "user" || persisted.content !== staged.content) return false;
    if (persisted.created_at < staged.created_at) return false;
    const ids = (m: Message) => parseAttachments(m).map((a) => a.id).sort().join(",");
    return ids(persisted) === ids(staged);
  }

  /** Single implementation of "refresh the open conversation's transcript".
   *
   *  Every reload used to be `listMessages(id).then(setMessages)` guarded by
   *  `!isStreaming(id)` **before** the await — so a 60s sync pull, a reflection
   *  finish, a cron event or the chat-switch loader could resolve MID-TURN and
   *  replace `messages` with the persisted rows, which lack the in-flight
   *  assistant row (and any staged `tmp-user-*` row). That closed the activity
   *  panel and froze the answer while the model was still talking. The decision
   *  is therefore deferred to RESOLUTION time:
   *
   *  - the chat is no longer open → discard the snapshot;
   *  - a live turn exists → re-attach its assistant row (like the switch loader)
   *    so deltas keep appending to a rendered row;
   *  - a turn is registered but has no live record (transient) → leave the
   *    transcript alone; the turn's own terminal reload reconciles it;
   *  - otherwise → apply the persisted rows, keeping any staged user row the
   *    snapshot does not contain yet.
   */
  async function reloadTranscript(id: string) {
    let rows: Message[];
    try {
      rows = await listMessages(id);
    } catch {
      return; // best-effort refresh; the turn's terminal reload reconciles
    }
    // The user switched (or closed the chat) while the fetch was in flight: this
    // snapshot belongs to a conversation that is no longer on screen.
    if (activeId() !== id) return;
    const current = messages();
    // Only the LAST persisted user row can be the staged turn's own copy: the
    // backend appends the user row for the current turn, so matching any earlier
    // user row would make a repeated send of the same text (a cron thread
    // replaying its prompt, or the user typing the same line twice) look already
    // persisted and drop the staged bubble from the second turn.
    const lastPersistedUser = rows.filter((r) => r.role === "user").slice(-1)[0];
    const staged = current.filter(
      (m) =>
        m.conversation_id === id &&
        m.id.startsWith("tmp-user-") &&
        (!lastPersistedUser || !sameStagedUserRow(lastPersistedUser, m)),
    );
    const live = liveTurns()[id];
    if (live) {
      // The backend writes the final assistant row immediately before emitting
      // `done`. If a reload resolves in that window, appending the synthetic live
      // row would leave two assistant rows for one turn. The persisted final row
      // is the ONLY row created after this turn started, so its timestamp is an
      // exact discriminator (content comparison could confuse a repeated
      // identical reply with the previous turn's answer).
      const tail = rows.slice(-1)[0];
      if (tail && tail.role === "assistant" && tail.stop_reason != null && tail.created_at >= live.startedAt) {
        setMessages([...rows, ...staged]);
        return;
      }
      // Re-attach the in-flight assistant row so a reload that raced the start
      // of a turn cannot hide it. Deltas keep appending to this key.
      setMessages([
        ...rows,
        ...staged,
        {
          id: live.assistantKey,
          conversation_id: id,
          role: "assistant",
          index: rows.length + staged.length,
          content: live.content,
          model: null,
          provider: null,
          thinking_level: null,
          thinking: live.thinking,
          usage: null,
          stop_reason: null,
          attachments: null,
          created_at: Date.now(),
        },
      ]);
      return;
    }
    // A turn that is registered without a live record yet (or any more) is a
    // transient state between the streaming flags and the record itself; leave
    // the visible transcript alone rather than risk dropping its live row.
    if (isStreaming(id)) return;
    setMessages([...rows, ...staged]);
  }

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

  const [searchQuery, setSearchQuery] = createSignal("");
  const [visibleConversations, setVisibleConversations] = createSignal<Conversation[]>([]);
  const [renameId, setRenameId] = createSignal<string | null>(null);
  const [renameValue, setRenameValue] = createSignal("");
  const [confirmDeleteId, setConfirmDeleteId] = createSignal<string | null>(null);

  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [baseUrl, setBaseUrl] = createSignal("");
  // `model` is what the header shows: the model in effect for the OPEN
  // conversation (its own override, or the device default when it has none).
  const [model, setModel] = createSignal("");
  // The device-wide default model from config. A conversation without its own
  // `model` column inherits this (exactly what the backend does in
  // `run_turn_core`), and a scheduled job's thread is one such conversation —
  // so switching to it must show THIS, not whatever chat was open before.
  const [globalModel, setGlobalModel] = createSignal("");
  const [preferences, setPreferences] = createSignal("");
  const [thinkingLevel, setThinkingLevel] = createSignal("");
  // The GLOBAL configured level (the default for new chats). Kept apart from
  // `thinkingLevel`, which is the level in effect for the open conversation: a
  // chat may override it, and switching to a chat with no override must fall
  // back to this value rather than to the previous chat's level.
  const [globalThinkingLevel, setGlobalThinkingLevel] = createSignal("");
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

  // --- Scheduled jobs (cron) ---
  // Jobs run prompts in conversations on a schedule, in the background.
  // `cronUnseen` drives the header dot: set when a run finishes/errors while
  // the Cron dialog is closed, cleared when it opens. `cronEnabled` is the
  // global kill-switch (Settings toggle, default true).
  const [cronOpen, setCronOpen] = createSignal(false);
  const [cronJobs, setCronJobs] = createSignal<CronJob[]>([]);
  const [cronRuns, setCronRuns] = createSignal<CronRun[]>([]);
  // messageId → job title for the transcript "scheduled · <title>" pill.
  const [cronTriggers, setCronTriggers] = createSignal<Record<string, string>>({});
  // The job whose recent runs are shown in the dialog; owned here so the
  // cron-updated handler can also refresh those runs.
  const [cronSelectedId, setCronSelectedId] = createSignal<string | null>(null);
  const [cronBusy, setCronBusy] = createSignal(false);
  const [cronError, setCronError] = createSignal<string | null>(null);
  const [cronUnseen, setCronUnseen] = createSignal(false);
  const [cronEnabled, setCronEnabled] = createSignal(true);
  // Sequencing guard for `loadCronRuns`: a slow response for a previous job
  // must not overwrite the newer selection's runs.
  let cronRunsSeq = 0;
  /// Guard for `refreshCronTriggers`: a response for a chat the user already
  /// switched away from must not overwrite the current chat's pill map.
  let cronTriggersSeq = 0;

  // Theme: dark by default, persisted across launches, toggled from the header.
  const [dark, setDark] = createSignal(true);
  /// Reasoning popup state: a `{id, text}` snapshot rather than a bare id. The
  /// id is re-resolved against `messages()`/`liveTools()` while it still
  /// exists (so a streamed trace keeps growing), and the snapshot text is the
  /// fallback once the row it named goes away — a round flushing to a tool call
  /// resets the in-flight row, and the end-of-stream reload replaces `tmp-` rows
  /// with persisted ones. That is what keeps an open popup from vanishing.
  const [openTrace, setOpenTrace] = createSignal<{
    id: string;
    text: string;
    live: boolean;
  } | null>(null);
  const thinkingOpenId = () => openTrace()?.id ?? null;

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
  // True when the native shell owns the keyboard/system insets (Android's
  // `MainActivity.kt` injects `window.PiInsets` before the page loads). There,
  // `createKeyboardInset()` deliberately stays 0 (see `lib/viewport.ts`), so
  // anything that needs the keyboard offset must use `keyboardLift()` below.
  const hasNativeInsets = createMemo(() => {
    const w = window as unknown as { PiInsets?: unknown };
    return !!w.PiInsets;
  });

  // The single keyboard lift the transcript follows, in CSS px. On Android that
  // is the native bottom inset (which already folds in the IME); elsewhere it
  // is the visual-viewport estimate. `Math.max` also keeps one value if both
  // ever report something.
  const keyboardLift = () =>
    Math.max(keyboardInset(), hasNativeInsets() ? insets().bottom : 0);

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
    // priority order (memory detail → memory dialog → cron dialog → settings →
    // new chat → reasoning popup → sidebar drawer → inline edit). Returning
    // `false` when nothing is open lets the native side finish the activity,
    // so back exits only on the chat page. Each close goes through the same
    // setter/handler as its visible control (e.g. `closeSettings` saves, the
    // edit branch cancels the inline editor). Inert on desktop, where no
    // `pi-back` event is ever dispatched.
    const offBack = onBack(() => {
      if (memoryOpen() && memoryDetailOpen()) {
        setMemoryDetailOpen(false);
        return true;
      }
      if (memoryOpen()) {
        setMemoryOpen(false);
        return true;
      }
      if (cronOpen()) {
        setCronOpen(false);
        return true;
      }
      if (settingsOpen()) {
        void closeSettings();
        return true;
      }
      if (openTrace()) {
        setOpenTrace(null);
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
    // Escape also closes the reasoning popup (the image lightbox handles its own
    // Escape inside the component).
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (openTrace()) {
        setOpenTrace(null);
        return;
      }
      if (sidebarOpen()) {
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
        if (id && event.payload.conversationId === id) {
          // The note lands in this chat's transcript; the reload must not clobber
          // a turn that started (or is still running) while the event arrived.
          void reloadTranscript(id);
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
      if (id) void reloadTranscript(id);
    });
    onCleanup(() => unlistenSync);

    // A scheduled job started / finished / errored. Any event refreshes the
    // job list (next-run, counters) and the trigger map (a run may have posted
    // a message); a run in the OPEN chat reloads its transcript (unless that
    // chat is mid-stream, whose own reload reconciles it) and re-sorts the
    // conversation list. The unseen dot is set only when a run settles while
    // the dialog is closed — opening it clears the flag.
    const unlistenCron = await listen<CronUpdateEvent>("cron-updated", (event) => {
      const { jobId, conversationId, status } = event.payload;
      // A scheduled run has no stream channel, so this event is the only signal
      // that the chat is occupied: feed the busy set first, then refresh.
      setCronRun(conversationId, status === "started");
      void refreshCrons();
      void refreshCronTriggers();
      if (status === "finished" || status === "error") {
        if (!cronOpen()) setCronUnseen(true);
        // The thread produced output while the user was elsewhere: flag it in the
        // sidebar (cleared when they open it) — the transcript refresh below only
        // covers the chat that is on screen.
        if (conversationId && conversationId !== activeId()) markUnseen(conversationId);
        // A run appends messages and bumps `updated_at`, so the sidebar order
        // must refresh even when the chat is not the open one.
        void refreshConversations();
      }
      const id = activeId();
      if (id && conversationId === id) void reloadTranscript(id);
      // Keep the selected job's run history live while the dialog is open.
      if (cronSelectedId() && cronSelectedId() === jobId) void loadCronRuns(jobId);
    });
    onCleanup(() => unlistenCron);

    const rows = await listConversations();
    setConversations(rows);
    setVisibleConversations(rows);
    // Open a real chat, not a job's thread that happened to fire most recently.
    const firstChat = rows.find((c) => c.kind !== "scheduled") ?? rows[0];
    if (firstChat) setActiveId(firstChat.id);

    const cfg = await getConfig();
    setBaseUrl(cfg.baseUrl);
    // Preserve the opened chat's own model: the conversation row is the source
    // of truth for a chat, `cfg.model` only for one that has none.
    const openedModel = firstChat?.model?.trim();
    setModel(openedModel ? openedModel : cfg.model);
    setGlobalModel(cfg.model);
    setPreferences(cfg.preferences ?? "");
    setThinkingLevel(cfg.thinkingLevel ?? "");
    setGlobalThinkingLevel(cfg.thinkingLevel ?? "");
    setEchoReasoning(cfg.echoReasoningContent ?? true);
    setAutoApproveTools(cfg.autoApproveTools ?? false);
    setTitleModel(cfg.titleModel ?? "");
    setCronEnabled(cfg.cronEnabled ?? true);
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
    // Cron jobs + the transcript trigger map, so the sidebar glyphs and the
    // "scheduled · <title>" pills are right on first paint.
    void refreshCrons();
    void refreshCronTriggers();
    void loadSkills();
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
    // Preserve object identity for unchanged rows: `<For>` diffs by identity,
    // so replacing every conversation on a background refresh re-created every
    // sidebar row (flicker, lost hover/focus).
    setConversations((prev) => mergeConversations(prev, rows));
    setVisibleConversations((prev) => mergeConversations(prev, rows));
    // Drop visible-inbox ids whose conversation no longer exists. A chat can
    // disappear without being opened or deleted by the user: a sync pull applies
    // a remote tombstone, `db::open` prunes an orphaned scheduled thread, or a
    // pull rewrites/renumbers rows. Such an id has no row to open, so the
    // hamburger dot would stay amber until a restart with no way to clear it.
    // Only prune from a COMPLETE list: during a search `rows` is a filtered
    // subset, so intersecting would wrongly drop the dot for live chats.
    // Return the previous Set when nothing was pruned, keeping the signal
    // stable (no re-render of the sidebar / hamburger).
    if (!q) {
      setCompletedUnseen((prev) => {
        if (prev.size === 0) return prev;
        const live = new Set(rows.map((c) => c.id));
        const next = new Set<string>();
        let pruned = false;
        for (const id of prev) {
          if (live.has(id)) next.add(id);
          else pruned = true;
        }
        return pruned ? next : prev;
      });
    }
  }

  // --- Auto chat names -------------------------------------------------------

  /// Ask the backend to name a chat from its first user turn. Fire-and-forget:
  /// title generation must never block sending/streaming. It runs from the
  /// stream's terminal `done` (the user row exists by then); the backend owns
  /// the "is this title still auto-generated?" decision, so a user rename is
  /// never overwritten. Each chat is requested at most once per session.
  async function maybeAutoTitle(id: string) {
    if (autoTitleRequested.has(id)) return;
    // Mark before the first await so a concurrent `send`/`done` attempt cannot
    // issue a second (paid) title request for the same chat.
    autoTitleRequested.add(id);
    // `messages()` belongs to the chat on screen; an off-screen settlement needs
    // its own rows to count user turns.
    let rows: Message[];
    if (activeId() === id) rows = messages();
    else rows = await listMessages(id).catch(() => []);
    const userTurns = rows.filter((m) => m.role === "user").length;
    // No user turn yet (an early/failed send): allow a later attempt. More than
    // one: only the first turn names the chat.
    if (userTurns === 0) {
      autoTitleRequested.delete(id);
      return;
    }
    if (userTurns > 1) return;
    try {
      const generated = (await generateConversationTitle(id)).trim();
      if (!generated || generated === "New chat") {
        // Nothing usable (e.g. called before the user row was persisted): allow
        // a later settled turn to retry.
        autoTitleRequested.delete(id);
        return;
      }
      // Update in place (keeping the row's identity stable for the sidebar) and
      // then reconcile against the backend's list.
      setConversations((prev) =>
        prev.map((c) => (c.id === id ? { ...c, title: generated } : c)),
      );
      setVisibleConversations((prev) =>
        prev.map((c) => (c.id === id ? { ...c, title: generated } : c)),
      );
      await refreshConversations();
    } catch {
      // Non-fatal; allow a later turn to retry.
      autoTitleRequested.delete(id);
    }
  }

  // --- Per-chat YOLO ---------------------------------------------------------

  /// The YOLO value in effect for the open chat: its own explicit choice, else
  /// the device default.
  const effectiveChatYolo = (): boolean => {
    const id = activeId();
    const explicit = id ? chatYolo()[id] : undefined;
    return explicit ?? autoApproveTools();
  };

  /// True when the open chat has no explicit choice and inherits the default.
  const chatYoloInherited = (): boolean => {
    const id = activeId();
    return !id || chatYolo()[id] == null;
  };

  // Load the open chat's override whenever it becomes active. A slow response
  // for a chat the user already switched away from is dropped by the token.
  let yoloReqSeq = 0;
  createEffect(() => {
    const id = activeId();
    const seq = ++yoloReqSeq;
    if (!id) return;
    getConversationAutoApprove(id)
      .then((value) => {
        if (seq !== yoloReqSeq || activeId() !== id) return;
        setChatYolo((prev) => ({ ...prev, [id]: value }));
      })
      .catch(() => {
        // Non-fatal: the chat just inherits the device default.
      });
  });

  /// Pin this chat's YOLO to the opposite of what is in effect (or, if it was
  /// inheriting, to the explicit opposite). `null` is never written here: the
  /// picker toggles between explicit on/off.
  /// Clear this chat's explicit YOLO choice so it inherits the Settings value
  /// again. Unlike the toggle, this writes `null`.
  async function clearChatYolo() {
    const id = activeId();
    if (!id) return;
    const prev = chatYolo()[id];
    // Supersede any in-flight load for this chat before the optimistic write.
    ++yoloReqSeq;
    setChatYolo((p) => ({ ...p, [id]: null }));
    try {
      await setConversationAutoApprove(id, null);
    } catch (e) {
      setChatYolo((p) => {
        const copy = { ...p };
        if (prev === undefined) delete copy[id];
        else copy[id] = prev;
        return copy;
      });
      showNotice(String(e));
    }
  }

  async function toggleChatYolo() {
    const id = activeId();
    if (!id) return;
    // A load started when the chat opened must not land after this write and
    // restore the pre-toggle value.
    ++yoloReqSeq;
    const next = !effectiveChatYolo();
    const prev = chatYolo()[id];
    setChatYolo((p) => ({ ...p, [id]: next }));
    try {
      await setConversationAutoApprove(id, next);
    } catch (e) {
      // Put the previous (possibly inherited) value back and say why.
      setChatYolo((p) => {
        const copy = { ...p };
        if (prev === undefined) delete copy[id];
        else copy[id] = prev;
        return copy;
      });
      showNotice(String(e));
    }
  }

  // --- Pinned chats ---------------------------------------------------------

  /// Pin/unpin a chat. The backend stamps the synced row; re-reading the list
  /// moves it into/out of the sidebar's Pinned section.
  async function toggleChatPinned(c: Conversation) {
    try {
      await setConversationPinned(c.id, !c.pinned);
      await refreshConversations();
    } catch (e) {
      showNotice(String(e));
    }
  }

  // --- Skills + `/compact` ---------------------------------------------------

  async function loadSkills() {
    try {
      setSkills(await listSkills());
    } catch {
      // Non-fatal: the slash menu simply shows the built-in commands.
    }
  }

  // Skills first, built-in commands last. The composer also requires an
  // explicit selection for commands, so a bare `/` + Enter can never run
  // `/compact` by accident.
  const slashItems = createMemo<SlashItem[]>(() => [
    ...skills().map((s) => ({
      id: s.id,
      name: s.name,
      description: s.description,
      kind: "skill" as const,
    })),
    {
      id: "compact",
      name: "compact",
      description: "Summarise older messages into a summary",
      kind: "command",
    },
  ]);

  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  /// Show (or clear with `null`) the transient line above the composer. A new
  /// notice replaces the old one and restarts the ~6s auto-clear.
  function showNotice(text: string | null) {
    if (noticeTimer !== undefined) {
      clearTimeout(noticeTimer);
      noticeTimer = undefined;
    }
    setNotice(text);
    if (text !== null) {
      noticeTimer = setTimeout(() => {
        noticeTimer = undefined;
        setNotice(null);
      }, 6000);
    }
  }
  onCleanup(() => {
    if (noticeTimer !== undefined) clearTimeout(noticeTimer);
  });

  async function runCompact() {
    const id = activeId();
    if (!id) return;
    if (isBusy(id)) {
      showNotice("Wait for the current turn to finish, then run /compact again.");
      return;
    }
    setCompacting(id, true);
    showNotice("Compacting older messages…");
    try {
      const result = await compactConversation(id);
      await reloadTranscript(id);
      await refreshConversations();
      // The user may have switched chats while the pass ran; don't show its
      // result over an unrelated transcript.
      if (activeId() !== id) return;
      showNotice(
        result.compacted ? "Context compacted." : (result.note || "Nothing to compact."),
      );
    } catch (e) {
      showNotice(String(e));
    } finally {
      setCompacting(id, false);
    }
  }

  // --- Scheduled jobs (cron) handlers ---

  // Refresh the job list. Background (event-driven) refreshes must never flash
  // an error banner, so failures are swallowed here; user-triggered actions
  // set `cronError` themselves.
  async function refreshCrons() {
    try {
      setCronJobs(await listCrons());
    } catch {
      // non-fatal — a later event/action retries
    }
  }

  // Rebuild the messageId → job title map for the transcript pills. Scoped to
  // the open chat (that is all MessageList renders), so the query stays small
  // even after years of scheduled runs; no chat means no pills.
  async function refreshCronTriggers() {
    const id = activeId();
    const seq = ++cronTriggersSeq;
    if (!id) {
      setCronTriggers({});
      return;
    }
    try {
      const triggers = await listCronTriggers(id);
      // A slow response for a previous chat must not overwrite the current
      // chat's pills (and the user may have switched away in the meantime).
      if (seq !== cronTriggersSeq || activeId() !== id) return;
      const map: Record<string, string> = {};
      for (const t of triggers) map[t.messageId] = t.title;
      setCronTriggers(map);
    } catch {
      // non-fatal (the pill is a nice-to-have)
    }
  }

  async function loadCronRuns(jobId: string | null) {
    if (!jobId) {
      setCronRuns([]);
      return;
    }
    const seq = ++cronRunsSeq;
    try {
      const runs = await listCronRuns(jobId, 20);
      // A newer selection superseded this one — drop the stale result.
      if (seq === cronRunsSeq) setCronRuns(runs);
    } catch (e) {
      if (seq === cronRunsSeq) setCronError(String(e));
    }
  }

  async function createCronJob(payload: CreateCronPayload): Promise<boolean> {
    setCronBusy(true);
    setCronError(null);
    try {
      await createCron(payload);
      await refreshCrons();
      // The job owns a fresh thread (kind `scheduled`): show it in the sidebar.
      void refreshConversations();
      return true;
    } catch (e) {
      setCronError(String(e));
      return false;
    } finally {
      setCronBusy(false);
    }
  }

  /// Patch a job (enabled/notify toggles and the edit form all land here).
  async function patchCron(
    id: string,
    patch: Omit<UpdateCronPayload, "id">,
  ): Promise<boolean> {
    setCronBusy(true);
    setCronError(null);
    try {
      await updateCron({ id, ...patch });
      await refreshCrons();
      // A title edit renames the job's thread too.
      if (patch.title != null) void refreshConversations();
      return true;
    } catch (e) {
      setCronError(String(e));
      return false;
    } finally {
      setCronBusy(false);
    }
  }

  async function deleteCronJob(id: string) {
    setCronBusy(true);
    setCronError(null);
    const threadId = cronJobs().find((j) => j.id === id)?.conversationId ?? null;
    try {
      await deleteCron(id);
      // Drop the selection if the deleted job was the one shown.
      setCronSelectedId((sel) => (sel === id ? null : sel));
      await refreshCrons();
      // The job owned its thread, so that conversation is gone too.
      if (threadId) {
        await refreshConversations();
        void refreshCronTriggers();
        if (activeId() === threadId) {
          const next =
            conversations().find((c) => c.kind !== "scheduled") ?? conversations()[0];
          setActiveId(next ? next.id : null);
        }
      }
    } catch (e) {
      setCronError(String(e));
    } finally {
      setCronBusy(false);
    }
  }

  async function runCronJobNow(id: string) {
    setCronBusy(true);
    setCronError(null);
    try {
      await runCronNow(id);
      // The `cron-updated` events refresh jobs/runs; refresh once here too so
      // lastRunAt/counters update even if the run settles before the listener
      // fires.
      void refreshCrons();
      void loadCronRuns(cronSelectedId());
    } catch (e) {
      setCronError(String(e));
    } finally {
      setCronBusy(false);
    }
  }

  function openCron() {
    setCronError(null);
    setCronOpen(true);
    // Opening the dialog clears the unseen dot; fresh data on the way.
    setCronUnseen(false);
    void refreshCrons();
    void refreshCronTriggers();
  }

  // Tell the backend which chat is open so a scheduled run there suppresses
  // its OS notification. Fires on every conversation switch (and the initial
  // null — the backend treats that as "no chat open").
  createEffect(() => {
    const id = activeId();
    // Suppress a run's notification only while the transcript it lands in is
    // actually on screen: an open Cron / Memory / Settings dialog covers it even
    // though the window still has focus.
    const visible = !cronOpen() && !memoryOpen() && !settingsOpen();
    cronSetActive(visible ? id : null).catch(() => {});
  });

  // Conversations owning at least one enabled job — the sidebar glyph source.
  // Excludes paused jobs, matching "Has a scheduled job" in spirit.
  const cronConversationIds = createMemo(
    () => new Set(cronJobs().filter((j) => j.enabled).map((j) => j.conversationId)),
  );

  // Sidebar search: query the backend (title + message text) when non-empty.
  createEffect(() => {
    const q = searchQuery().trim();
    const t = setTimeout(() => void refreshConversations(), q ? 200 : 0);
    onCleanup(() => clearTimeout(t));
  });

  // Auto-load messages whenever the active conversation changes.
  createEffect(() => {
    const id = activeId();
    setOpenTrace(null);
    // The transient notice belongs to the chat it was raised for.
    showNotice(null);
    setNearBottom(true);
    // Scheduled-run pills are per chat: reload the trigger map for the new one.
    void refreshCronTriggers();
    // Editing a message is scoped to the conversation it lives in; a switch
    // would otherwise let Send target the wrong conversation. Cancelling
    // restores the pre-edit draft, so the single composer draft then simply
    // follows whichever chat is open — it is NOT keyed per conversation, and a
    // half-typed draft is not restored when the user comes back to this chat.
    if (untrack(editingId) !== null) cancelEditMessage();
    // The "no conversation open" error bucket is a one-screen message (e.g. a
    // failed attachment pick); drop it as soon as a chat is opened so it cannot
    // resurface on the next empty screen.
    clearStreamError("");
    // Opening a conversation counts as reading its background output.
    if (id) clearUnseen(id);
    if (!id) {
      setMessages([]);
      return;
    }
    // One implementation of "load this chat's rows and never clobber a live
    // turn" (re-attaches the in-flight assistant row when there is one).
    void reloadTranscript(id);
  });

  // Keep the newest message in view unless the user has scrolled up or is
  // actively dragging. Without the gesture guard a streamed delta arriving
  // mid-drag re-pins to the bottom and fights the user's scroll-up.
  let userInteracting = false;
  let lastScrollTop: number | null = null;
  let lastScrollHeight = 0;
  createEffect(() => {
    messages();
    liveTools();
    if (userInteracting || !nearBottom()) return;
    const el = messagesScrollEl;
    if (el) el.scrollTop = el.scrollHeight;
  });

  // `nearBottom` gates auto-follow. A scroll event must never *re-enable* it the
  // moment the user starts scrolling up: the wheel handler clears it on the first
  // wheel-up, then the arriving scroll event recomputed `dist <= 80` and set it
  // back, so the next streamed delta re-pinned to the bottom — visibly jittering
  // for the first ~80px of every scroll-up attempt. Only an actual scroll *down*
  // toward the bottom edge re-engages auto-follow; `shrank` covers the case where
  // content collapsed under a pinned viewport and the browser clamped scrollTop
  // down — the user is still at the bottom, so auto-follow must stay engaged.
  const updateNearBottom = (el: HTMLElement) => {
    const prev = lastScrollTop;
    lastScrollTop = el.scrollTop;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    const shrank = el.scrollHeight < lastScrollHeight;
    lastScrollHeight = el.scrollHeight;
    if (dist > 80) {
      setNearBottom(false);
      return;
    }
    setNearBottom(shrank || prev == null || el.scrollTop >= prev);
  };

  // Mobile keyboard: when the lift grows while the transcript is pinned to the
  // bottom, keep the newest message above the composer. Use `keyboardLift()`,
  // not `keyboardInset()`: on Android the latter is 0 by design (native insets
  // own the IME), so keying on it would never re-pin the newest message there.
  // Deferred one frame because the padding change must lay out first.
  // Deliberately does nothing while the user is reading older messages.
  let lastKeyboardLift = 0;
  createEffect(() => {
    const lift = keyboardLift();
    const grew = lift > lastKeyboardLift;
    lastKeyboardLift = lift;
    if (!grew || !nearBottom()) return;
    requestAnimationFrame(() => {
      const el = messagesScrollEl;
      if (el) el.scrollTop = el.scrollHeight;
    });
  });

  // A new chat starts empty and unnamed ("New chat"). The title is generated
  // from the first message the user sends (see `maybeAutoTitle`); there is no
  // naming dialog any more.
  async function createNewChat() {
    const created = await createConversation();
    setConversations([created, ...conversations()]);
    setVisibleConversations([created, ...visibleConversations()]);
    setActiveId(created.id);
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
    // A deleted chat can't have unread output any more (it would keep the
    // hamburger dot lit forever).
    clearUnseen(id);
    // Drop its error banner too: the id can never be active again, but leaving
    // the entry would leak one string per deleted chat for the session.
    clearStreamError(id);
    await refreshConversations();
    // Its cron jobs cascade away with the conversation row: refresh so a ghost
    // job disappears from the (possibly open) Cron dialog and sidebar glyphs.
    void refreshCrons();
    void refreshCronTriggers();
    if (activeId() === id) {
      const next =
        conversations().find((c) => c.kind !== "scheduled") ?? conversations()[0];
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
      // Opening Settings must not clobber the open conversation's effective
      // model/level: the dialog edits the DEVICE defaults, while the header
      // keeps showing what this chat actually runs with.
      const conv = activeConversation();
      setBaseUrl(cfg.baseUrl);
      setGlobalModel(cfg.model);
      const convModel = conv?.model?.trim();
      setModel(convModel ? convModel : cfg.model);
      setPreferences(cfg.preferences ?? "");
      setGlobalThinkingLevel(cfg.thinkingLevel ?? "");
      const convLevel = conv?.thinking_level;
      if (convLevel != null) setThinkingLevel(convLevel);
      else setThinkingLevel(cfg.thinkingLevel ?? "");
      setEchoReasoning(cfg.echoReasoningContent ?? true);
      setAutoApproveTools(cfg.autoApproveTools ?? false);
      setTitleModel(cfg.titleModel ?? "");
      setCronEnabled(cfg.cronEnabled ?? true);
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
      thinkingLevel: globalThinkingLevel(),
      echoReasoningContent: echoReasoning(),
      autoApproveTools: autoApproveTools(),
      titleModel: titleModel(),
      cronEnabled: cronEnabled(),
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
    // Skills may have been added/edited while Settings was open (the editor
    // writes the skills directory): re-read them for the slash menu.
    void loadSkills();
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
    // The picker also persists this as the device default, so a chat that
    // inherits it (e.g. a scheduled thread) resolves to the same model.
    setGlobalModel(id);
    // Adopt the new model's thinking levels; drop a level it doesn't accept.
    const opts = await thinkingOptions(id, headerProviderId()).catch(() => null);
    setThinkingOpts(opts);
    const level = thinkingLevel();
    if (opts && level && !opts.options.includes(level)) {
      // The level is not merely wrong for this chat: this model cannot run it at
      // all, so reset the device default as well (otherwise clearing only the
      // per-chat override would just fall back to the same unsupported level)
      // and clear the chat's override so it inherits the new "no reasoning".
      setGlobalThinkingLevel("");
      setThinkingLevel("");
      const conv = activeConversation();
      if (conv) {
        try {
          // `null` clears the per-chat override (inherit). An empty string is a
          // real override meaning "no reasoning" now, so it must not be used to
          // mean "clear". Only write when there is an override to clear: a no-op
          // update would still bump `updated_at` and reorder the sidebar.
          if (conv.thinking_level != null) {
            await setConversationThinkingLevel(conv.id, null);
          }
          setConversations((prev) =>
            prev.map((c) => (c.id === conv.id ? { ...c, thinking_level: null } : c)),
          );
          setVisibleConversations((prev) =>
            prev.map((c) => (c.id === conv.id ? { ...c, thinking_level: null } : c)),
          );
        } catch {
          // best-effort: the next switch re-reads the row
        }
      }
    }
    try {
      await setConfig({
        baseUrl: baseUrl(),
        model: id,
        preferences: preferences(),
        thinkingLevel: globalThinkingLevel(),
        echoReasoningContent: echoReasoning(),
        autoApproveTools: autoApproveTools(),
        titleModel: titleModel(),
        cronEnabled: cronEnabled(),
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
      if (conv) {
        await setConversationModel(conv.id, id);
        setConversations((prev) =>
          prev.map((c) => (c.id === conv.id ? { ...c, model: id } : c)),
        );
        setVisibleConversations((prev) =>
          prev.map((c) => (c.id === conv.id ? { ...c, model: id } : c)),
        );
      }
    } catch (e) {
      setSettingsError(String(e));
    }
  }

  async function changeThinking(level: string) {
    setGlobalThinkingLevel(level);
    setThinkingLevel(level);
    // With a conversation open the pick is that chat's own reasoning effort
    // (synced on its row, used by the backend verbatim). The global default is
    // persisted too, so a new chat starts at the level the user just chose.
    //
    // The header's "Default" entry is the empty string: that means "inherit the
    // device default", i.e. CLEAR the per-chat override (`null`). The backend
    // reserves `""` for an explicit "no reasoning" override, so passing it here
    // would pin the chat instead of inheriting.
    const conv = activeConversation();
    const perChat = level === "" ? null : level;
    if (conv) {
      try {
        await setConversationThinkingLevel(conv.id, perChat);
        setConversations((prev) =>
          prev.map((c) => (c.id === conv.id ? { ...c, thinking_level: perChat } : c)),
        );
        setVisibleConversations((prev) =>
          prev.map((c) => (c.id === conv.id ? { ...c, thinking_level: perChat } : c)),
        );
      } catch (e) {
        setSettingsError(String(e));
      }
    }
    try {
      await setConfig({
        baseUrl: baseUrl(),
        // Persist the DEVICE default, not the open chat's override: a reasoning
        // change must not rewrite `cfg.model` to this chat's model (that would
        // silently change what a new chat / scheduled thread inherits).
        model: globalModel() || model(),
        preferences: preferences(),
        thinkingLevel: globalThinkingLevel(),
        echoReasoningContent: echoReasoning(),
        autoApproveTools: autoApproveTools(),
        titleModel: titleModel(),
        cronEnabled: cronEnabled(),
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
      if (activeId() === id) void reloadTranscript(id);
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
  // is toggled in Settings. Also warns when the active conversation already
  // contains image attachments (not just pending) and the model can't handle them.
  const attachmentWarning = createMemo<string | null>(() => {
    if (currentModelVision() !== false) return null;
    const pendingImages = pendingAttachments().filter((a) => a.kind === "image").length;
    if (pendingImages > 0) {
      return `${model()} isn't marked as accepting image input, so ${
        pendingImages === 1 ? "this image" : "these images"
      } may be ignored or rejected. Pick a vision model in the header.`;
    }
    const convoHasImages = messages().some((m) =>
      parseAttachments(m).some((a) => a.kind === "image"),
    );
    if (!convoHasImages) return null;
    return `${model()} isn't marked as accepting image input, so images in this conversation may be ignored or rejected. Pick a vision model in the header.`;
  });

  // Resolve picked paths into attachments. On Android the dialog plugin returns
  // `content://` URIs, which the path-based `readAttachments` (std::fs) cannot
  // open — so read each through the native ContentResolver bridge instead.
  // Desktop (and any plain filesystem path) keeps the single batched
  // `readAttachments` call unchanged.
  async function readPickedAttachments(paths: string[]): Promise<Attachment[]> {
    if (!isAndroidDevice() && !paths.some((p) => p.startsWith("content://"))) {
      return readAttachments(paths);
    }
    const atts: Attachment[] = [];
    for (const p of paths) {
      // One unreadable item must not drop the whole selection: keep the
      // survivors, matching the desktop `readAttachments` per-file behavior.
      try {
        if (isAndroidDevice() || p.startsWith("content://")) {
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
    // Resolve the chat the pick was made in BEFORE any await: switching chats
    // while the native dialog / URI read is in flight must not file the failure
    // under the conversation the user moved to.
    const id = activeId();
    try {
      const atts = await readPickedAttachments(paths);
      if (atts.length === 0) {
        // Every item failed (e.g. an unreadable `content://` URI). Surface it
        // instead of silently doing nothing. Scoped to the chat the pick was made
        // in, so it doesn't follow the user into another conversation.
        setStreamErrorFor(
          id,
          paths.length === 1 ? "Could not read the selected file." : "Could not read the selected files.",
        );
        return;
      }
      setPendingAttachments((prev) => [...prev, ...atts]);
    } catch (e) {
      setStreamErrorFor(id, String(e));
    }
  }

  // Attachments with a retry in flight. Each upload mints a fresh object key, so
  // a double click would upload the file twice and orphan the first object.
  const retryingAttachments = new Set<string>();
  const retryKey = (a: Attachment, messageId?: string | null) =>
    messageId ? `message:${messageId}:${a.id}` : `pending:${a.id}`;
  /** Whether a staged attachment is waiting on an upload retry (blocks `send`). */
  const attachmentsAreRetrying = (list: Attachment[]) =>
    list.some((a) => retryingAttachments.has(`pending:${a.id}`));
  /**
   * Refuse a send while a staged attachment is retrying.
   *
   * The send snapshots the attachment list, so persisting mid-retry would freeze
   * the stale `uploadError` on the row while the upload lands (an orphan object
   * and a model that is told the file is unreadable). Returns `true` when the
   * send must not proceed — and says so, because a dead Send button with no
   * explanation reads as a bug.
   */
  const sendBlockedByRetry = (list: Attachment[], id: string | null) => {
    if (!attachmentsAreRetrying(list)) return false;
    setStreamErrorFor(id, "Waiting for the attachment upload to finish — try again in a moment.");
    return true;
  };
  // The banner text a retry set, per attachment. The stream-error banner is the
  // only carrier of a turn's actionable error text, so a successful retry may
  // clear it only when it is the very message that retry wrote.
  const retryBanner = new Map<string, string>();
  const reportRetryFailure = (key: string, id: string | null, message: string) => {
    retryBanner.set(key, message);
    setStreamErrorFor(id, message);
  };
  const clearOwnRetryBanner = (key: string, id: string | null) => {
    const mine = retryBanner.get(key);
    retryBanner.delete(key);
    if (mine === undefined) return;
    // Identical reasons are common (two same-size files over the cap, or both
    // "cloud sync is off"), so a text match alone could clear a banner that still
    // describes another attachment's live failure.
    for (const other of retryBanner.values()) if (other === mine) return;
    if ((streamErrors()[id ?? ""] ?? null) === mine) clearStreamError(id);
  };

  // Retry a failed remote offload from the composer's attachment chip.
  async function retryPendingAttachment(a: Attachment) {
    const id = activeId();
    const key = retryKey(a);
    if (retryingAttachments.has(key)) return;
    retryingAttachments.add(key);
    try {
      const updated = await retryAttachmentUpload(a);
      setPendingAttachments((prev) =>
        prev.map((x) => (x.id === updated.id ? updated : x)),
      );
      if (updated.uploadError) {
        reportRetryFailure(key, id, `Upload failed: ${updated.uploadError}`);
      } else {
        clearOwnRetryBanner(key, id);
      }
    } catch (e) {
      reportRetryFailure(key, id, String(e));
    } finally {
      retryingAttachments.delete(key);
    }
  }

  // Retry a failed offload from a persisted user message. The backend rewrites
  // the row's attachment JSON on success, so reload through `reloadTranscript`:
  // a plain `listMessages().then(setMessages)` would replace the transcript with
  // the persisted rows and drop the live assistant row (the retry chip is
  // rendered the moment the user row exists, i.e. while the reply still streams).
  async function retryMessageAttachment(messageId: string, a: Attachment) {
    const id = activeId();
    // A staged `tmp-user-*` row has no DB row to write back to yet: the upload
    // would land, the merge would fail, and a second click would mint a second
    // object. The chip is hidden for those rows too, so this is belt-and-braces.
    if (messageId.startsWith("tmp-")) return;
    // Keyed by message + attachment so a repeated click on the same chip is
    // ignored. Two *different* attachments of one message may retry at once —
    // `db::merge_message_attachment` takes the connection lock once for the whole
    // read-modify-write, so neither can lose the other's update.
    const key = retryKey(a, messageId);
    if (retryingAttachments.has(key)) return;
    retryingAttachments.add(key);
    try {
      const updated = await retryAttachmentUpload(a, messageId);
      if (updated.uploadError) {
        reportRetryFailure(key, id, `Upload failed: ${updated.uploadError}`);
      } else {
        clearOwnRetryBanner(key, id);
      }
      if (id) await reloadTranscript(id);
    } catch (e) {
      reportRetryFailure(key, id, String(e));
    } finally {
      retryingAttachments.delete(key);
    }
  }

  // Attachment pickers. The composer offers File / Photo / Camera; File is the
  // original picker (existing `onPickAttachments` prop). Android's native bridge
  // takes the kind; on desktop Camera degrades to the image filter.
  async function pickAttachmentsKind(kind: "any" | "image" | "camera") {
    // Same reason as `addAttachmentPaths`: capture the chat before the dialog.
    const id = activeId();
    try {
      const paths = await pickAttachmentPaths(kind);
      if (!paths) return;
      await addAttachmentPaths(paths);
    } catch (e) {
      setStreamErrorFor(id, String(e));
    }
  }

  // File explorer button: the ordinary picker (any file type).
  const pickAttachments = () => pickAttachmentsKind("any");

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
    addStreaming(id);
    // A new turn in this conversation supersedes any earlier failure shown for
    // it. Other conversations keep their own banners.
    clearStreamError(id);
    setOpenTrace(null);

    const token = (turnTokens[id] = (turnTokens[id] ?? 0) + 1);
    setLiveTurn(id, {
      token,
      assistantKey,
      startedAt: Date.now(),
      content: "",
      thinking: "",
      liveTools: [],
    });

    const received = { current: false };
    const channel = new Channel<StreamEvent>();
    channel.onmessage = (ev) => {
      // Ignore events from a superseded turn (Stop, then a new send).
      if (turnTokens[id] !== token) return;
      // `done` is the ONLY terminal event. `error` is a mid-turn report: the
      // backend emits it and then finishes the turn normally (persists the
      // partial assistant row and emits `done`), so treating it as terminal
      // dropped the live record and reloaded a transcript that had no answer yet.
      // It must also NOT clear the banner — the following terminal `done` used to
      // do exactly that, which made every failure message flash and vanish.
      if (ev.type === "error") {
        // Keyed to THIS turn's conversation (not the visible one): the failure
        // belongs to the chat it happened in.
        setStreamErrorFor(id, ev.message);
        return;
      }
      if (ev.type === "done") {
        // A turn that ended in `error` keeps its banner (the actionable text is
        // not in the persisted row — that only carries the `(error)` annotation);
        // any other terminal makes an earlier banner for this chat stale.
        if (ev.stopReason !== "error") clearStreamError(id);
        // Settled off-screen: mark the row (and the hamburger) so the user knows
        // there is output to read. OS notifications are the backend's job. A
        // turn that errored before producing anything has nothing to read, so it
        // must not light a "New activity" dot (a partial answer still counts).
        if (activeId() !== id && received.current) markUnseen(id);
        removeStreaming(id);
        dropLiveTurn(id, token);
        // Streaming is already cleared, so this applies the persisted rows (the
        // assistant row carries the error annotation, if any).
        if (activeId() === id) void reloadTranscript(id);
        refreshConversations().catch(() => {});
        // First turn finished: this is the one place auto-naming runs (the user
        // row the backend derived a title from now exists). Pins are disabled
        // mid-stream, but a reload still keeps the strip honest.
        void maybeAutoTitle(id);
        // A live turn may have created / changed / deleted cron jobs through the
        // agent tools (which have no event emitter of their own).
        void refreshCrons();
        void refreshCronTriggers();
        return;
      }
      // Everything below updates the per-conversation live record — which must
      // accumulate REGARDLESS of what is on screen, so a conversation switched
      // away from keeps growing and switching back re-attaches an up-to-date
      // row instead of one frozen at the switch moment. Only the visible
      // transcript mutations (`setMessages`) are gated on the conversation
      // being the one viewed; tool events are recorded the same way, and
      // `turns()` only renders live entries while this chat is visible anyway
      // (the terminal `done` reload then replaces them with the persisted rows).
      if (ev.type === "delta") {
        received.current = true;
        patchLiveTurn(id, (live) => {
          live.content += ev.text;
        });
        if (activeId() === id)
          setMessages((prev) =>
            prev.map((m) =>
              m.id === assistantKey ? { ...m, content: m.content + ev.text } : m,
            ),
          );
      } else if (ev.type === "thinkingDelta") {
        received.current = true;
        patchLiveTurn(id, (live) => {
          live.thinking += ev.text;
        });
        if (activeId() === id)
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
        // call, matching the persisted rows after `done`. The preamble is read
        // from the LIVE RECORD, not from `messages()` (which belongs to whatever
        // conversation is visible — an off-screen turn captures its own).
        const live = liveTurns()[id];
        const thinking = live?.thinking ?? "";
        const preamble = live?.content ?? "";
        const hasPreamble = thinking.trim().length > 0 || preamble.trim().length > 0;
        if (hasPreamble) {
          patchLiveTurn(id, (l) => {
            l.content = "";
            l.thinking = "";
          });
        }
        if (activeId() === id) {
          setMessages((prev) =>
            prev.map((m) =>
              m.id === assistantKey ? { ...m, content: "", thinking: "" } : m,
            ),
          );
        }
        patchLiveTurn(id, (l) => {
          l.liveTools = [
            ...l.liveTools,
            {
              callId: ev.callId,
              name: ev.name,
              arguments: formatToolArgs(ev.arguments),
              gated: ev.gated,
              state: ev.gated ? "pending" : "running",
              thinking: hasPreamble ? thinking : undefined,
              text: hasPreamble ? preamble : undefined,
            },
          ];
        });
      } else if (ev.type === "toolResult") {
        received.current = true;
        patchLiveTurn(id, (l) => {
          l.liveTools = l.liveTools.map((t) =>
            t.callId === ev.callId
              ? { ...t, state: "done", ok: ev.ok, output: ev.output }
              : t,
          );
        });
      }
    };

    try {
      await streamChat(id, text, staged, channel, regenerate);
    } catch (e) {
      if (turnTokens[id] !== token) return;
      if (activeId() === id && !received.current) {
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
      removeStreaming(id);
      dropLiveTurn(id, token);
      setStreamErrorFor(id, String(e));
    }
  }

  async function send() {
    const staged = pendingAttachments();
    let id = activeId();

    if (isBusy(id) || (!draft().trim() && staged.length === 0)) return;
    if (sendBlockedByRetry(staged, id)) return;

    // Editing an existing user row: rewrite it (truncating later messages) and
    // re-run the reply from it, instead of appending a second user turn.
    const editing = editingId();
    if (editing && !id) {
      // An edit always targets a row in an existing chat (switching chats
      // cancels it). With no active conversation there is nothing to rewrite,
      // so restore the pre-edit draft instead of creating an empty chat.
      cancelEditMessage();
      return;
    }
    if (!id) {
      const created = await createConversation();
      setConversations([created, ...conversations()]);
      setVisibleConversations([created, ...visibleConversations()]);
      setActiveId(created.id);
      id = created.id;

    }
    const text = draft().trim();

    // Re-check after the awaits above (`createConversation` can take a moment,
    // and the retry chip stays clickable throughout).
    if (sendBlockedByRetry(staged, id)) return;

    if (editing) {
      try {
        await editUserMessage(id, editing, text, staged.length > 0 ? staged : null);
      } catch (e) {
        // Keep the composer in edit mode with the text intact so the user can
        // retry (or Cancel) instead of losing what they typed. The pre-edit
        // stash stays armed, so Cancel still restores their original draft.
        setStreamErrorFor(id, String(e));
        return;
      }
      // The user deliberately replaced the composer contents with this edit:
      // the pre-edit stash is no longer wanted.
      dropEditStash();
      setEditingId(null);
      setDraft("");
      setPendingAttachments([]);
      // The edit rewrote the row and deleted everything after it; reload so the
      // transcript matches, then stream a fresh assistant reply for the edit.
      // `reloadTranscript` swallows a failed fetch — the stream's `done` reload
      // below still reconciles state.
      await reloadTranscript(id);
      if (activeId() !== id) return;
      // The edited user row already exists — the backend must not insert a
      // second one, so mark this re-run as a regeneration.
      await runAssistantTurn(id, text, staged, null, true);
      return;
    }

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
    // Naming runs from the stream's terminal `done` event, after the backend has
    // persisted the user row. Calling it here (before the row exists) could only
    // ever return "New chat" and would consume the once-per-chat guard.
  }

  async function stop() {
    const id = activeId();
    if (!id || !isBusy(id)) return;
    // Close the abort race exactly as before: keep the live turn (temp row +
    // accumulated content) until the backend's terminal event arrives — its
    // handler drops the record and reloads the persisted rows. The turn token
    // is deliberately NOT superseded, so the terminal still reconciles; a quick
    // re-send bumps the token and the old terminal is ignored.
    try {
      await stopChat(id);
    } catch {
      // noop — the backend still settles the aborted turn.
    }
    removeStreaming(id);
    // A cron run has no stream channel of its own: clear its busy marker
    // optimistically too, so a failed/missed terminal event cannot leave the
    // composer permanently showing Stop. If the run somehow keeps going, the
    // backend refuses the next send with an explicit message.
    setCronRun(id, false);
    // `stop_chat` cancels the exclusive compaction token too; clear the local
    // busy marker so the composer is usable immediately.
    setCompacting(id, false);
    // The user message is persisted by the backend *before* streaming starts, so
    // it is already in the transcript. Handing the draft back here would
    // duplicate it on the next send, so Stop simply returns the composer to its
    // idle state; the message can be edited to re-run the turn.
  }

  // --- Editing a user message in the central composer ---------------------

  /** Load a persisted user message into the composer for editing. The row is
   *  rewritten (and later turns truncated) only when the user sends. */
  function startEditMessage(m: Message) {
    if (isBusy(activeId())) return;
    // Only persisted rows are editable; a `tmp-` row hasn't been written yet.
    if (m.id.startsWith("tmp-")) return;
    // Stash the user's own input exactly once, on the first pen click. Clicking
    // another message's pen while already editing replaces the loaded edit
    // content but must not overwrite the original stash (else Cancel would
    // "restore" the previous edit's text instead of the user's draft).
    if (!preEditStashed) {
      preEditDraft = draft();
      preEditAttachments = pendingAttachments();
      preEditStashed = true;
    }
    setEditingId(m.id);
    setDraft(m.content);
    setPendingAttachments(parseAttachments(m));
    setComposerFocusNonce((n) => n + 1);
  }

  function cancelEditMessage() {
    setEditingId(null);
    // Hand back what the composer held before the pen loaded the message;
    // without a stash (defensive) the composer goes empty as it used to.
    setDraft(preEditStashed ? preEditDraft : "");
    setPendingAttachments(preEditStashed ? preEditAttachments : []);
    dropEditStash();
  }

  function approveLiveTool(callId: string) {
    patchLiveTurn(activeId() ?? "", (l) => {
      l.liveTools = l.liveTools.map((x) => (x.callId === callId ? { ...x, state: "running" } : x));
    });
    approveTool(callId).catch(() => {});
  }

  function denyLiveTool(callId: string) {
    patchLiveTurn(activeId() ?? "", (l) => {
      l.liveTools = l.liveTools.map((x) => (x.callId === callId ? { ...x, state: "running" } : x));
    });
    denyTool(callId).catch(() => {});
  }

  // Group messages into per-turn timelines, augmented with live tool activity.
  // Only show live tools when this conversation is the streaming one.
  const turns = () => groupTurns(messages(), liveTools(), isStreaming(activeId()));

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
      if (key) {
        // Setting a provider key is catalog-relevant: without it list_models
        // errors, so the model picker is empty (or stale) until a restart.
        // Refetch the edited provider's model list right away — same as
        // switching providers in the header — so a new key takes effect
        // without leaving and reopening Settings.
        await loadModels(true, id);
      }
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
    // A scheduled job's thread carries no provider of its own (NULL), and an
    // empty string must not shadow the active provider either — `??` alone
    // treated `""` as a real override and left the header pointing at nothing.
    const convProv = conv?.provider_id;
    const prov = convProv && convProv !== "" ? convProv : ap;
    if (convId === lastSyncId && ap === lastSyncAp) return;
    lastSyncId = convId;
    lastSyncAp = ap;
    setHeaderProviderId(prov);
    // The conversation's own model wins; one without it inherits the device
    // default (`cfg.model`), which is what the backend resolves for a cron
    // thread — so the header must show that, not the previously open chat's
    // model. Read untracked: a global-model change is not a conversation switch.
    const ownModel = conv?.model?.trim();
    const nextModel = ownModel ? ownModel : untrack(globalModel);
    if (nextModel) setModel(nextModel);
    // Same rule for reasoning effort: a non-null level is this chat's own
    // (`""` included — it means "no reasoning"), otherwise fall back to the
    // device default rather than leaving the previous chat's level showing.
    const convLevel = conv?.thinking_level;
    if (convLevel != null) setThinkingLevel(convLevel);
    else setThinkingLevel(untrack(globalThinkingLevel));
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

  // Resolve a reasoning trace by id. A persisted (or current-round) trace lives
  // on a message row; a *flushed* live round's trace lives on its `LiveTool` —
  // its synthetic `live:<callId>:thought` id matches no message, so it must be
  // resolved against `liveTools()`. Returns null when the id no longer names
  // anything (the row was reset by a tool round or replaced by the reload).
  const resolveTrace = (id: string): { text: string; live: boolean } | null => {
    if (id.startsWith("live:")) {
      const rest = id.slice("live:".length);
      const callId = rest.endsWith(":thought") ? rest.slice(0, -":thought".length) : rest;
      const t = liveTools().find((x) => x.callId === callId);
      return t?.thinking?.trim() ? { text: t.thinking, live: true } : null;
    }
    const m = messages().find((x) => x.id === id);
    if (!m?.thinking?.trim()) return null;
    const live = Boolean(
      isStreaming(activeId()) &&
        m.id.startsWith("tmp-assistant-") &&
        !m.content,
    );
    return { text: m.thinking, live };
  };

  // The popup's live text: the resolved trace while its source still exists,
  // otherwise the snapshot taken when it was opened / last resolvable. Never
  // null while a snapshot is held, so an open popup survives the flush and the
  // end-of-stream row swap instead of closing under the user.
  const openThinking = (): { text: string; live: boolean } | null => {
    const snapshot = openTrace();
    if (!snapshot) return null;
    return resolveTrace(snapshot.id) ?? { text: snapshot.text, live: false };
  };

  // Keep the snapshot text fresh while the popup is open (a streamed trace grows
  // until its round flushes), without ever clearing it. Guarded so the write
  // cannot re-trigger itself in a loop.
  createEffect(() => {
    const snapshot = openTrace();
    if (!snapshot) return;
    const resolved = resolveTrace(snapshot.id);
    if (resolved && (resolved.text !== snapshot.text || resolved.live !== snapshot.live)) {
      setOpenTrace({ ...snapshot, text: resolved.text, live: resolved.live });
    }
  });

  /// Open the reasoning popup for `id`, snapshotting `text` as a fallback, or
  /// close it when the same trace is already open.
  function toggleThought(id: string, text: string) {
    if (thinkingOpenId() === id) {
      setOpenTrace(null);
      return;
    }
    const resolved = resolveTrace(id);
    setOpenTrace({ id, text: resolved?.text ?? text, live: resolved?.live ?? false });
  }

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
        cronConversationIds={cronConversationIds()}
        unseenConversationIds={completedUnseen()}
        onTogglePin={toggleChatPinned}
      />

      {/* Main */}
      <main
        class="flex min-w-0 flex-1 flex-col"
        // Mobile only, and ONLY without the native bridge: the soft keyboard's
        // inset raises the composer and shrinks the transcript. Android must not
        // pad here too — its IME inset already arrives in `insets().bottom` and
        // pads the app root. Adding both lifts the composer twice as far and
        // shrinks the transcript twice (the old inconsistent keyboard behaviour).
        style={
          !hasNativeInsets() && keyboardInset() > 0
            ? { "padding-bottom": `${keyboardInset()}px` }
            : undefined
        }
      >
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
          openCron={openCron}
          cronUnseen={cronUnseen()}
          cronHasJobs={cronJobs().length > 0}
          hasUnseen={completedUnseen().size > 0}
        />

        <Show when={settingsError() && !settingsOpen()}>
          <p class="border-b border-neutral-200 bg-red-50 px-6 py-2 text-sm text-red-700 dark:border-neutral-800 dark:bg-red-950/40 dark:text-red-400">
            {settingsError()}
          </p>
        </Show>

        <section
          ref={(el) => (messagesScrollEl = el)}
          onScroll={(e) => updateNearBottom(e.currentTarget)}
          onTouchStart={() => (userInteracting = true)}
          onTouchCancel={() => (userInteracting = false)}
          onTouchEnd={(e) => {
            updateNearBottom(e.currentTarget);
            userInteracting = false;
          }}
          onWheel={(e) => {
            // Wheel-up means the user is deliberately leaving the bottom; stop
            // auto-following immediately instead of waiting for onScroll.
            if (e.deltaY < 0) setNearBottom(false);
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
              streaming={isBusy(activeId())}
              thinkingOpenId={thinkingOpenId()}
              onToggleThought={toggleThought}
              onApprove={approveLiveTool}
              onDeny={denyLiveTool}
              onStartEdit={startEditMessage}
              editingId={editingId()}
              onRetryAttachment={(m, a) => retryMessageAttachment(m.id, a)}
              cronTriggers={cronTriggers()}
            />
          </Show>

          <Show when={streamError()}>
            <p class="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-400">
              {streamError()}
            </p>
          </Show>
        </section>

        {/* Transient status for `/compact` etc. Non-blocking; auto-clears. */}
        <Show when={notice()}>
          <p
            aria-live="polite"
            class="border-t border-neutral-200 px-6 py-2 text-xs text-neutral-500 dark:border-neutral-800 dark:text-neutral-400"
          >
            {notice()}
          </p>
        </Show>

        <Composer
          pendingAttachments={pendingAttachments()}
          onRemoveAttachment={removeAttachment}
          contextNote={contextNote()}
          attachmentWarning={attachmentWarning()}
          streaming={isBusy(activeId())}
          busy={isBusy(activeId())}
          onPickAttachments={pickAttachments}
          onRetryAttachment={retryPendingAttachment}
          draft={draft()}
          setDraft={setDraft}
          onSend={send}
          onStop={stop}
          editing={editingId() !== null}
          onCancelEdit={cancelEditMessage}
          focusNonce={composerFocusNonce()}
          canCamera={isAndroidDevice()}
          yolo={effectiveChatYolo()}
          yoloInherited={chatYoloInherited()}
          onToggleYolo={toggleChatYolo}
          onClearYolo={clearChatYolo}
          onPickKind={pickAttachmentsKind}
          slashItems={slashItems()}
          onRunCompact={runCompact}
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
        cronEnabled={cronEnabled()}
        setCronEnabled={setCronEnabled}
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
        titleModel={titleModel()}
        setTitleModel={setTitleModel}
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

      <CronPage
        cronOpen={cronOpen()}
        setCronOpen={setCronOpen}
        wideViewport={wideViewport()}
        fullscreenInsetStyle={fullscreenInsetStyle}
        cronJobs={cronJobs()}
        cronRuns={cronRuns()}
        loadCronRuns={loadCronRuns}
        cronSelectedId={cronSelectedId()}
        setCronSelectedId={setCronSelectedId}
        cronBusy={cronBusy()}
        cronError={cronError()}
        setCronError={setCronError}
        onCreateCron={createCronJob}
        onUpdateCron={patchCron}
        onDeleteCron={deleteCronJob}
        onRunCronNow={runCronJobNow}
        onOpenThread={(id) => {
          // The job's thread is a normal conversation (kind `scheduled`), so
          // opening it just activates it; the sidebar keeps it in its own
          // section rather than mixing it with chats.
          setCronOpen(false);
          setActiveId(id);
          setSidebarOpen(false);
        }}
      />

      {/* Expanded transcript image (clicked in MessageList) */}
      <ImageLightbox />

      {/* Reasoning trace popup */}
      <Show when={openThinking()}>
        <div
          class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-6"
          onClick={() => setOpenTrace(null)}
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
                onClick={() => setOpenTrace(null)}
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