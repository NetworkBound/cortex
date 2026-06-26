import { invoke } from "@tauri-apps/api/core";

/**
 * Per-component context spend for a single session. Mirrors the Rust
 * `ContextBreakdown` struct in `src-tauri/src/commands/context.rs`.
 *
 * Each `*_chars` field counts characters (UTF-8); `total_estimated_tokens`
 * is `(sum of all chars) / 4`, the standard "good enough" estimate for
 * both Anthropic and OpenAI tokenisers when you don't want to pull in
 * tiktoken or a 6 MB BPE table.
 */
export interface ContextBreakdown {
  system_chars: number;
  claude_md_chars: number;
  rules_chars: number;
  repo_map_chars: number;
  history_chars: number;
  history_message_count: number;
  attached_files_chars: number;
  total_estimated_tokens: number;
}

/**
 * Ask the backend for a per-component char-count breakdown of the active
 * context. `projectRoot` is optional — without it, CLAUDE.md/rules tallies
 * fall to zero (we don't guess at a project).
 */
export async function estimateContextBreakdown(
  sessionId: string,
  projectRoot?: string,
): Promise<ContextBreakdown> {
  return invoke<ContextBreakdown>("estimate_context_breakdown", {
    sessionId,
    projectRoot: projectRoot ?? null,
  });
}

/** Aider-style `/web <url>` — server-side fetch, returns a markdown blob. */
export interface FetchedPage {
  url: string;
  title: string | null;
  markdown: string;
  fetched_unix_ms: number;
  truncated: boolean;
}

export async function fetchUrl(url: string): Promise<FetchedPage> {
  return invoke<FetchedPage>("fetch_url", { url });
}

/**
 * Continue-style `@diff` provider: returns the unified output of
 * `git diff --no-color HEAD` in `projectRoot`, capped at 32 KiB. Empty
 * string means "no changes" or "not a git repo" — both are non-errors.
 */
export async function gitWorkingDiff(projectRoot: string): Promise<string> {
  return invoke<string>("git_working_diff", { projectRoot });
}

/**
 * One compile error / warning from the project's toolchain. Mirrors the
 * Rust `Diagnostic` struct in `src-tauri/src/projects/diagnostics.rs`.
 */
export interface Diagnostic {
  source: string;
  severity: string;
  path: string;
  line: number;
  message: string;
}

/**
 * Continue-style `@problems` provider: runs `cargo check` and `tsc --noEmit`
 * (when their config files exist) and returns up to 100 diagnostics. Cached
 * for 30s per project root on the backend.
 */
export async function projectDiagnostics(
  projectRoot: string,
): Promise<Diagnostic[]> {
  return invoke<Diagnostic[]>("project_diagnostics", { projectRoot });
}

/**
 * Continue-style `@terminal` provider: returns the last ~200 lines of
 * `~/.cortex/last-shell-output.log` if present, else `null`.
 */
export async function recentTerminalOutput(): Promise<string | null> {
  return invoke<string | null>("recent_terminal_output");
}
