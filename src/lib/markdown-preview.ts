/**
 * Markdown preview pane helpers.
 *
 * The preview itself is rendered inside `EditorPane` (next to the
 * CodeMirror host) by the `MarkdownPreview` component. This module is
 * the small coordination layer that lets non-React callers — chiefly
 * the `/preview` slash command — flip the pane on/off without reaching
 * into the editor's local React state.
 *
 * Wire-up:
 *   - `EditorPane` listens for `MARKDOWN_PREVIEW_TOGGLE_EVENT` and
 *     updates its `showPreview` state when fired.
 *   - Callers dispatch via `toggleMarkdownPreview()` (toggle) or
 *     `setMarkdownPreview(true|false)` (explicit set).
 *
 * Keeping the surface tiny keeps the contract obvious for future
 * callers (command palette, keyboard shortcut, etc.).
 */

/** Window event name fired to toggle/set the preview pane. */
export const MARKDOWN_PREVIEW_TOGGLE_EVENT = "cortex:markdown-preview-toggle";

/**
 * Payload for the toggle event.
 *
 *   - `mode: "toggle"` flips whatever the pane is currently showing.
 *   - `mode: "set"` forces it to the boolean in `value`. Used by the
 *     editor itself when the open file stops being a markdown file
 *     (we want preview off rather than carrying stale state).
 */
export interface MarkdownPreviewToggleDetail {
  mode: "toggle" | "set";
  value?: boolean;
}

/** File extensions the preview pane treats as markdown. */
const MARKDOWN_EXTS = new Set(["md", "markdown", "mdx", "mdown", "mkd"]);

/**
 * Whether the given path looks like a markdown file. Centralised so the
 * EditorPane toggle button and the slash command agree on what counts.
 */
export function isMarkdownPath(path: string | null | undefined): boolean {
  if (!path) return false;
  const lastDot = path.lastIndexOf(".");
  if (lastDot < 0) return false;
  const ext = path.slice(lastDot + 1).toLowerCase();
  return MARKDOWN_EXTS.has(ext);
}

function dispatch(detail: MarkdownPreviewToggleDetail): void {
  try {
    window.dispatchEvent(
      new CustomEvent<MarkdownPreviewToggleDetail>(
        MARKDOWN_PREVIEW_TOGGLE_EVENT,
        { detail },
      ),
    );
  } catch {
    /* not in a browser-like env — ignore */
  }
}

/** Flip the preview pane (only takes effect if a markdown file is open). */
export function toggleMarkdownPreview(): void {
  dispatch({ mode: "toggle" });
}

/** Explicitly show or hide the preview pane. */
export function setMarkdownPreview(value: boolean): void {
  dispatch({ mode: "set", value });
}
