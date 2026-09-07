import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::refactor_suggester::Refactor`.
 *
 * `confidence` is clamped to [0,1] backend-side, so callers can render a
 * three-tier pill (high ≥ 0.75, med ≥ 0.5, low otherwise) without their own
 * bounds checking.
 */
export interface Refactor {
  name: string;
  rationale: string;
  before_snippet: string;
  after_snippet: string;
  confidence: number;
}

/**
 * Mirrors `src-tauri::commands::refactor_suggester::RefactorReport`. An empty
 * `refactors` array means the model returned unparseable output — the backend
 * deliberately swallows that rather than failing the command, so the modal
 * can render an "AI returned no parseable refactors" empty-state instead of a
 * crash.
 */
export interface RefactorReport {
  path: string;
  refactors: Refactor[];
  generated_unix_ms: number;
}

/**
 * Ask the gateway for 3-5 specific refactor suggestions on the given file. The
 * backend caps the file blob at 64 KiB and times out the gateway call at 30s.
 *
 * @param path    Absolute path to the file the user wants refactor advice on.
 * @param intent  Optional free-form focus ("testability", "minimise allocs", …).
 *   Empty or whitespace-only strings are sent through as `null` so the backend
 *   skips the "User focus:" line entirely.
 */
export async function suggestRefactors(
  path: string,
  intent?: string,
): Promise<RefactorReport> {
  const cleanedIntent = intent?.trim();
  return invoke<RefactorReport>("suggest_refactors", {
    args: {
      path,
      intent: cleanedIntent && cleanedIntent.length > 0 ? cleanedIntent : null,
    },
  });
}

/** Bucket a confidence score into a UI tier — keeps tier logic in one place. */
export function confidenceTier(c: number): "high" | "med" | "low" {
  if (c >= 0.75) return "high";
  if (c >= 0.5) return "med";
  return "low";
}
