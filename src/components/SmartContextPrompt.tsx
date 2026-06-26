// Small floating card above the chat composer that shows AI-suggested
// `@`-tokens the user might want to attach to their draft. Each suggestion
// renders as a clickable chip — clicking inserts the token at the textarea
// cursor; "Add all" inserts every token in one shot.
//
// Auto-dismisses after 30s of being open. The parent (`ChatPane`) owns the
// `suggestions` array; this component is purely presentational + emits the
// insert/dismiss events back.

import { useEffect } from "react";
import {
  suggestionLabel,
  suggestionToToken,
  type ContextSuggestion,
} from "@/lib/context-picker";

/** Wall-clock auto-dismiss. Matches the picker's effective TTL — keeps
 *  the card from lingering when the user moves on without acting on it. */
const AUTO_DISMISS_MS = 30_000;

interface Props {
  suggestions: ContextSuggestion[];
  /** Insert a single `@token` at the textarea cursor. */
  onInsert: (token: string) => void;
  /** Insert every suggested `@token`, space-joined, at the cursor. */
  onInsertAll: (tokens: string[]) => void;
  /** Close the card (Esc, dismiss button, or auto-dismiss timer). */
  onDismiss: () => void;
}

export function SmartContextPrompt({
  suggestions,
  onInsert,
  onInsertAll,
  onDismiss,
}: Props) {
  // Auto-dismiss timer. Resets every time the suggestion list changes so
  // a fresh "/ctx" re-run gets a full 30 seconds of attention.
  useEffect(() => {
    if (suggestions.length === 0) return;
    const handle = window.setTimeout(onDismiss, AUTO_DISMISS_MS);
    return () => window.clearTimeout(handle);
  }, [suggestions, onDismiss]);

  // Esc closes the card. Capture-phase so we don't fight other keydown
  // handlers (e.g. the FilePicker) registered on inner inputs.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        onDismiss();
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onDismiss]);

  if (suggestions.length === 0) return null;

  return (
    <div
      className="smart-context-prompt"
      role="dialog"
      aria-label="Suggested context attachments"
      // Prevent click-outside-style focus loss; the textarea should keep
      // its caret position so insertions land at the right offset.
      onMouseDown={(e) => e.preventDefault()}
    >
      <div className="smart-context-prompt-header">
        <span className="smart-context-prompt-title">
          Suggested context ({suggestions.length})
        </span>
        <button
          type="button"
          className="smart-context-prompt-dismiss"
          onClick={onDismiss}
          title="Dismiss (Esc)"
        >
          ×
        </button>
      </div>
      <div className="smart-context-prompt-chips">
        {suggestions.map((s, i) => (
          <button
            type="button"
            key={`${s.kind}-${s.value}-${i}`}
            className="smart-context-prompt-chip"
            onClick={() => onInsert(suggestionToToken(s))}
            title={s.reason}
          >
            <span className="smart-context-prompt-chip-label">
              {suggestionLabel(s)}
            </span>
            <span className="smart-context-prompt-chip-reason">{s.reason}</span>
            <span className="smart-context-prompt-chip-confidence">
              {Math.round(s.confidence * 100)}%
            </span>
          </button>
        ))}
      </div>
      <div className="smart-context-prompt-actions">
        <button
          type="button"
          className="smart-context-prompt-add-all"
          onClick={() =>
            onInsertAll(suggestions.map((s) => suggestionToToken(s)))
          }
        >
          Add all
        </button>
      </div>
    </div>
  );
}
