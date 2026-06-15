import { invoke } from "@tauri-apps/api/core";

/**
 * Result of `export_workspace`. Mirrors `ExportSummary` in
 * `src-tauri/src/commands/workspace.rs` — keep in sync.
 */
export interface ExportSummary {
  path: string;
  sessions_exported: number;
  project_files_exported: number;
  bytes_written: number;
}

/**
 * Result of `import_workspace`. Mirrors `ImportSummary` in
 * `src-tauri/src/commands/workspace.rs` — keep in sync.
 */
export interface ImportSummary {
  settings_applied: boolean;
  sessions_imported: number;
  project_files_written: number;
  project_files_skipped: number;
}

/**
 * Write a portable Cortex workspace bundle to `outPath`. The bundle includes
 * gateway connection settings (URL + model only — never the API key), the
 * Obsidian vault pointer, per-project `.cortex/*` config, and the last 50
 * sessions' messages.
 */
export async function exportWorkspace(outPath: string): Promise<ExportSummary> {
  return invoke<ExportSummary>("export_workspace", { outPath });
}

/**
 * Read a `cortex.workspace.v1` bundle from disk and apply it. Settings are
 * overwritten, sessions are upserted (idempotent), and project files are
 * written into `projectRoot` (or the active project if omitted) — existing
 * files with identical content are skipped, never silently clobbered.
 */
export async function importWorkspace(
  bundlePath: string,
  projectRoot?: string,
): Promise<ImportSummary> {
  return invoke<ImportSummary>("import_workspace", {
    bundlePath,
    projectRoot: projectRoot ?? null,
  });
}
