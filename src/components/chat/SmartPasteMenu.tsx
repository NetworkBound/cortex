export interface SmartPasteState {
  /** The original pasted blob (so actions can wrap/trim it on demand). */
  pasted: string;
  /** Where it landed in the composer text; actions rewrite this slice. */
  start: number;
  end: number;
  language: string;
}

/**
 * Floating action strip under the textarea after a large paste: wrap in a
 * fence, trim whitespace, save as a snippet, or keep as-is. Dismissing is a
 * true no-op — the browser already inserted the text.
 */
export function SmartPasteMenu({
  paste,
  onFence,
  onTrim,
  onSaveSnippet,
  onDismiss,
}: {
  paste: SmartPasteState;
  onFence: () => void;
  onTrim: () => void;
  onSaveSnippet: () => void;
  onDismiss: () => void;
}) {
  return (
    <div
      className="smart-paste-menu"
      role="menu"
      aria-label="Pasted text actions"
      onMouseDown={(e) => e.preventDefault() /* keep textarea focus */}
    >
      <span className="smart-paste-label">
        Pasted {paste.pasted.length} chars
        {paste.language && ` · ${paste.language}`}
      </span>
      <button
        type="button"
        role="menuitem"
        className="smart-paste-action"
        onClick={onFence}
        title="Wrap in code fence"
      >
        fence{paste.language ? ` (${paste.language})` : ""}
      </button>
      <button
        type="button"
        role="menuitem"
        className="smart-paste-action"
        onClick={onTrim}
        title="Collapse blank lines and trailing whitespace"
      >
        trim
      </button>
      <button
        type="button"
        role="menuitem"
        className="smart-paste-action"
        onClick={onSaveSnippet}
        title="Save the pasted text as a reusable snippet"
      >
        save snippet
      </button>
      <button
        type="button"
        role="menuitem"
        className="smart-paste-action smart-paste-dismiss"
        onClick={onDismiss}
        title="Keep paste as-is (Esc)"
      >
        as-is
      </button>
    </div>
  );
}
