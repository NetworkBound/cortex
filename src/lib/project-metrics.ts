/**
 * Thin TS wrapper around the `project_metrics` Tauri command.
 *
 * Mirrors `src-tauri::commands::project_metrics::ProjectMetrics`. The walk is
 * bounded at 50,000 entries and respects `.cortexignore` — callers should
 * surface `truncated` so the UI can warn when the picture is incomplete.
 */
import { invoke } from "@tauri-apps/api/core";

export interface LangStat {
  files: number;
  lines: number;
  bytes: number;
}

export interface FileEntry {
  path: string;
  lines: number;
  bytes: number;
}

export interface DirEntry {
  path: string;
  file_count: number;
  total_bytes: number;
}

export interface ProjectMetrics {
  project_root: string;
  total_files: number;
  total_lines: number;
  total_bytes: number;
  languages: Record<string, LangStat>;
  largest_files: FileEntry[];
  biggest_dirs: DirEntry[];
  generated_unix_ms: number;
  truncated: boolean;
}

/**
 * Walk `projectRoot`, counting lines + bytes per file and bucketing by
 * language / first-level directory. Read-only; results are computed fresh
 * on every call (no backend cache).
 */
export async function projectMetrics(
  projectRoot: string,
): Promise<ProjectMetrics> {
  return invoke<ProjectMetrics>("project_metrics", { projectRoot });
}

/** Pretty-print a byte count with KB/MB/GB suffixes (1024-based). */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 ? 0 : 1)} ${units[i]}`;
}

/** Locale-aware integer formatter so totals render with thousand separators. */
export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}
