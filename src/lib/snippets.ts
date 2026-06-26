// Saved-prompt snippet store, backed by `~/.cortex/snippets.json`.
//
// Snippets are short reusable text blobs the user references with `#name` in
// the composer. The picker reads them via `listSnippets`; insertion writes
// `#snippet:name` into the textarea, and `expandSnippets` swaps each marker
// for the snippet body right before chatSend.
//
// Storage shape (mirrors what `commands/snippets.rs` reads/writes):
//   { [name]: { body, created_unix_ms, last_used_unix_ms } }
//
// All commands are best-effort: on backend failure we return an empty list /
// `null` rather than throwing, so the picker degrades gracefully when the
// snippets file doesn't exist yet.

import { invoke } from "@tauri-apps/api/core";

export interface Snippet {
  name: string;
  body: string;
  created_unix_ms: number;
  last_used_unix_ms: number;
}

/** Inline marker the composer inserts when the user picks a snippet. */
const SNIPPET_MARKER_RE = /#snippet:([A-Za-z0-9_\-.]+)/g;

export async function listSnippets(): Promise<Snippet[]> {
  try {
    return await invoke<Snippet[]>("list_snippets");
  } catch {
    return [];
  }
}

export async function getSnippet(name: string): Promise<Snippet | null> {
  try {
    return await invoke<Snippet | null>("get_snippet", { name });
  } catch {
    return null;
  }
}

export async function saveSnippet(name: string, body: string): Promise<Snippet | null> {
  try {
    return await invoke<Snippet>("save_snippet", { name, body });
  } catch (err) {
    console.warn("saveSnippet failed", err);
    return null;
  }
}

export async function deleteSnippet(name: string): Promise<boolean> {
  try {
    await invoke("delete_snippet", { name });
    return true;
  } catch (err) {
    console.warn("deleteSnippet failed", err);
    return false;
  }
}

/**
 * Replace every `#snippet:<name>` marker in `text` with the snippet body.
 * Unknown names are left as-is (so the user sees the unresolved marker rather
 * than a silent drop). Bumps `last_used_unix_ms` for every expanded snippet.
 */
export async function expandSnippets(text: string): Promise<string> {
  const matches = Array.from(text.matchAll(SNIPPET_MARKER_RE));
  if (matches.length === 0) return text;
  const names = Array.from(new Set(matches.map((m) => m[1])));
  const resolved = new Map<string, string>();
  await Promise.all(
    names.map(async (n) => {
      const s = await getSnippet(n);
      if (s) resolved.set(n, s.body);
    }),
  );
  return text.replace(SNIPPET_MARKER_RE, (full, name: string) => {
    return resolved.get(name) ?? full;
  });
}

/** Bare regex export for callers that want to detect snippet markers themselves. */
export const SNIPPET_MARKER = SNIPPET_MARKER_RE;
