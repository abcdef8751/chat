import { invoke, Channel } from "@tauri-apps/api/core";

export interface Conversation {
  id: string;
  title: string;
  /** `chat` (user-owned) or `scheduled` (a cron job's own thread). */
  kind: "chat" | "scheduled";
  model: string | null;
  provider_id: string | null;
  system_prompt: string | null;
  compaction_summary: string | null;
  /** Id of the newest message folded into `compaction_summary` (local-only). */
  compaction_through: string | null;
  created_at: number;
  updated_at: number;
}

export interface Message {
  id: string;
  conversation_id: string;
  role: string;
  index: number;
  content: string;
  model: string | null;
  provider: string | null;
  thinking_level: string | null;
  thinking: string | null;
  usage: string | null;
  stop_reason: string | null;
  attachments: string | null;
  created_at: number;
}

export interface Attachment {
  id: string;
  name: string;
  mime: string;
  size: number;
  kind: "image" | "file";
  dataUrl: string | null;
  text: string | null;
}

export interface ModelOverride {
  contextWindow: number | null;
  inputPerMillion: number | null;
  outputPerMillion: number | null;
  cacheReadPerMillion: number | null;
  cacheWritePerMillion: number | null;
  /**
   * User-set input modalities (models.dev vocabulary, e.g. `["text","image"]`).
   * Beats models.dev; the only way to declare capability for a custom provider,
   * which has no catalog entry. `null` = fall through.
   */
  inputModalities: string[] | null;
  /** User-set output modalities (e.g. `["text"]`). `null` = fall through. */
  outputModalities: string[] | null;
  /** User-set reasoning support. `null` = fall through. */
  reasoning: boolean | null;
}

export interface Provider {
  id: string;
  name: string;
  baseUrl: string;
  defaultModel?: string | null;
  /** Set only when added via the catalog picker (models.dev is then used). */
  catalogId?: string | null;
}

/** A provider plus its live key status and active flag (for pickers/manager). */
export interface ProviderInfo {
  id: string;
  name: string;
  baseUrl: string;
  defaultModel: string | null;
  /** The models.dev provider id, set only when added via the catalog picker. */
  catalogId: string | null;
  hasKey: boolean;
  active: boolean;
}

/** A provider entry from the models.dev catalog shown in "Add provider". */
export interface ModelsDevProvider {
  id: string;
  name: string;
  baseUrl: string;
}

export interface Config {
  baseUrl: string;
  model: string;
  /** All configured providers. Empty on legacy configs (single provider). */
  providers: Provider[];
  /** The provider new chats default to. */
  activeProviderId: string;
  preferences: string;
  thinkingLevel: string;
  echoReasoningContent: boolean;
  memoryReflectionEnabled: boolean;
  memoryReflectionIdleMinutes: number;
  /** Opt-in "Cloud backup & sync": OFF by default, so the app is local-only. */
  syncEnabled: boolean;
  modelOverrides: Record<string, ModelOverride>;
  /**
   * Working directory for host shell tools (Android: a directory under shared
   * storage, e.g. `/storage/emulated/0/PiChat`). Empty = platform default.
   */
  shellWorkspaceDir: string;
  /**
   * "YOLO mode": auto-approve every gated tool call instead of waiting for the
   * user to approve/deny each one. Off by default (local-only; not synced).
   * The backend always serializes this; optional here only to avoid churning
   * every existing `setConfig` call site.
   */
  autoApproveTools?: boolean;
  /**
   * Global kill-switch for the cron scheduler ("Scheduled jobs"). When off,
   * no job runs. On by default; optional here only to avoid churning every
   * existing `setConfig` call site.
   */
  cronEnabled?: boolean;
}

export interface Pricing {
  modelId: string;
  contextWindow: number;
  inputPerMillion: number | null;
  outputPerMillion: number | null;
  cacheReadPerMillion: number | null;
  cacheWritePerMillion: number | null;
  overridden: boolean;
  source: "override" | "fetched" | "bundled" | "default";
}

export interface RefreshResult {
  count: number;
  provider: string;
  fetchedAt: number;
}

export interface ModelInfo {
  id: string;
  name: string | null;
  /**
   * Whether the model can accept image input, resolved from the user's override
   * (override → models.dev → text-only, so always set in practice). The composer
   * re-resolves it client-side from the raw fields below — see
   * `src/lib/capabilities.ts` — because it is baked at list-fetch time and would go
   * stale when a capability is toggled in Settings.
   */
  vision: boolean | null;
  /**
   * models.dev `reasoning` flag; `null`/absent when the provider isn't in the
   * catalog. The backend always serializes these three; optional here only to
   * avoid churning every existing `ModelInfo` construction.
   */
  reasoning?: boolean | null;
  /**
   * models.dev coarse `attachment` flag (documents + images). Prefer
   * `modalities` (input list) when present; this is the fallback.
   */
  attachment?: boolean | null;
  /**
   * Raw models.dev `modalities` JSON, e.g.
   * `{"input":["text","image"],"output":["text"]}`. `null` when unknown.
   */
  modalities?: string | null;
}

export interface ThinkingOptions {
  options: string[];
  source: "models.dev" | "openai" | "none";
  supportsReasoning: boolean;
}

export interface MemoryFile {
  name: string;
  content: string;
  core: boolean;
}

export interface ReflectionStats {
  count: number;
  cost: number;
  lastAt: number;
}

/** Staged vs. still-pending memory extractions (the backfill's map phase). */
export interface ExtractionStats {
  staged: number;
  pending: number;
}

export interface BackfillStatus {
  running: boolean;
  phase: string;
  total: number;
  done: number;
  failed: number;
  /** In-flight passes allowed right now; AIMD moves this during the run. */
  concurrency: number;
  /** The most recent failure, so a run that ends with failures is diagnosable. */
  lastError: string | null;
}

/** Result of importing an Anthropic-format export. */
export interface ImportReport {
  conversations: number;
  messages: number;
  skipped: number;
}

/** Live state of the opt-in Supabase backup + sync feature. */
export interface SyncStatus {
  enabled: boolean;
  loggedIn: boolean;
  email: string | null;
  syncing: boolean;
  pending: number;
  lastSyncAt: number | null; // ms epoch or null
  lastError: string | null;
  phase: string; // "push" | "pull" | "" when idle
  pushed: number;
  pulled: number;
  pullTotal: number; // remote rows this pull must fetch (0 = unknown/indeterminate)
  encryption: boolean; // client-side encryption configured
  locked: boolean; // remote is encrypted but this device has no key yet
}

export type StreamEvent =
  | { type: "delta"; text: string }
  | { type: "thinkingDelta"; text: string }
  | { type: "toolCall"; callId: string; name: string; arguments: string; gated: boolean }
  | { type: "toolResult"; callId: string; name: string; ok: boolean; output: string }
  | { type: "done"; stopReason: string }
  | { type: "error"; message: string };

export function listConversations(): Promise<Conversation[]> {
  return invoke<Conversation[]>("list_conversations");
}

export function createConversation(title?: string): Promise<Conversation> {
  return invoke<Conversation>("create_conversation", { title: title ?? null });
}

export function listMessages(conversationId: string): Promise<Message[]> {
  return invoke<Message[]>("list_messages", { conversationId });
}

export function addMessage(payload: {
  conversationId: string;
  role: string;
  content: string;
  model?: string | null;
  provider?: string | null;
  thinkingLevel?: string | null;
  thinking?: string | null;
  usage?: string | null;
  stopReason?: string | null;
}): Promise<Message> {
  return invoke<Message>("add_message", payload);
}

/**
 * Edit a user message, then discard every message after it in that
 * conversation so the turn can be re-run from the edit point. The backend
 * updates the row, deletes later rows by `index`, and reconciles the
 * compaction/reflection watermarks. The caller re-runs `streamChat` afterwards.
 */
export function editUserMessage(
  conversationId: string,
  messageId: string,
  content: string,
  attachments: Attachment[] | null,
): Promise<void> {
  return invoke<void>("edit_user_message", {
    conversationId,
    messageId,
    content,
    attachments,
  });
}

export function renameConversation(conversationId: string, title: string): Promise<void> {
  return invoke<void>("rename_conversation", { conversationId, title });
}

export function setConversationProvider(
  conversationId: string,
  providerId: string | null,
): Promise<void> {
  return invoke<void>("set_conversation_provider", {
    conversationId,
    providerId: providerId ?? null,
  });
}

export function setConversationModel(conversationId: string, model: string): Promise<void> {
  return invoke<void>("set_conversation_model", { conversationId, model });
}

// --- Multi-provider management ---

export function listProviders(): Promise<ProviderInfo[]> {
  return invoke<ProviderInfo[]>("list_providers");
}

export function addProvider(
  name: string,
  baseUrl: string,
  apiKey?: string | null,
  catalogId?: string | null,
): Promise<ProviderInfo> {
  return invoke<ProviderInfo>("add_provider", {
    name,
    baseUrl,
    apiKey: apiKey ?? null,
    catalogId: catalogId ?? null,
  });
}

export function updateProvider(
  id: string,
  name?: string | null,
  baseUrl?: string | null,
  apiKey?: string | null,
): Promise<ProviderInfo> {
  return invoke<ProviderInfo>("update_provider", {
    id,
    name: name ?? null,
    baseUrl: baseUrl ?? null,
    apiKey: apiKey === undefined ? null : apiKey,
  });
}

export function removeProvider(id: string): Promise<ProviderInfo[]> {
  return invoke<ProviderInfo[]>("remove_provider", { id });
}

export function setActiveProvider(id: string): Promise<ProviderInfo> {
  return invoke<ProviderInfo>("set_active_provider", { id });
}

/** Providers offered in the "Add provider" picker, from the models.dev catalog. */
export function listModelsDevProviders(): Promise<ModelsDevProvider[]> {
  return invoke<ModelsDevProvider[]>("list_models_dev_providers");
}

export function deleteConversation(conversationId: string): Promise<void> {
  return invoke<void>("delete_conversation", { conversationId });
}

export function searchConversations(query: string): Promise<Conversation[]> {
  return invoke<Conversation[]>("search_conversations", { query });
}

export function getConfig(): Promise<Config> {
  return invoke<Config>("get_config");
}

export function setConfig(config: Config): Promise<void> {
  return invoke<void>("set_config", { config });
}

/** Replace only the per-model pricing/context overrides (no other config field). */
export function setModelOverrides(
  modelOverrides: Record<string, ModelOverride>,
): Promise<void> {
  return invoke<void>("set_model_overrides", { modelOverrides });
}

export function hasApiKey(): Promise<boolean> {
  return invoke<boolean>("has_api_key");
}

export function setApiKey(apiKey: string | null): Promise<void> {
  return invoke<void>("set_api_key", { apiKey: apiKey ?? null });
}

/** Whether a Brave Search API key is stored (enables the native web tools). */
export function hasBraveKey(): Promise<boolean> {
  return invoke<boolean>("has_brave_key");
}

/** Set (or clear, with null) the Brave Search API key in the OS keychain. */
export function setBraveKey(braveKey: string | null): Promise<void> {
  return invoke<void>("set_brave_key", { braveKey: braveKey ?? null });
}

export function listModels(refresh?: boolean, providerId?: string | null): Promise<ModelInfo[]> {
  return invoke<ModelInfo[]>("list_models", {
    refresh: refresh ?? null,
    providerId: providerId ?? null,
  });
}

/** Resolve pricing/context for a model against the provider that owns it. Pass
 * `providerId` (the header/conversation provider) so a model is looked up in the
 * right provider's cache rather than the active provider's — otherwise a model
 * from a non-active provider misses its fetched context and falls back to a
 * bundled family guess. */
export function getPricing(modelId: string, providerId?: string | null): Promise<Pricing> {
  return invoke<Pricing>("get_pricing", { modelId, providerId: providerId ?? null });
}

export function thinkingOptions(
  modelId: string,
  providerId?: string | null,
): Promise<ThinkingOptions> {
  return invoke<ThinkingOptions>("thinking_options", {
    modelId,
    providerId: providerId ?? null,
  });
}

export function refreshPricing(): Promise<RefreshResult> {
  return invoke<RefreshResult>("refresh_pricing");
}

export function streamChat(
  conversationId: string,
  content: string,
  attachments: Attachment[],
  channel: Channel<StreamEvent>,
  /**
   * `true` when the user message already exists in the transcript (e.g. after
   * `editUserMessage`, which keeps the edited row): the backend builds the turn
   * from `content` but must NOT persist a second `user` row. Defaults to
   * `false` for a normal send.
   */
  regenerate = false,
): Promise<void> {
  return invoke<void>("stream_chat", {
    conversationId,
    content,
    attachments: attachments.length > 0 ? attachments : null,
    channel,
    regenerate,
  });
}

export function readAttachments(paths: string[]): Promise<Attachment[]> {
  return invoke<Attachment[]>("read_attachments", { paths });
}

/**
 * Open the native file picker. **Android only** — desktop uses the dialog
 * plugin's `open()` instead. Returns `content://` URIs, which the content-URI
 * reader (`readAttachmentFromUri`) already handles. An empty array means the
 * user cancelled.
 */
export function pickAttachmentUris(): Promise<string[]> {
  return invoke<string[]>("pick_attachment_uris");
}

export function stopChat(conversationId: string): Promise<void> {
  return invoke<void>("stop_chat", { conversationId });
}

export function approveTool(callId: string): Promise<void> {
  return invoke<void>("approve_tool", { callId });
}

export function denyTool(callId: string): Promise<void> {
  return invoke<void>("deny_tool", { callId });
}

export function listMemoryFiles(): Promise<MemoryFile[]> {
  return invoke<MemoryFile[]>("list_memory_files");
}

export function writeMemoryFile(name: string, content: string): Promise<void> {
  return invoke<void>("write_memory_file", { name, content });
}

export function deleteMemoryFile(name: string): Promise<void> {
  return invoke<void>("delete_memory_file", { name });
}

export function memoryReflectionStats(): Promise<ReflectionStats> {
  return invoke<ReflectionStats>("memory_reflection_stats");
}

export function reflectNow(conversationId: string): Promise<void> {
  return invoke<void>("reflect_now", { conversationId });
}

/**
 * Extract durable facts from every conversation that needs it, in parallel, then
 * consolidate the staged summaries into memory in one serial pass. Returns
 * immediately; progress arrives on the `memory-backfill` event.
 *
 * `model` / `thinkingLevel` optionally override which model (and reasoning
 * level) the phases run on; empty/`null` means the active provider's default.
 * `allHistory` switches the map phase to every pending conversation instead of
 * just the newest import batch.
 */
export function backfillMemories(
  opts: {
    minChars?: number;
    concurrency?: number;
    model?: string | null;
    thinkingLevel?: string | null;
    allHistory?: boolean;
    force?: boolean;
  } = {},
): Promise<BackfillStatus> {
  return invoke<BackfillStatus>("backfill_memories", {
    minChars: opts.minChars ?? null,
    concurrency: opts.concurrency ?? null,
    model: opts.model ?? null,
    thinkingLevel: opts.thinkingLevel ?? null,
    allHistory: opts.allHistory ?? null,
    force: opts.force ?? null,
  });
}

export function backfillStatus(): Promise<BackfillStatus> {
  return invoke<BackfillStatus>("backfill_status");
}

export function cancelBackfill(): Promise<void> {
  return invoke<void>("cancel_backfill");
}

export function memoryExtractionStats(): Promise<ExtractionStats> {
  return invoke<ExtractionStats>("memory_extraction_stats");
}

export function clearExtractions(): Promise<number> {
  return invoke<number>("clear_extractions");
}

/**
 * Import an Anthropic-format export: a `conversations.json` holding an array of
 * conversations. Imported chats keep their original timestamps and are excluded
 * from the idle reflection sweep; the backfill picks them up instead.
 */
export function importConversations(path: string): Promise<ImportReport> {
  return invoke<ImportReport>("import_conversations", { path });
}

// --- Cloud backup + sync (opt-in, local-first) ---

export function syncSignIn(email: string, password: string): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_sign_in", { email, password });
}

/**
 * Create a new Supabase account. Returns the resulting status: if email
 * confirmation is off (current setup) the user is signed in immediately; if it's
 * on, `loggedIn` is false and a confirmation link was emailed.
 */
export function syncSignUp(email: string, password: string): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_sign_up", { email, password });
}

export function syncSignOut(): Promise<void> {
  return invoke<void>("sync_sign_out");
}

export function syncStatus(): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_status");
}

export function syncToggle(on: boolean): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_toggle", { on });
}

export function syncNow(): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_now");
}

/**
 * Enable client-side encryption: generates a random key in the OS keychain and
 * re-uploads all rows encrypted. No passphrase. The data sent to Supabase is
 * ciphertext from then on.
 */
export function syncSetEncryption(): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_set_encryption");
}

/** Import a recovery code (base64 key) to unlock this device. */
export function syncImportKey(code: string): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_import_key", { code });
}

/** The current key as a portable recovery code ("" when encryption is off). */
export function syncRecoveryCode(): Promise<string> {
  return invoke<string>("sync_recovery_code");
}

/** Disable client-side encryption and re-upload rows as plaintext. */
export function syncRemoveEncryption(): Promise<SyncStatus> {
  return invoke<SyncStatus>("sync_remove_encryption");
}

// --- Scheduled jobs (cron) ---

/** How much of a job's own thread each run replays. */
export type CronContext = "fresh" | "recent" | "full";

/** A scheduled run: carries out `prompt` in its own thread (`conversationId`)
 * on `schedule`. The thread is created with the job, not picked from chats. */
export interface CronJob {
  id: string;
  conversationId: string;
  title: string;
  /** Normalized 5-field cron expression. */
  schedule: string;
  prompt: string;
  enabled: boolean;
  toolPolicy: "readonly" | "autoapprove";
  /** `fresh` = no prior runs, `recent` = last few (default), `full` = thread. */
  contextMode: CronContext;
  notify: boolean;
  /** Epoch ms of the next scheduled run. */
  nextRunAt: number;
  lastRunAt: number | null;
  runCount: number;
  createdAt: number;
}

/** One execution of a cron job (created per run, not per message). */
export interface CronRun {
  id: string;
  jobId: string;
  conversationId: string;
  triggerMessageId: string;
  scheduledFor: number;
  startedAt: number;
  finishedAt: number | null;
  stopReason: string | null;
  error: string | null;
  usage: string | null;
  cost: number | null;
}

/** One message that fired a run — what the transcript pill is built from. */
export interface CronTrigger {
  messageId: string;
  jobId: string;
  title: string;
  scheduledFor: number;
}

/** `cron_preview` result: validation plus the next run time. */
export interface CronPreview {
  schedule: string;
  description: string;
  nextRunAt: number | null;
  error: string | null;
}

/** Payload for `create_cron` (all field names frozen by the backend). */
export type CreateCronPayload = {
  /** Omitted/null => derived from the prompt by the backend. */
  title?: string | null;
  /** Preset ("hourly" | "daily@HH:MM" | "weekly@DDD:HH:MM") or 5-field cron. */
  schedule: string;
  prompt: string;
  toolPolicy?: "readonly" | "autoapprove" | null;
  context?: CronContext | null;
  notify?: boolean | null;
};

/** Payload for `update_cron` — every field optional; only what's present changes. */
export type UpdateCronPayload = {
  id: string;
  title?: string | null;
  schedule?: string | null;
  prompt?: string | null;
  enabled?: boolean | null;
  toolPolicy?: "readonly" | "autoapprove" | null;
  context?: CronContext | null;
  notify?: boolean | null;
};

/** `cron-updated` event payload (started / finished / error per job). */
export interface CronUpdateEvent {
  jobId: string;
  conversationId: string;
  status: "started" | "finished" | "error";
}

export function listCrons(): Promise<CronJob[]> {
  return invoke<CronJob[]>("list_crons");
}

export function createCron(payload: CreateCronPayload): Promise<CronJob> {
  return invoke<CronJob>("create_cron", payload);
}

export function updateCron(payload: UpdateCronPayload): Promise<CronJob> {
  return invoke<CronJob>("update_cron", payload);
}

export function deleteCron(id: string): Promise<void> {
  return invoke<void>("delete_cron", { id });
}

/** Fire the job immediately in the background (no scheduler wait). */
export function runCronNow(id: string): Promise<void> {
  return invoke<void>("run_cron_now", { id });
}

export function listCronRuns(jobId?: string | null, limit?: number | null): Promise<CronRun[]> {
  return invoke<CronRun[]>("list_cron_runs", { jobId: jobId ?? null, limit: limit ?? null });
}

export function listCronTriggers(conversationId?: string | null): Promise<CronTrigger[]> {
  // Scoped to one chat: the transcript chip only renders the open conversation,
  // so the backend returns that chat's triggers (never the whole history).
  return invoke<CronTrigger[]>("list_cron_triggers", {
    conversationId: conversationId ?? null,
  });
}

/** Validate a schedule and resolve its next run; call debounced as the user types. */
export function cronPreview(schedule: string): Promise<CronPreview> {
  return invoke<CronPreview>("cron_preview", { schedule });
}

/** Tell the backend which chat is open, so a run there can skip its OS notification. */
export function cronSetActive(conversationId: string | null): Promise<void> {
  return invoke<void>("cron_set_active", { conversationId });
}

/** Send a notification through the same path a finished run uses; rejects with
 * the OS/D-Bus error instead of hiding it. */
export function testNotification(): Promise<void> {
  return invoke<void>("test_notification");
}

/** Ask for the OS notification permission (Android 13+ shows the system
 * dialog); resolves to whether notifications may be posted now. Desktop is
 * always granted. */
export function requestNotificationPermission(): Promise<boolean> {
  return invoke<boolean>("request_notification_permission");
}
