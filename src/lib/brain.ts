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
  /** Indexed chunk id backing this citation (chat message id or note path). */
  chunk_id: string;
  /** Index-time timestamp (message ts, or note mtime at embed time), unix ms. */
  indexed_ts: number;
  /** Note whose source file changed since indexing — cited text may be outdated. */
  stale: boolean;
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
 *  with citations. Fully local (Ollama). `projectRoot`, when given, scopes
 *  retrieval to that project plus global/unscoped content — content owned by
 *  a DIFFERENT project never leaks into the answer (per-project memory
 *  namespaces). Omit it to search everything (legacy behavior). */
export async function brainRag(
  question: string,
  limit = 8,
  projectRoot?: string | null,
): Promise<BrainAnswer> {
  return invoke<BrainAnswer>("brain_rag", { question, limit, projectRoot: projectRoot ?? null });
}

export async function setObsidianVault(path: string | null): Promise<void> {
  return invoke("set_obsidian_vault", { path });
}

/** One member of a near-duplicate memory group — same display shape as a
 *  note citation (no content), see `BrainCitation`. */
export interface DuplicateMember {
  reference: string;
  open_path: string;
  project_root: string | null;
}

export interface DuplicateGroup {
  members: DuplicateMember[];
  max_similarity: number;
}

/** Find near-identical indexed notes (embedding-distance clustering) so the
 *  UI can flag likely copy/paste duplicates for the user to merge — detection
 *  only, nothing is deleted. `threshold` overrides the default strictness
 *  (0.0-1.0 cosine similarity). */
export async function brainMemoryDuplicates(threshold?: number): Promise<DuplicateGroup[]> {
  return invoke<DuplicateGroup[]>("brain_memory_duplicates", { threshold: threshold ?? null });
}
