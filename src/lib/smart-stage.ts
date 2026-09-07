import { invoke } from "@tauri-apps/api/core";

/**
 * Structured result from the `smart_stage` backend. Mirrors the Rust
 * `SmartStageReport` struct one-for-one.
 */
export interface SmartStageReport {
  /** Files we actually ran `git add` on successfully. */
  staged: string[];
  /** Files the model deliberately left out. */
  skipped: string[];
  /** Per-file `git add` failures (path + stderr). Stage continues on these. */
  errors: string[];
  /** One-line model rationale (or "parse failed" when the JSON was malformed). */
  reason: string;
}

/**
 * Ask the gateway to pick which files to `git add` in `projectRoot` based on
 * `intent` (e.g. "just the snippets backend changes, not the UI"), then run
 * the adds. The backend runs `git status --porcelain -uall` + `git diff` to
 * build the prompt, so an empty working tree returns a no-op report.
 *
 * Throws when:
 * - `projectRoot` isn't a directory,
 * - `intent` is empty,
 * - the gateway call times out.
 */
export async function smartStage(
  projectRoot: string,
  intent: string,
): Promise<SmartStageReport> {
  return invoke<SmartStageReport>("smart_stage", { projectRoot, intent });
}
