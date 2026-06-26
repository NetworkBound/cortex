import { invoke } from "@tauri-apps/api/core";

/**
 * Error source the backend will pull from. Mirrors the match arms in
 * `src-tauri::commands::ai_debugger::resolve_error`. `manual` requires a free-
 * form `error_text`; `chat_error` lets the frontend forward the last error-
 * role message without round-tripping the chat history.
 */
export type DebugSource =
  | "recent_crash"
  | "recent_issue"
  | "last_test_failure"
  | "chat_error"
  | "manual";

/** Mirrors `src-tauri::commands::ai_debugger::DebugResult`. */
export interface DebugResult {
  error_source: DebugSource;
  error_summary: string;
  root_cause: string;
  suggested_fix: string;
  code_patch: string;
  /** Clamped to [0,1] backend-side — safe to render as a pill directly. */
  confidence: number;
  source_path: string | null;
  source_line: number | null;
  generated_unix_ms: number;
}

/**
 * Ask the gateway to debug the chosen error. `error_text` is required when
 * `errorSource` is `manual` / `chat_error`; the other sources pull from the
 * crash log / issues table / `~/.cortex/last-test-failure.json`.
 */
export async function debugError(args: {
  projectRoot: string;
  errorSource: DebugSource;
  errorId?: string | null;
  errorText?: string | null;
  errorStack?: string | null;
}): Promise<DebugResult> {
  return invoke<DebugResult>("debug_error", {
    args: {
      project_root: args.projectRoot,
      error_source: args.errorSource,
      error_id: args.errorId ?? null,
      error_text: args.errorText ?? null,
      error_stack: args.errorStack ?? null,
    },
  });
}

/**
 * Three-tier UI pill for the confidence score — matches `refactor_suggester`'s
 * thresholds so the visual treatment stays consistent across debugger /
 * refactor modals. Patches are only safe to "apply" at the high tier; the
 * modal disables the button below 0.6 by design.
 */
export function confidenceTier(c: number): "high" | "med" | "low" {
  if (c >= 0.75) return "high";
  if (c >= 0.5) return "med";
  return "low";
}

/** Human label for the source dropdown. Kept close to the type so adding a
 *  new variant is a single-file change. */
export const DEBUG_SOURCE_LABELS: Record<DebugSource, string> = {
  recent_crash: "Most recent crash",
  recent_issue: "Most recent observability issue",
  last_test_failure: "Last test failure",
  chat_error: "Last chat error",
  manual: "Paste error manually",
};
