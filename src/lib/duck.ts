/**
 * Thin TS wrapper around `duck_question` + `save_duck_transcript`. Mirrors
 * `src-tauri::commands::duck::{DuckTurn, SaveDuckResult}` — keep field sets
 * in sync if the Rust structs change.
 *
 * The backend prompts the gateway with a strict "Socratic rubber duck — never
 * answer, always ask one question" system prompt and times out at 20s. The
 * UI replays the full transcript on every call so we don't need any
 * server-side session state.
 */
import { invoke } from "@tauri-apps/api/core";

/** A single bubble in the duck dialog. `role` is "user" or "duck". */
export interface DuckTurn {
  role: "user" | "duck";
  content: string;
  ts_unix_ms: number;
}

/** Returned from `save_duck_transcript`. `written_path` is absolute. */
export interface SaveDuckResult {
  written_path: string;
  bytes: number;
}

/**
 * Ask the gateway for the next Socratic question. `transcript` is the full back-
 * and-forth so far (including the user's latest message at the end). Returns
 * the new duck `DuckTurn` — append it to the transcript and re-render.
 */
export async function duckQuestion(
  topic: string,
  transcript: DuckTurn[],
): Promise<DuckTurn> {
  return invoke<DuckTurn>("duck_question", {
    args: { topic, transcript },
  });
}

/**
 * Persist the transcript to `~/Documents/Cortex Brain/duck/<date>-<slug>.md`.
 * The backend slug-sanitises the topic so the filename can't escape the
 * vault root.
 */
export async function saveDuckTranscript(
  topic: string,
  transcript: DuckTurn[],
): Promise<SaveDuckResult> {
  return invoke<SaveDuckResult>("save_duck_transcript", {
    args: { topic, transcript },
  });
}
