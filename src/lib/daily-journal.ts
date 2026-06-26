/**
 * Thin TS wrapper around `daily_journal` + `save_journal`. Mirrors
 * `src-tauri::commands::daily_journal::{JournalReport, JournalStats,
 * SaveJournalResult}` — keep field sets in sync if the Rust structs change.
 *
 * Backend collates today's sessions / commits / memory updates / snapshots /
 * PRP advances into an activity bundle, asks the gateway for a markdown summary
 * with the four required headers, and returns the structured stats alongside
 * the prose. 45s timeout.
 */
import { invoke } from "@tauri-apps/api/core";

/** Per-section counts surfaced as the scoreboard row above the markdown. */
export interface JournalStats {
  sessions: number;
  commits: number;
  memory_updates: number;
  snapshots: number;
  prp_advances: number;
}

/** Full report returned from `daily_journal`. `date` is `YYYY-MM-DD`. */
export interface JournalReport {
  date: string;
  markdown: string;
  stats: JournalStats;
  generated_unix_ms: number;
}

/** Returned from `save_journal`. `written_path` is absolute. */
export interface SaveJournalResult {
  written_path: string;
  bytes: number;
}

/**
 * Generate today's (or `date`'s) journal. Omit `date` to default to local
 * today. `projectRoot` is optional — without it the journal skips commits
 * and PRP activity but still picks up sessions / memory / snapshots.
 */
export async function dailyJournal(args: {
  projectRoot?: string | null;
  date?: string | null;
}): Promise<JournalReport> {
  return invoke<JournalReport>("daily_journal", {
    args: {
      project_root: args.projectRoot ?? null,
      date: args.date ?? null,
    },
  });
}

/**
 * Persist a generated journal to `~/Documents/Cortex Brain/journal/<date>.md`.
 * Frontmatter records the stat counts so memory walkers can surface journals
 * without re-parsing the body.
 */
export async function saveJournal(args: {
  date: string;
  markdown: string;
  stats: JournalStats;
}): Promise<SaveJournalResult> {
  return invoke<SaveJournalResult>("save_journal", {
    args: {
      date: args.date,
      markdown: args.markdown,
      stats: args.stats,
    },
  });
}
