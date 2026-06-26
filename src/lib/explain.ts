/**
 * Thin TS wrapper around the `explain_code` + `save_explanation` Tauri
 * commands. Mirrors `src-tauri::commands::explain::{ExplainResult,
 * ExplainSaveResult}` — keep the field set in sync if you change the Rust
 * structs.
 *
 * The backend caps the source blob at 64 KiB and times out the gateway call
 * at 45s — callers should surface the error message returned from the
 * promise rejection (the `ExplainModal` does this in its body slot).
 */
import { invoke } from "@tauri-apps/api/core";

/** Canonical audience keys understood by the backend `resolve_audience`. */
export type ExplainAudience = "beginner" | "intermediate" | "expert";

/**
 * Mirrors `src-tauri::commands::explain::ExplainResult`. `line_start`/
 * `line_end` are echoed back from the *clamped* range the backend actually
 * used (it snaps over-shoot to the file length) so the modal can re-paint
 * the inputs without going out of bounds.
 */
export interface ExplainResult {
  path: string;
  line_start: number | null;
  line_end: number | null;
  language: string;
  audience: string;
  markdown: string;
  generated_unix_ms: number;
}

/**
 * Mirrors `src-tauri::commands::explain::ExplainSaveResult`. Returned to the
 * modal so we can echo the written path in a toast.
 */
export interface ExplainSaveResult {
  written_path: string;
  bytes: number;
}

export interface ExplainArgs {
  /** Absolute path to the file to explain. */
  path: string;
  /** Optional 1-indexed start line (inclusive). */
  line_start?: number | null;
  /** Optional 1-indexed end line (inclusive). */
  line_end?: number | null;
  /** "beginner" (default) | "intermediate" | "expert". */
  audience?: ExplainAudience;
}

/**
 * Ask the gateway to explain the file (or the given line range) at the chosen
 * audience level. `line_start`/`line_end` are 1-indexed inclusive; omit them
 * (or pass `null`) to explain the whole file.
 */
export async function explainCode(args: ExplainArgs): Promise<ExplainResult> {
  return invoke<ExplainResult>("explain_code", {
    path: args.path,
    lineStart: args.line_start ?? null,
    lineEnd: args.line_end ?? null,
    audience: args.audience ?? null,
  });
}

/**
 * Persist a generated explanation into the Cortex Brain vault under
 * `~/Documents/Cortex Brain/explanations/<YYYY-MM-DD>-<slug>.md`. YAML
 * frontmatter records source/language/audience/range so memory walkers can
 * surface explanations without re-parsing the filename.
 */
export async function saveExplanation(args: {
  path: string;
  line_start: number | null;
  line_end: number | null;
  language: string;
  audience: string;
  markdown: string;
}): Promise<ExplainSaveResult> {
  return invoke<ExplainSaveResult>("save_explanation", {
    path: args.path,
    lineStart: args.line_start,
    lineEnd: args.line_end,
    language: args.language,
    audience: args.audience,
    markdown: args.markdown,
  });
}
