/**
 * Thin TS wrapper around the `review_diff` Tauri command. Mirrors
 * `src-tauri::commands::review::{ReviewFinding, ReviewReport}` — keep the
 * field set in sync if you change the Rust structs.
 *
 * The backend caps the diff (16 KiB per file, 64 KiB total), redacts secrets
 * and times out the model call at 120s; callers surface the rejection
 * message (the `ReviewPanel` does this in its body slot).
 */
import { invoke } from "@tauri-apps/api/core";

export type ReviewSeverity = "critical" | "high" | "medium" | "low" | "info";

/** Strongest first — the order the panel groups findings in. */
export const REVIEW_SEVERITIES: ReviewSeverity[] = [
  "critical",
  "high",
  "medium",
  "low",
  "info",
];

export interface ReviewFinding {
  severity: ReviewSeverity | string;
  /** Repo-relative path with forward slashes; `""` when not file-specific. */
  file: string;
  line: number | null;
  title: string;
  detail: string;
  suggestion: string | null;
}

export interface ReviewReport {
  base: string | null;
  model: string;
  agent_id: string;
  cross_model: boolean;
  fell_back: boolean;
  summary: string | null;
  findings: ReviewFinding[];
  files: string[];
  diff_bytes: number;
  truncated: boolean;
  unparsed: boolean;
  latency_ms: number;
}

export interface ReviewArgs {
  /** Absolute project root (any separator flavour). */
  project_root: string;
  /** Branch/commit to diff against; omit for uncommitted changes vs HEAD. */
  base?: string | null;
  /** Explicit reviewer model slug; omit to auto-pick a cross-model reviewer. */
  agent?: string | null;
  /** The session's current model, so the auto-pick can choose another one. */
  author_model?: string | null;
}

export async function reviewDiff(args: ReviewArgs): Promise<ReviewReport> {
  return invoke<ReviewReport>("review_diff", {
    projectRoot: args.project_root,
    base: args.base ?? null,
    agent: args.agent ?? null,
    authorModel: args.author_model ?? null,
  });
}

/**
 * Parse `/review [base] [--model <slug>]` arguments. Tokens starting with
 * `--model=`/`--model ` (or `-m`) pick the reviewer; the first remaining
 * token is the base ref.
 */
export function parseReviewArgs(raw: string): {
  base: string | null;
  model: string | null;
} {
  const tokens = raw.trim().split(/\s+/).filter(Boolean);
  let base: string | null = null;
  let model: string | null = null;
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (t.startsWith("--model=")) {
      model = t.slice("--model=".length) || null;
    } else if (t === "--model" || t === "-m") {
      model = tokens[i + 1] ?? null;
      i++;
    } else if (base === null) {
      base = t;
    }
  }
  return { base, model };
}
