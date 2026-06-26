/**
 * Thin TS wrappers around the AI merge-conflict resolver Tauri commands.
 *
 * Mirrors `src-tauri::commands::conflict_resolver::{ConflictReport,
 * ResolvedConflict, StageReport}` — keep the field set in sync if you
 * change the Rust structs.
 *
 * The backend never writes resolved content itself — the modal calls the
 * existing `save_file_text` command after the user clicks "Accept
 * resolution" for a given file. `stage_resolved_files` runs `git add` on
 * the paths the user accepted.
 */
import { invoke } from "@tauri-apps/api/core";

/** A single AI-resolved conflict ready to be presented for review. */
export interface ResolvedConflict {
  /** Repo-relative path (matches `git ls-files -u`). */
  path: string;
  /** Original file content (with conflict markers). Capped at 64 KiB. */
  before: string;
  /** AI-proposed resolved content. */
  after: string;
  /** `"ours"` | `"theirs"` | `"merged"` — best-effort tag. */
  ai_chosen_side: "ours" | "theirs" | "merged";
  /** 0..1 heuristic confidence — UI buckets as low/med/high. */
  confidence: number;
}

/** Summary of an `audit_deps` / `resolve_conflicts` run. */
export interface ConflictReport {
  files: ResolvedConflict[];
  errors: string[];
}

/** Report from a `stage_resolved_files` batch — staged paths + per-file errs. */
export interface StageReport {
  staged: string[];
  errors: string[];
}

/**
 * Scan `project_root` for files with unresolved merge conflicts and ask
 * the gateway to propose a resolution for each. Files without conflict markers
 * (typically binaries flagged by `git ls-files -u`) are skipped with an
 * entry in `errors`.
 */
export async function resolveConflicts(
  projectRoot: string,
): Promise<ConflictReport> {
  return invoke<ConflictReport>("resolve_conflicts", {
    projectRoot,
  });
}

/**
 * Run `git add` on each of `paths` from `project_root`. Wraps the modal's
 * "Stage all accepted" button. Errors are per-file so a single bad path
 * doesn't poison the batch — the report's `errors` list tells the user
 * exactly which entries failed.
 */
export async function stageResolvedFiles(
  projectRoot: string,
  paths: string[],
): Promise<StageReport> {
  return invoke<StageReport>("stage_resolved_files", {
    projectRoot,
    paths,
  });
}

/** Bucket a confidence score into a UI tier — shared with refactor-suggester. */
export function confidenceTier(c: number): "high" | "med" | "low" {
  if (c >= 0.75) return "high";
  if (c >= 0.5) return "med";
  return "low";
}
