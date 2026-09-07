/**
 * Thin TS wrapper around the `generate_docs` Tauri command.
 *
 * Mirrors `src-tauri::commands::doc_gen::DocResult`. The backend caps the
 * source blob at 64 KiB and times out the gateway call at 45s — callers
 * should surface the error message returned from the promise rejection.
 */
import { invoke } from "@tauri-apps/api/core";

/** Canonical style keys understood by the backend `resolve_style` helper. */
export type DocStyle = "auto" | "rust" | "jsdoc" | "python" | "markdown" | "generic";

/**
 * Mirrors `src-tauri::commands::doc_gen::DocResult`. `style` is the
 * post-resolution canonical key (never the raw user input — the backend
 * folds "auto" / empty / unknown into the language default).
 */
export interface DocResult {
  path: string;
  language: string;
  style: string;
  original: string;
  with_docs: string;
  generated_unix_ms: number;
}

/**
 * Ask the gateway for a documented version of the given file. Pass `style` to
 * force a specific comment style; omit (or pass "auto") to let the backend
 * pick based on the file extension.
 *
 * @param path  Absolute path to the file the user wants documented.
 * @param style Optional style override — "auto" maps to the language default.
 */
export async function generateDocs(path: string, style?: DocStyle): Promise<DocResult> {
  const cleaned = style && style !== "auto" ? style : null;
  return invoke<DocResult>("generate_docs", { path, style: cleaned });
}
