import { invoke } from "@tauri-apps/api/core";

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
  usage: string | null;
  stop_reason: string | null;
  created_at: number;
}

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
  usage?: string | null;
  stopReason?: string | null;
}): Promise<Message> {
  return invoke<Message>("add_message", payload);
}
