import { invoke } from "@tauri-apps/api/core";

/** One semantic-search result over the vault/memory. */
export interface SemanticHit {
  path: string;
  snippet: string;
  score: number;
  /** "semantic" when re-ranked by embeddings; "lexical" on graceful fallback. */
  mode: string;
}

/** Mirrors `ChatSemanticHit` in `commands/chat_semantic.rs`. */
export interface ChatSemanticHit {
  session_id: string;
  message_id: string;
  /** Unix ms. */
  ts: number;
  role: string;
  snippet: string;
  score: number;
  /** Owning project root, or `null` for global/unscoped content. */
  project_root: string | null;
}

/** Mirrors `ReindexResult` in `commands/chat_semantic.rs`. */
export interface ReindexResult {
  embedded: number;
  failed: number;
  total_indexed: number;
}

/**
 * Search past chat messages by meaning (embeddings via the local Ollama).
 * Throws when the embedder is unreachable — callers treat that as "no
 * semantic results", not as a search failure.
 */
export async function semanticChatSearch(
  query: string,
  limit = 10,
): Promise<ChatSemanticHit[]> {
  return invoke<ChatSemanticHit[]>("semantic_chat_search", { query, limit });
}

/** Re-embed every stored chat message (and vault note) that isn't indexed yet. */
export async function semanticChatReindex(): Promise<ReindexResult> {
  return invoke<ReindexResult>("semantic_chat_reindex");
}

/**
 * Search the Obsidian vault / memory by meaning: vault markdown is retrieved
 * lexically then re-ranked by embedding cosine similarity via the homelab
 * Ollama (mxbai-embed-large). Falls back to lexical order if Ollama/the embed
 * model is unavailable — never throws on the search itself.
 */
export async function semanticMemorySearch(
  query: string,
  projectRoot?: string | null,
  limit = 10,
): Promise<SemanticHit[]> {
  return invoke<SemanticHit[]>("semantic_memory_search", {
    query,
    projectRoot: projectRoot ?? null,
    limit,
  });
}
