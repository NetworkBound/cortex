/**
 * Thin TS wrapper around the `generate_changelog` Tauri command.
 *
 * Mirrors `src-tauri::commands::changelog::ChangelogResult`. The backend caps
 * the commit blob at 32 KiB and times out the gateway call at 45s — callers
 * should surface the rejection message verbatim.
 */
import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::changelog::ChangelogResult`. `since` is the
 * resolved time-range string (the backend defaults `null`/empty to
 * `"2 weeks ago"`).
 */
export interface ChangelogResult {
  since: string;
  markdown: string;
  commit_count: number;
  generated_unix_ms: number;
}

/**
 * Ask the gateway for a Keep-a-Changelog-style markdown document covering the
 * recent commits in `projectRoot`. Pass `since` to override the default
 * "2 weeks ago" range — anything `git log --since=` accepts is valid.
 */
export async function generateChangelog(
  projectRoot: string,
  since?: string | null,
): Promise<ChangelogResult> {
  const sinceArg = since && since.trim() ? since.trim() : null;
  return invoke<ChangelogResult>("generate_changelog", {
    projectRoot,
    since: sinceArg,
  });
}
