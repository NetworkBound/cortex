import { invoke } from "@tauri-apps/api/core";

export interface BrainSnapshot {
  recent_projects: RecentProject[];
  recent_sessions: RecentSession[];
  recent_memory: RecentMemory[];
  obsidian_vault: string | null;
}

export interface RecentProject {
  root: string;
  name: string;
  last_modified_ms: number;
  has_git: boolean;
  has_claude_md: boolean;
  has_runbooks: boolean;
}

export interface RecentSession {
  session_id: string;
  last_active_ms: number;
  message_count: number;
  agents: string[];
  first_message: string | null;
}

export interface RecentMemory {
  path: string;
  title: string | null;
  source: string;
  modified_unix_ms: number;
  preview: string;
}

export async function brainSnapshot(): Promise<BrainSnapshot> {
  return invoke<BrainSnapshot>("brain_snapshot");
}

/** One source behind a RAG answer. `reference` is a session_id (chat) or a
 *  vault-/home-relative path (note) for display. `open_path` is the absolute
 *  file path to open for a note (empty for chats — those resume by session). */
export interface BrainCitation {
  n: number;
  source: string; // "chat" | "note"
  reference: string;
  open_path: string;
  snippet: string;
  score: number;
}

export interface BrainAnswer {
  answer: string;
  citations: BrainCitation[];
  model: string;
  used_context: boolean;
}

/** Unified RAG: answer a question grounded in the user's notes + chat history,
 *  with citations. Fully local (Ollama). */
export async function brainRag(question: string, limit = 8): Promise<BrainAnswer> {
  return invoke<BrainAnswer>("brain_rag", { question, limit });
}

export async function setObsidianVault(path: string | null): Promise<void> {
  return invoke("set_obsidian_vault", { path });
}
