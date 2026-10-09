import { invoke, Channel } from "@tauri-apps/api/core";

export interface Conversation {
  id: string;
  title: string;
  model: string | null;
  system_prompt: string | null;
  compaction_summary: string | null;
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
}

export interface Config {
  baseUrl: string;
  model: string;
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
   * Whether models.dev says the model accepts image input. `null` when the
   * provider isn't in the catalog, in which case nothing is warned about.
   */
  vision: boolean | null;
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

export function renameConversation(conversationId: string, title: string): Promise<void> {
  return invoke<void>("rename_conversation", { conversationId, title });
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

export function listModels(refresh?: boolean): Promise<ModelInfo[]> {
  return invoke<ModelInfo[]>("list_models", { refresh: refresh ?? null });
}

export function getPricing(modelId: string): Promise<Pricing> {
  return invoke<Pricing>("get_pricing", { modelId });
}

export function thinkingOptions(modelId: string): Promise<ThinkingOptions> {
  return invoke<ThinkingOptions>("thinking_options", { modelId });
}

export function refreshPricing(): Promise<RefreshResult> {
  return invoke<RefreshResult>("refresh_pricing");
}

export function streamChat(
  conversationId: string,
  content: string,
  attachments: Attachment[],
  channel: Channel<StreamEvent>,
): Promise<void> {
  return invoke<void>("stream_chat", {
    conversationId,
    content,
    attachments: attachments.length > 0 ? attachments : null,
    channel,
  });
}

export function readAttachments(paths: string[]): Promise<Attachment[]> {
  return invoke<Attachment[]>("read_attachments", { paths });
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
 */
export function backfillMemories(
  minChars?: number,
  concurrency?: number,
): Promise<BackfillStatus> {
  return invoke<BackfillStatus>("backfill_memories", { minChars, concurrency });
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
