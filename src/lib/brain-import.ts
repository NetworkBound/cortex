import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::brain_import::ImportResult`. Returned to the
 * caller so the chat can echo the written path back to the user.
 */
export interface ImportResult {
  written_path: string;
  bytes: number;
}

/**
 * Save a markdown body into the Cortex Brain vault under
 * `~/Documents/Cortex Brain/imports/<YYYY-MM-DD>-<slug>.md`.
 *
 * The backend prepends YAML frontmatter (`imported_at`, `kind`) so downstream
 * Obsidian / memory walkers can identify the file kind without parsing the
 * filename.
 *
 * @param content Markdown body (no frontmatter — backend adds it).
 * @param label   Filename label; will be slugified.
 * @param kind    Short identifier — e.g. `chat`, `note`, `share`.
 */
export async function importToBrain(
  content: string,
  label: string,
  kind: string,
): Promise<ImportResult> {
  return invoke<ImportResult>("import_to_brain", { content, label, kind });
}
