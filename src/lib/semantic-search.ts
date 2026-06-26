import { invoke } from "@tauri-apps/api/core";

/** One semantic-search result over the vault/memory. */
export interface SemanticHit {
  path: string;
  snippet: string;
  score: number;
  /** "semantic" when re-ranked by embeddings; "lexical" on graceful fallback. */
  mode: string;
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
