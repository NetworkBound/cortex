import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::memory_dedupe::DuplicatePair`. The backend
 * returns up to 50 pairs sorted by similarity desc.
 */
export interface DuplicatePair {
  file_a: string;
  file_b: string;
  similarity: number;
  shared_words: string[];
}

export interface DedupeOptions {
  /** Jaccard threshold (0 – 1). Defaults to backend default (0.4). */
  threshold?: number;
  activeProject?: string | null;
  obsidianVault?: string | null;
}

/**
 * Walk every markdown file under the configured memory sources and return
 * pairs with normalized-token Jaccard similarity above `threshold`.
 */
export async function findDuplicateMemory(
  opts: DedupeOptions = {},
): Promise<DuplicatePair[]> {
  return invoke<DuplicatePair[]>("find_duplicate_memory", {
    threshold: opts.threshold ?? null,
    activeProject: opts.activeProject ?? null,
    obsidianVault: opts.obsidianVault ?? null,
  });
}
