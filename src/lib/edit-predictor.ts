/**
 * Zed-style edit-prediction extension for CodeMirror 6.
 *
 * After the user makes a meaningful edit (>= 2 chars changed) and pauses for
 * {@link IDLE_MS}, we ship the changed line's before/after plus the current
 * file body to the `predict_next_edit` Tauri command. The backend asks the
 * LLM to spot OTHER lines that should likely receive the same edit and we
 * render each high-confidence suggestion as a per-line ghost block:
 *
 *   - strikethrough on the original line
 *   - a new "ghost-add" line directly below showing the proposed text
 *
 * Multiple suggestions can be live at once. Tab on a suggestion's line
 * accepts it (replaces the line with the proposed text), Escape clears ALL
 * pending suggestions on that line.
 *
 * Coexists with `inlineAutocomplete`:
 *   - inline-autocomplete → ghost AT the cursor
 *   - edit-predictor      → ghost ELSEWHERE in the file
 *
 * Both safely add as separate extensions. They use different StateFields and
 * disjoint decoration positions, so there is no rendering conflict.
 */
import { StateEffect, StateField, type Extension } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type PluginValue,
  type ViewUpdate,
  WidgetType,
  keymap,
} from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";

import { useCortexStore } from "@/state/store";

/** Idle pause after an edit before we call the backend. */
const IDLE_MS = 1500;
/** Minimum character delta on a single line to bother asking. */
const MIN_DELTA = 2;
/** Don't ship enormous files — backend also caps but trim early. */
const MAX_BODY_CHARS = 64_000;

export interface EditSuggestion {
  /** 1-indexed line number from the backend. */
  line: number;
  original: string;
  suggested: string;
  confidence: number;
  reason?: string;
}

interface RecentEdit {
  line: number; // 1-indexed
  before: string;
  after: string;
}

// --- effects --------------------------------------------------------------

/** Replace the full suggestion set. */
const setSuggestions = StateEffect.define<EditSuggestion[]>();
/** Drop suggestions targeting a specific 1-indexed line. */
const dropSuggestionForLine = StateEffect.define<number>();
/** Drop everything (Escape, doc-reload, etc.). */
const clearAllSuggestions = StateEffect.define<null>();

// --- ghost widgets --------------------------------------------------------

class StrikeWidget extends WidgetType {
  // Stacked-block widget rendered AFTER the original line: shows the
  // proposed replacement as a ghost "+" line. We intentionally don't try
  // to live-strikethrough the original (mark decoration over the full line
  // is unreliable when the user is mid-typing); instead we use a CSS class
  // on the line itself via `lineDeco` below.
  constructor(readonly suggested: string, readonly reason: string) {
    super();
  }
  override eq(other: StrikeWidget): boolean {
    return other.suggested === this.suggested && other.reason === this.reason;
  }
  override toDOM(): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "cm-edit-predict-ghost";
    const add = document.createElement("span");
    add.className = "cm-edit-predict-ghost-add";
    add.textContent = this.suggested;
    wrap.appendChild(add);
    if (this.reason) {
      const why = document.createElement("span");
      why.className = "cm-edit-predict-ghost-reason";
      why.textContent = `  ⤷ ${this.reason} — Tab to accept`;
      wrap.appendChild(why);
    } else {
      const hint = document.createElement("span");
      hint.className = "cm-edit-predict-ghost-reason";
      hint.textContent = "  ⤷ Tab to accept";
      wrap.appendChild(hint);
    }
    return wrap;
  }
  override ignoreEvent(): boolean {
    return true;
  }
}

// --- state field ---------------------------------------------------------

const suggestionsField = StateField.define<EditSuggestion[]>({
  create: () => [],
  update(value, tr) {
    let next = value;
    for (const e of tr.effects) {
      if (e.is(setSuggestions)) next = e.value;
      else if (e.is(clearAllSuggestions)) next = [];
      else if (e.is(dropSuggestionForLine)) {
        next = next.filter((s) => s.line !== e.value);
      }
    }
    // If the doc length changed and we still have suggestions, sanity-check
    // that each suggestion's `original` still exists at the recorded line.
    // Suggestions whose line drifted (insert/delete above them) are dropped
    // — re-firing the predictor on the next idle window will resurface them
    // at the correct position.
    if (next.length > 0 && tr.docChanged) {
      const doc = tr.state.doc;
      next = next.filter((s) => {
        if (s.line < 1 || s.line > doc.lines) return false;
        const lineText = doc.line(s.line).text;
        return lineText === s.original;
      });
    }
    return next;
  },
  provide: (f) =>
    EditorView.decorations.from(f, (suggestions) => buildDecorations(suggestions)),
});

function buildDecorations(suggestions: EditSuggestion[]): DecorationSet {
  if (suggestions.length === 0) return Decoration.none;
  // Sort by line so the decoration set is in document order — CM requires
  // ascending `from`.
  const sorted = [...suggestions].sort((a, b) => a.line - b.line);
  const ranges = sorted
    .map((s) => {
      // Decorations are built relative to a snapshot doc length we don't
      // have here, so we return placeholders that the EditorView resolves
      // via the line index. We use `Decoration.line` to mark the original
      // and a widget AFTER the line to render the proposed text.
      return s;
    });
  // We can't reach into `EditorView.state.doc` from `EditorView.decorations.from`'s
  // callback (it gets only the field value). Instead we encode positions
  // lazily by returning a function-decorations? Not supported. So we use
  // a wrapper plugin (see below) to compute concrete ranges.
  // To keep things simple, return Decoration.none here and let
  // `decorationPlugin` provide the real decorations from the view.
  void ranges;
  return Decoration.none;
}

// A view-aware plugin to build per-line decorations from the suggestions
// field. We use `Decoration.line` (class on the line) plus a widget block
// inserted right after the line.
const decorationPlugin = ViewPlugin.fromClass(
  class implements PluginValue {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = this.build(view);
    }
    update(u: ViewUpdate): void {
      const prev = u.startState.field(suggestionsField, false) ?? [];
      const curr = u.state.field(suggestionsField, false) ?? [];
      if (u.docChanged || u.viewportChanged || prev !== curr) {
        this.decorations = this.build(u.view);
      }
    }
    private build(view: EditorView): DecorationSet {
      const list = view.state.field(suggestionsField, false) ?? [];
      if (list.length === 0) return Decoration.none;
      const doc = view.state.doc;
      const items: { from: number; to: number; deco: Decoration }[] = [];
      for (const s of list) {
        if (s.line < 1 || s.line > doc.lines) continue;
        const ln = doc.line(s.line);
        items.push({
          from: ln.from,
          to: ln.from,
          deco: Decoration.line({ class: "cm-edit-predict-strike" }),
        });
        items.push({
          from: ln.to,
          to: ln.to,
          deco: Decoration.widget({
            widget: new StrikeWidget(s.suggested, s.reason ?? ""),
            side: 1,
            block: true,
          }),
        });
      }
      items.sort((a, b) => a.from - b.from || a.to - b.to);
      const builder = items.map((it) => it.deco.range(it.from, it.to));
      return Decoration.set(builder, true);
    }
  },
  { decorations: (p) => p.decorations },
);

// --- view plugin: detect edits, debounce, fetch --------------------------

function makeFetcher() {
  return ViewPlugin.fromClass(
    class implements PluginValue {
      private timer: number | null = null;
      private generation = 0;
      private pending: RecentEdit | null = null;
      private view: EditorView;

      constructor(view: EditorView) {
        this.view = view;
      }

      update(u: ViewUpdate): void {
        if (!u.docChanged) return;
        // Skip transactions we triggered ourselves when accepting a
        // suggestion — they're tagged with `userEvent: "edit-predict.accept"`.
        const isSelfApply = u.transactions.some((t) =>
          t.isUserEvent("edit-predict.accept"),
        );
        if (isSelfApply) return;
        // Heuristic: look at the LAST changed line in the new doc; capture
        // its before/after by diffing the two states.
        const edit = captureRecentEdit(u);
        if (!edit) return;
        // Fire on a "meaningful" edit: at least MIN_DELTA characters actually
        // changed on the line. Use the size of the edited region (the span
        // left after stripping the common prefix/suffix) rather than the raw
        // length delta, so a same-length replacement like `foo`→`bar` (3 chars
        // changed, delta 0) still triggers while a single-keystroke fix like
        // `foo`→`fox` (1 char changed) is skipped to avoid being chatty.
        if (changedChars(edit.before, edit.after) < MIN_DELTA) {
          return;
        }
        this.pending = edit;
        this.scheduleFetch();
      }

      destroy(): void {
        if (this.timer !== null) window.clearTimeout(this.timer);
      }

      private scheduleFetch(): void {
        if (this.timer !== null) window.clearTimeout(this.timer);
        const gen = ++this.generation;
        this.timer = window.setTimeout(() => {
          void this.fetch(gen);
        }, IDLE_MS);
      }

      private async fetch(gen: number): Promise<void> {
        if (gen !== this.generation) return;
        const edit = this.pending;
        this.pending = null;
        if (!edit) return;
        const path = useCortexStore.getState().editorPath;
        if (!path) return;
        const full = this.view.state.doc.toString();
        // When the file is too large we ship only the tail. The backend
        // numbers lines relative to the body it receives, so we must record
        // how many full-document lines were dropped from the head and add
        // that offset back to every returned line number (and translate the
        // recent-edit line into body-local coordinates). Truncating on a
        // line boundary keeps the offset exact.
        const { body, lineOffset } = truncateTail(full, MAX_BODY_CHARS);
        // If the edited line falls inside the dropped head we can't anchor
        // the request to the truncated body — skip rather than mislead.
        const bodyEditLine = edit.line - lineOffset;
        if (bodyEditLine < 1) return;
        let suggestions: EditSuggestion[] = [];
        try {
          suggestions = await invoke<EditSuggestion[]>("predict_next_edit", {
            args: {
              path,
              recent_edit: {
                line: bodyEditLine,
                before: edit.before,
                after: edit.after,
              },
              file_body: body,
            },
          });
        } catch {
          return;
        }
        if (gen !== this.generation) return;
        if (!Array.isArray(suggestions) || suggestions.length === 0) {
          this.view.dispatch({ effects: clearAllSuggestions.of(null) });
          return;
        }
        // Re-base body-local line numbers onto the full document so the
        // suggestions land on the right lines.
        const rebased =
          lineOffset === 0
            ? suggestions
            : suggestions.map((s) => ({ ...s, line: s.line + lineOffset }));
        this.view.dispatch({ effects: setSuggestions.of(rebased) });
      }
    },
  );
}

/** Trim a document to at most `maxChars` from the TAIL, snapping the cut to
 *  a line boundary. Returns the trimmed body plus `lineOffset` — the count of
 *  whole lines dropped from the head — so the caller can translate between
 *  body-local (1-indexed) and full-document line numbers. When no truncation
 *  is needed, `lineOffset` is 0. */
function truncateTail(
  full: string,
  maxChars: number,
): { body: string; lineOffset: number } {
  if (full.length <= maxChars) return { body: full, lineOffset: 0 };
  // Take the last `maxChars`, then advance past the (likely partial) first
  // line so the body starts cleanly at a line boundary.
  let cut = full.length - maxChars;
  const nl = full.indexOf("\n", cut);
  // If there's a newline at/after the raw cut, start just after it; otherwise
  // the tail is a single long line — keep it as-is.
  if (nl !== -1) cut = nl + 1;
  const body = full.slice(cut);
  // Lines dropped = number of newlines in the discarded head. The body's
  // line 1 corresponds to full-document line (lineOffset + 1).
  let lineOffset = 0;
  for (let i = 0; i < cut; i++) {
    if (full.charCodeAt(i) === 10 /* \n */) lineOffset++;
  }
  return { body, lineOffset };
}

/** Count how many characters actually changed between `before` and `after` by
 *  stripping the shared prefix and suffix and taking the larger of the two
 *  remaining (edited) spans. `foo`→`bar` → 3, `foo`→`fox` → 1, `foo`→`foobar`
 *  → 3. */
function changedChars(before: string, after: string): number {
  if (before === after) return 0;
  const aLen = before.length;
  const bLen = after.length;
  let prefix = 0;
  const minLen = Math.min(aLen, bLen);
  while (prefix < minLen && before[prefix] === after[prefix]) prefix++;
  let suffix = 0;
  while (
    suffix < minLen - prefix &&
    before[aLen - 1 - suffix] === after[bLen - 1 - suffix]
  ) {
    suffix++;
  }
  return Math.max(aLen, bLen) - prefix - suffix;
}

/** Walk the transaction changes and return the single edited line's
 *  before/after. Returns null when the edit spans multiple lines or when we
 *  can't unambiguously identify a single line. */
function captureRecentEdit(u: ViewUpdate): RecentEdit | null {
  // A batched ViewUpdate can carry multiple transactions. Reading `before`
  // from the first transaction's start state and `after` from the last's end
  // state only lines up when a single line number is valid in BOTH coordinate
  // spaces; an earlier transaction that inserts/removes lines above the edited
  // line shifts that line, so the same lineNo would describe unrelated lines.
  // Rather than report a misleading before/after pair, only handle the single-
  // transaction case here and skip multi-transaction batches.
  if (u.transactions.length !== 1) return null;
  // We track the edited line as a plain number so TS doesn't narrow the
  // closure-captured local to `never` when the iterChanges callback writes
  // to it. -1 means "not yet seen".
  let editedLine = -1;
  let multi = false;
  for (const tr of u.transactions) {
    tr.changes.iterChanges((fromA, toA) => {
      if (multi) return;
      const beforeDoc = tr.startState.doc;
      const startLine = beforeDoc.lineAt(fromA).number;
      const endLine = beforeDoc.lineAt(toA).number;
      if (startLine !== endLine) {
        multi = true;
        return;
      }
      if (editedLine !== -1 && editedLine !== startLine) {
        multi = true;
        return;
      }
      editedLine = startLine;
    });
  }
  if (multi || editedLine === -1) return null;
  const lineNo = editedLine;
  // Best-effort: read the line from the FIRST transaction's start state and
  // the LAST transaction's end state.
  const first = u.transactions[0];
  const last = u.transactions[u.transactions.length - 1];
  if (!first || !last) return null;
  const beforeDoc = first.startState.doc;
  const afterDoc = last.state.doc;
  if (lineNo < 1 || lineNo > beforeDoc.lines || lineNo > afterDoc.lines) {
    return null;
  }
  const before = beforeDoc.line(lineNo).text;
  const after = afterDoc.line(lineNo).text;
  if (before === after) return null;
  return { line: lineNo, before, after };
}

// --- keymap: Tab accept / Escape dismiss ---------------------------------

function acceptOnCursorLine(view: EditorView): boolean {
  const list = view.state.field(suggestionsField, false);
  if (!list || list.length === 0) return false;
  const head = view.state.selection.main.head;
  const cursorLine = view.state.doc.lineAt(head).number;
  const match = list.find((s) => s.line === cursorLine);
  if (!match) return false;
  const ln = view.state.doc.line(match.line);
  view.dispatch({
    changes: { from: ln.from, to: ln.to, insert: match.suggested },
    effects: dropSuggestionForLine.of(match.line),
    userEvent: "edit-predict.accept",
  });
  return true;
}

function dismissAll(view: EditorView): boolean {
  const list = view.state.field(suggestionsField, false);
  if (!list || list.length === 0) return false;
  view.dispatch({ effects: clearAllSuggestions.of(null) });
  return true;
}

const predictorKeymap = keymap.of([
  { key: "Tab", run: acceptOnCursorLine },
  { key: "Escape", run: dismissAll },
]);

// --- public extension ----------------------------------------------------

/** Build the edit-predictor extension. Drop into the CM `extensions` array
 *  alongside `inlineAutocomplete` — they coexist without conflict. */
export function editPredictor(): Extension {
  return [suggestionsField, decorationPlugin, makeFetcher(), predictorKeymap];
}
