/**
 * Helpers for the inline CodeMirror editor pane.
 *
 * The editor itself lives in `components/EditorPane.tsx`. Anything that
 * just needs to *open* a file in that pane (the file explorer, slash
 * commands, the command palette…) should go through one of the helpers
 * here so the wiring stays in one place.
 */
import { useCortexStore } from "@/state/store";

/**
 * Custom-event name dispatched on `window` to ask the editor pane to
 * open a given file. The pane also listens for store changes, but the
 * event lets callers in non-React land (or that don't want to import
 * the store) trigger an open without touching state directly.
 *
 * Payload shape: `{ detail: { path: string } }`.
 */
export const EDITOR_OPEN_EVENT = "cortex:editor-open";

export interface EditorOpenDetail {
  path: string;
}

/**
 * Programmatically open a file in the inline editor pane.
 *
 * This is a fire-and-forget helper — it (a) flips the activity panel
 * to the editor tab, (b) sets `editorPath` in the store, and (c)
 * broadcasts a `cortex:editor-open` window event so any non-React
 * subscribers also get a chance to react.
 */
export function openInEditor(path: string): void {
  const trimmed = path?.trim?.() ?? "";
  if (!trimmed) return;
  const store = useCortexStore.getState();
  store.openEditorPath(trimmed);
  if (store.activityTab !== "editor") {
    store.setActivityTab("editor");
  }
  try {
    window.dispatchEvent(
      new CustomEvent<EditorOpenDetail>(EDITOR_OPEN_EVENT, {
        detail: { path: trimmed },
      }),
    );
  } catch {
    /* not in a browser-like env — ignore */
  }
}

/** Close the editor pane (clears the path, leaves the tab as-is). */
export function closeEditor(): void {
  useCortexStore.getState().openEditorPath(null);
}
