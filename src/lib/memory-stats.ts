import { invoke } from "@tauri-apps/api/core";

/**
 * Tauri-bridge wrappers for the memory-bridge stats panel. Mirrors the Rust
 * `MemoryStats` / `SyncReport` shapes verbatim so the panel can render results
 * without any post-processing.
 */

export type MemorySourceKind =
  | "claude_project_memory"
  | "runbooks"
  | "global_instructions"
  | "project_instructions"
  | "obsidian";

export interface SourceStats {
  label: string;
  kind: MemorySourceKind;
  root_path: string;
  file_count: number;
  total_bytes: number;
  oldest_unix_ms: number | null;
  newest_unix_ms: number | null;
}

export interface ChromaState {
  exists: boolean;
  bytes: number;
}

export interface MemoryStats {
  sources: SourceStats[];
  chroma: ChromaState;
  total_file_count: number;
  total_bytes: number;
}

export interface SyncReport {
  imported: number;
  skipped: number;
  errors: string[];
}

export async function memoryStats(): Promise<MemoryStats> {
  return invoke<MemoryStats>("memory_stats");
}

export async function syncMemory(): Promise<SyncReport> {
  return invoke<SyncReport>("sync_memory");
}

/** Human-friendly byte size — keeps memory-stats UI free of utility imports. */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/** Compact "Mar 4" / "2024-03-04" style date from a unix-ms timestamp. */
export function formatDate(ms: number | null): string {
  if (ms == null) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/**
 * Aggregate file counts per `MemorySourceKind`. Used by the panel's top-line
 * mini bar chart so we don't recompute on every render.
 */
export function groupByKind(sources: SourceStats[]): Array<{ kind: MemorySourceKind; count: number; bytes: number }> {
  const byKind = new Map<MemorySourceKind, { count: number; bytes: number }>();
  for (const s of sources) {
    const cur = byKind.get(s.kind) ?? { count: 0, bytes: 0 };
    cur.count += s.file_count;
    cur.bytes += s.total_bytes;
    byKind.set(s.kind, cur);
  }
  // Stable presentation order so the chart doesn't flip between renders.
  const order: MemorySourceKind[] = [
    "claude_project_memory",
    "runbooks",
    "global_instructions",
    "project_instructions",
    "obsidian",
  ];
  return order
    .map((kind) => ({ kind, ...(byKind.get(kind) ?? { count: 0, bytes: 0 }) }))
    .filter((row) => row.count > 0);
}

export function kindLabel(kind: MemorySourceKind): string {
  switch (kind) {
    case "claude_project_memory":
      return "Claude memory";
    case "runbooks":
      return "Runbooks";
    case "global_instructions":
      return "Global instructions";
    case "project_instructions":
      return "Project instructions";
    case "obsidian":
      return "Obsidian";
  }
}
