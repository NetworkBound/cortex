import { invoke } from "@tauri-apps/api/core";

/**
 * Project-wide file search — bindings for the Tauri `search_project` and
 * `find_files` commands. The backend caps results to 500 hits / 100 files
 * (text search) and 50 paths (find files); callers can rely on those limits
 * without paginating.
 *
 * The UI lives at `src/components/SearchPanel.tsx`.
 */

export interface SearchHit {
  path: string;
  line: number;
  col: number;
  match_text: string;
  before: string | null;
  after: string | null;
}

export interface SearchOptions {
  /** Match case exactly. Default false (smart-case-off). */
  caseSensitive?: boolean;
  /** Treat the query as a literal string instead of a regex. Default false. */
  fixedString?: boolean;
}

/**
 * Full-text search across files under `projectRoot`. Empty queries
 * short-circuit to `[]` without round-tripping the backend.
 */
export async function searchProject(
  projectRoot: string,
  query: string,
  opts: SearchOptions = {},
): Promise<SearchHit[]> {
  const q = query.trim();
  if (!q || !projectRoot) return [];
  return invoke<SearchHit[]>("search_project", {
    projectRoot,
    query: q,
    caseSensitive: opts.caseSensitive ?? false,
    fixedString: opts.fixedString ?? false,
  });
}

/**
 * Fuzzy file-path search ("Go to file" / Ctrl+P). Returns absolute paths
 * ranked by a Sublime-style subsequence scorer. Empty query returns the
 * first 50 files under the project (matches VS Code's behaviour).
 */
export async function findFiles(
  projectRoot: string,
  query: string,
): Promise<string[]> {
  if (!projectRoot) return [];
  return invoke<string[]>("find_files", {
    projectRoot,
    query: query.trim(),
  });
}

/**
 * Group a flat hit list by `path` for grouped rendering in the panel.
 * Preserves the order of first occurrence so the most-recently-touched
 * file (per the backend walker) stays at the top.
 */
export function groupHitsByFile(hits: SearchHit[]): { path: string; hits: SearchHit[] }[] {
  const order: string[] = [];
  const map = new Map<string, SearchHit[]>();
  for (const h of hits) {
    if (!map.has(h.path)) {
      order.push(h.path);
      map.set(h.path, []);
    }
    map.get(h.path)!.push(h);
  }
  return order.map((p) => ({ path: p, hits: map.get(p)! }));
}

/** Shorten an absolute path to `…/last-3-segments` for display. */
export function shortenPath(path: string, projectRoot?: string | null): string {
  if (projectRoot && path.startsWith(projectRoot)) {
    return path.slice(projectRoot.length).replace(/^[\\/]/, "");
  }
  const parts = path.split(/[\\/]/);
  if (parts.length <= 3) return path;
  return "…/" + parts.slice(-3).join("/");
}
