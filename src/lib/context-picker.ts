// Smart-context auto-picker — frontend bridge.
//
// Wraps the `suggest_context` Tauri command. Given the user's draft message
// and the active project root, the backend asks the gateway which `@`-tokens the
// user should attach before sending. Returns at most 8 suggestions, each
// already filtered to confidence >= 0.5 by the Rust side.

import { invoke } from "@tauri-apps/api/core";

/** Mirrors `ContextSuggestion` in `src-tauri/src/commands/context_picker.rs`. */
export interface ContextSuggestion {
  /** One of `file`, `memory`, `recent`, `diff`, `problems`. */
  kind: ContextSuggestionKind;
  /** Path / id payload. Empty string for `diff` / `problems`. */
  value: string;
  /** One-line rationale shown beside the chip. */
  reason: string;
  /** Model-reported confidence in `[0.0, 1.0]`. */
  confidence: number;
}

export type ContextSuggestionKind =
  | "file"
  | "memory"
  | "recent"
  | "diff"
  | "problems";

interface ContextSuggestionsPayload {
  suggestions: ContextSuggestion[];
}

/**
 * Ask the backend to recommend `@`-tokens for the current draft. Throws on
 * gateway timeouts / invalid responses — callers should surface the error as
 * a toast and leave the textarea unchanged.
 */
export async function suggestContext(
  message: string,
  projectRoot: string | null | undefined,
): Promise<ContextSuggestion[]> {
  const payload = await invoke<ContextSuggestionsPayload>("suggest_context", {
    message,
    projectRoot: projectRoot ?? null,
  });
  return payload.suggestions ?? [];
}

/**
 * Format a suggestion as the `@`-token the composer should insert. Mirrors
 * the picker envelopes used by `at-vocab.ts`:
 *   - `file:src/foo.rs`
 *   - `memory:/abs/path/to/note.md`
 *   - `recent:<trace_id>`
 *   - `diff` / `problems` are bare (no `:value`).
 */
export function suggestionToToken(s: ContextSuggestion): string {
  switch (s.kind) {
    case "diff":
      return "@diff";
    case "problems":
      return "@problems";
    default:
      return `@${s.kind}:${s.value}`;
  }
}

/**
 * Short human label used as the chip text. Long file paths get a basename
 * fallback so the card stays compact.
 */
export function suggestionLabel(s: ContextSuggestion): string {
  if (s.kind === "diff") return "@diff";
  if (s.kind === "problems") return "@problems";
  const v = s.value;
  const trimmed = v.split(/[\\/]/).pop() || v;
  return `@${s.kind}:${trimmed}`;
}
