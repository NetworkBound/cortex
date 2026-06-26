/**
 * Editor↔agent loop — selection plumbing for the two bridges out of the
 * editor pane (P0-FINAL Wave 1, reference-gaps audit):
 *
 *   - "Add selection to chat" (Ctrl/Cmd+L): drops an `@file:<path>:L<a>-L<b>`
 *     mention into the chat composer. The backend slices exactly those lines
 *     into the prompt (`split_line_range` in commands/chat.rs).
 *   - Inline assist (Ctrl/Cmd+I): the popover in `InlineAssist.tsx` calls
 *     `runInlineAssist` and applies the rewrite as a normal CodeMirror
 *     transaction — undo-able, dirty-tracked, never touches disk directly.
 */
import { invoke } from "@tauri-apps/api/core";
import type { EditorView } from "@codemirror/view";

import { pushToast } from "./toast";

export interface SelectionInfo {
  /** Exact selected text (empty string when the selection is a caret). */
  text: string;
  from: number;
  to: number;
  /** 1-based, inclusive. */
  startLine: number;
  endLine: number;
  empty: boolean;
}

export function selectionInfo(view: EditorView): SelectionInfo {
  const { from, to } = view.state.selection.main;
  const doc = view.state.doc;
  const startLine = doc.lineAt(from).number;
  // A selection ending exactly at a line start (common when dragging whole
  // lines) shouldn't claim the next line.
  const endLineRaw = doc.lineAt(to).number;
  const endLine =
    to > from && doc.lineAt(to).from === to ? endLineRaw - 1 : endLineRaw;
  return {
    text: view.state.sliceDoc(from, to),
    from,
    to,
    startLine,
    endLine,
    empty: from === to,
  };
}

/** Composer token for a selection: `@file:<path>:L<a>-L<b>` (single-line
 *  selections collapse to `:L<a>`; an empty selection mentions the whole
 *  file). */
export function selectionMention(path: string, sel: SelectionInfo): string {
  if (sel.empty) return `@file:${path}`;
  if (sel.startLine === sel.endLine) return `@file:${path}:L${sel.startLine}`;
  return `@file:${path}:L${sel.startLine}-L${sel.endLine}`;
}

/** Insert the selection mention into the chat composer and focus it. The
 *  composer lives in the always-visible chat column, so the editor stays
 *  open and the user keeps their place. */
export function addSelectionToChat(view: EditorView, path: string): boolean {
  if (!path) return false;
  const sel = selectionInfo(view);
  const token = selectionMention(path, sel);
  window.dispatchEvent(
    new CustomEvent("cortex:composer-insert", { detail: { value: token } }),
  );
  window.dispatchEvent(new CustomEvent("cortex:composer-focus"));
  const what = sel.empty
    ? "File"
    : sel.startLine === sel.endLine
      ? `Line ${sel.startLine}`
      : `Lines ${sel.startLine}–${sel.endLine}`;
  pushToast({
    title: `${what} attached to chat`,
    body: token,
    kind: "success",
  });
  return true;
}

/** Lines of context shipped around the selection — mirrors the ghost-text
 *  completion's window (50 before / 20 after). */
const CONTEXT_LINES_BEFORE = 50;
const CONTEXT_LINES_AFTER = 20;

export function assistContext(
  view: EditorView,
  sel: SelectionInfo,
): { before: string; after: string } {
  const doc = view.state.doc;
  const firstLine = doc.lineAt(sel.from).number;
  const lastLine = doc.lineAt(sel.to).number;
  const beforeFrom = doc.line(Math.max(1, firstLine - CONTEXT_LINES_BEFORE)).from;
  const afterTo = doc.line(Math.min(doc.lines, lastLine + CONTEXT_LINES_AFTER)).to;
  return {
    before: doc.sliceString(beforeFrom, sel.from),
    after: doc.sliceString(sel.to, afterTo),
  };
}

export interface InlineAssistResult {
  replacement: string;
  model: string;
  latency_ms: number;
}

export function runInlineAssist(args: {
  selection: string;
  before: string;
  after: string;
  language: string | null;
  instruction: string;
  model: string | null;
  path: string | null;
}): Promise<InlineAssistResult> {
  return invoke<InlineAssistResult>("inline_assist", { args });
}

export interface DiffRow {
  kind: "context" | "del" | "add";
  text: string;
}

/** Minimal line diff for the assist preview: common leading/trailing lines
 *  render as context, everything between as del/add blocks — exactly the
 *  shape a selection rewrite produces. Not a general LCS; doesn't need to
 *  be. */
export function diffLines(oldText: string, newText: string): DiffRow[] {
  const a = oldText.split("\n");
  const b = newText.split("\n");
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) {
    prefix++;
  }
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix++;
  }
  const rows: DiffRow[] = [];
  for (let i = 0; i < prefix; i++) rows.push({ kind: "context", text: a[i] });
  for (let i = prefix; i < a.length - suffix; i++) rows.push({ kind: "del", text: a[i] });
  for (let i = prefix; i < b.length - suffix; i++) rows.push({ kind: "add", text: b[i] });
  for (let i = a.length - suffix; i < a.length; i++) rows.push({ kind: "context", text: a[i] });
  return rows;
}
