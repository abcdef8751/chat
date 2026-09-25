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
  modelOverrides: Record<string, ModelOverride>;
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
}

export interface ThinkingOptions {
  options: string[];
  source: "models.dev" | "openai" | "none";
  supportsReasoning: boolean;
}

export interface MemoryFile {
  name: string;
  content: string;
  generated: boolean;
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
