import { invoke } from "@tauri-apps/api/core";

export interface ChatSummary {
  file_path: string;
  session_id: string;
  project: string | null;
  project_root: string | null;
  first_message: string | null;
  message_count: number;
  modified_unix_ms: number;
}

export interface ChatTurn {
  role: string;
  content: string;
  ts_unix_ms: number | null;
}

export interface ChatTranscript {
  file_path: string;
  session_id: string;
  project_root: string | null;
  turns: ChatTurn[];
}

export interface ChatSearchHit {
  file_path: string;
  session_id: string;
  project: string | null;
  role: string;
  snippet: string;
  modified_unix_ms: number;
}

export async function listClaudeChats(): Promise<ChatSummary[]> {
  return invoke<ChatSummary[]>("list_claude_chats");
}

export async function getClaudeChat(
  path: string,
  maxTurns?: number,
): Promise<ChatTranscript> {
  return invoke<ChatTranscript>("get_claude_chat", {
    path,
    maxTurns: maxTurns ?? null,
  });
}

export async function searchClaudeChats(
  query: string,
  limit?: number,
): Promise<ChatSearchHit[]> {
  return invoke<ChatSearchHit[]>("search_claude_chats", {
    query,
    limit: limit ?? null,
  });
}

/**
 * Trim `content` to at most `max` characters, collapsing newlines into
 * single spaces and appending an ellipsis when truncation occurred.
 *
 * Used by ChatHistorySidebar's hover-preview card and view modal to keep
 * snippets single-line and bounded; safe on empty/null inputs.
 */
export function truncateMessage(
  content: string | null | undefined,
  max: number,
): string {
  if (!content) return "";
  const flat = content.replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  return flat.slice(0, Math.max(0, max - 1)).trimEnd() + "…";
}
