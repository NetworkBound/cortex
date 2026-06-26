import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::session_summary::SessionSummary`.
 *
 * `saved_path` is populated only when the summarizer was asked to write into
 * `~/Documents/Cortex Brain/sessions/`. A null value means either the user
 * didn't request a save or the write silently failed (the backend logs but
 * never refuses the summary itself when persistence fails).
 */
export interface SessionSummary {
  session_id: string;
  headline: string;
  body: string;
  generated_unix_ms: number;
  saved_path: string | null;
}

/**
 * Generate an AI summary of the given session by replaying its full message
 * history through the gateway. The backend caps the transcript at ~24k chars so
 * very long sessions stay under context limits — newest turns win.
 *
 * @param sessionId    The session id whose messages we should summarise.
 * @param saveToBrain  When true, also writes the summary to
 *   `~/Documents/Cortex Brain/sessions/<session_id>-summary.md` with YAML
 *   frontmatter so memory walkers can ingest it.
 */
export async function summarizeSession(
  sessionId: string,
  saveToBrain = false,
): Promise<SessionSummary> {
  return invoke<SessionSummary>("summarize_session", {
    args: { session_id: sessionId, save_to_brain: saveToBrain },
  });
}
