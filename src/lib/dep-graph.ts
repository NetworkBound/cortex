import { invoke } from "@tauri-apps/api/core";

/**
 * Frontend types for the `build_dep_graph` Tauri command. Mirrors the
 * `DepNode` / `DepEdge` / `DepGraph` structs in
 * `src-tauri/src/commands/dep_graph.rs` — keep both sides in sync.
 *
 * The backend walks the active project (respecting `.cortexignore`),
 * parses imports out of every supported source file, and emits one
 * node per file plus one edge per resolved intra-project import.
 */
export interface DepGraphNode {
  /** Project-relative forward-slash path; the node id. */
  id: string;
  /** Final path component for compact display. */
  label: string;
  /**
   * Short language tag: `ts`, `tsx`, `js`, `jsx`, `rs`, `py`, `css`,
   * `html`, `json`, `md`. Anything else is reported as `other` by the
   * frontend renderer's color map.
   */
  language: string;
  /** Line count of the source file; drives the rendered radius. */
  lines: number;
}

export interface DepGraphEdge {
  /** Importer file id (project-relative path). */
  from: string;
  /** Importee file id (project-relative path). */
  to: string;
  /** `import` | `require` | `use` | `mod` | `py-import`. */
  kind: string;
}

export interface DepGraph {
  nodes: DepGraphNode[];
  edges: DepGraphEdge[];
  /** True when the backend hit the 500-node / 2000-edge cap. */
  truncated: boolean;
}

/**
 * Fire the `build_dep_graph` Tauri command for the given project root.
 * Throws on backend errors (e.g. invalid path); the panel above shows
 * the message verbatim so users can tell whether their .cortexignore
 * or path argument is the problem.
 */
export async function buildDepGraph(projectRoot: string): Promise<DepGraph> {
  return invoke<DepGraph>("build_dep_graph", { projectRoot });
}

/**
 * Color palette keyed by the backend's `language` field. Mirrors the
 * KnowledgeGraph node theming style — saturated enough to read on the
 * dark background, muted enough not to vibrate next to neighbours.
 *
 * Anything missing from the table falls back to the `other` gray.
 */
export const LANGUAGE_COLORS: Record<string, string> = {
  ts: "#6aa9ff",
  tsx: "#6aa9ff",
  js: "#f7df1e",
  jsx: "#f7df1e",
  rs: "#f59e0b",
  py: "#ffd93d",
  css: "#22d3ee",
  html: "#22c55e",
  json: "#a78bfa",
  md: "#94a3b8",
  other: "#6b7280",
};

/** Resolve a `language` field to a CSS color, falling back to gray. */
export function colorForLanguage(language: string): string {
  return LANGUAGE_COLORS[language] ?? LANGUAGE_COLORS.other!;
}
