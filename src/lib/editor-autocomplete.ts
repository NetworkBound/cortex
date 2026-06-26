/**
 * Inline AI ghost-text autocomplete (Terax #14).
 *
 * Drops into CodeMirror's `extensions` array. After {@link IDLE_MS} of typing
 * pause we POST 50 lines before / 20 lines after the cursor to the
 * `inline_complete` Tauri command, then render the returned text as a gray
 * ghost widget at the cursor.
 *
 *   - `Tab`     accepts (inserts the suggestion into the buffer)
 *   - `Escape`  dismisses
 *   - any other keystroke supersedes the previous suggestion (it disappears
 *     and a new one is scheduled by the next idle timer)
 *
 * Lifecycle is tracked with two `StateEffect`s and one `StateField`. The
 * actual fetch + decoration plumbing lives in a `ViewPlugin`.
 *
 * The extension is safe to add to a read-only editor — `editable === false`
 * means `keydown` events don't fire, so we simply never produce ghosts.
 */
import { StateEffect, StateField, type Extension } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  ViewPlugin,
  type PluginValue,
  type ViewUpdate,
  WidgetType,
  keymap,
} from "@codemirror/view";
import { invoke } from "@tauri-apps/api/core";

/** Idle window before we ask the backend for a suggestion. */
const IDLE_MS = 800;
/** Lines of context preceding the cursor we send to the backend. */
const LINES_BEFORE = 50;
/** Lines of context following the cursor we send to the backend. */
const LINES_AFTER = 20;

interface InlineCompleteResult {
  completion: string;
  latency_ms: number;
}

interface Suggestion {
  /** Document offset where the ghost should render. */
  pos: number;
  /** Text the user will accept when they press Tab. */
  text: string;
}

/** Effect: set / replace the current suggestion. */
const setSuggestion = StateEffect.define<Suggestion | null>();
/** Effect: dismiss the current suggestion (Escape, blur, doc change, …). */
const clearSuggestion = StateEffect.define<null>();

// --- ghost widget ---------------------------------------------------------

class GhostWidget extends WidgetType {
  constructor(readonly text: string) {
    super();
  }

  override eq(other: GhostWidget): boolean {
    return other.text === this.text;
  }

  override toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "cm-inline-ghost";
    // First line inline, remaining lines stacked below — keeps the cursor
    // visually anchored without the ghost wrapping mid-word.
    const lines = this.text.split("\n");
    span.appendChild(document.createTextNode(lines[0] ?? ""));
    for (let i = 1; i < lines.length; i += 1) {
      const br = document.createElement("br");
      span.appendChild(br);
      const cont = document.createElement("span");
      cont.className = "cm-inline-ghost-cont";
      cont.appendChild(document.createTextNode(lines[i]));
      span.appendChild(cont);
    }
    return span;
  }

  override ignoreEvent(): boolean {
    return true;
  }
}

// --- state field tracks the active suggestion -----------------------------

const suggestionField = StateField.define<Suggestion | null>({
  create: () => null,
  update(value, tr) {
    let next = value;
    for (const e of tr.effects) {
      if (e.is(setSuggestion)) next = e.value;
      else if (e.is(clearSuggestion)) next = null;
    }
    // Any document change other than the one that accepted the suggestion
    // invalidates it. We rely on the keymap handler to dispatch
    // `clearSuggestion` before inserting accepted text, so user-driven
    // edits drop the ghost here without re-firing the request loop.
    if (next && tr.docChanged) next = null;
    // Selection change also invalidates (cursor moved off the anchor).
    if (next && tr.selection && tr.selection.main.head !== next.pos) {
      next = null;
    }
    return next;
  },
  provide: (f) =>
    EditorView.decorations.from(f, (sug) => {
      if (!sug || !sug.text) return Decoration.none;
      return Decoration.set([
        Decoration.widget({
          widget: new GhostWidget(sug.text),
          side: 1,
        }).range(sug.pos),
      ]);
    }),
});

// --- view plugin: idle timer + backend fetch ------------------------------

interface CompleterOptions {
  /** Override language hint sent to the backend. Defaults to `undefined`. */
  language?: () => string | undefined;
}

function makePlugin(opts: CompleterOptions) {
  return ViewPlugin.fromClass(
    class implements PluginValue {
      private timer: number | null = null;
      private generation = 0;
      private view: EditorView;

      constructor(view: EditorView) {
        this.view = view;
      }

      update(u: ViewUpdate): void {
        // Any user-driven doc change or selection move restarts the timer.
        // The accept path tags its transaction with `userEvent:
        // "input.complete"` so we can ignore self-inflicted edits.
        if (!u.docChanged && !u.selectionSet) return;
        const isAccept = u.transactions.some(
          (t) => t.isUserEvent("input.complete"),
        );
        if (isAccept) return;
        // Existing field-level rules already drop the visible ghost on doc /
        // selection change — we just need to (re)schedule a new fetch.
        this.scheduleFetch();
      }

      destroy(): void {
        if (this.timer !== null) window.clearTimeout(this.timer);
      }

      private scheduleFetch(): void {
        if (this.timer !== null) window.clearTimeout(this.timer);
        const gen = ++this.generation;
        this.timer = window.setTimeout(() => this.fetch(gen), IDLE_MS);
      }

      private async fetch(gen: number): Promise<void> {
        if (gen !== this.generation) return;
        const view = this.view;
        const state = view.state;
        const pos = state.selection.main.head;
        if (state.selection.main.from !== state.selection.main.to) return;

        const { before, after } = sliceContext(state.doc.toString(), pos);
        if (!before.trim()) return;

        let result: InlineCompleteResult;
        try {
          result = await invoke<InlineCompleteResult>("inline_complete", {
            args: {
              before,
              after,
              language: opts.language?.() ?? null,
            },
          });
        } catch {
          return;
        }
        if (gen !== this.generation) return;
        const text = result.completion ?? "";
        if (!text) return;
        // Verify the cursor hasn't moved while we were waiting.
        const head = view.state.selection.main.head;
        if (head !== pos) return;

        view.dispatch({
          effects: setSuggestion.of({ pos, text }),
        });
      }
    }
  );
}

function sliceContext(doc: string, pos: number): { before: string; after: string } {
  const head = doc.slice(0, pos);
  const tail = doc.slice(pos);

  const beforeLines = head.split("\n");
  const before = beforeLines.slice(Math.max(0, beforeLines.length - LINES_BEFORE)).join("\n");

  const afterLines = tail.split("\n");
  const after = afterLines.slice(0, LINES_AFTER).join("\n");

  return { before, after };
}

// --- keymap: Tab accepts, Escape dismisses --------------------------------

function acceptCommand(view: EditorView): boolean {
  const sug = view.state.field(suggestionField, false);
  if (!sug || !sug.text) return false;
  view.dispatch({
    changes: { from: sug.pos, insert: sug.text },
    selection: { anchor: sug.pos + sug.text.length },
    effects: clearSuggestion.of(null),
    userEvent: "input.complete",
  });
  return true;
}

function dismissCommand(view: EditorView): boolean {
  const sug = view.state.field(suggestionField, false);
  if (!sug) return false;
  view.dispatch({ effects: clearSuggestion.of(null) });
  return true;
}

const inlineKeymap = keymap.of([
  { key: "Tab", run: acceptCommand },
  { key: "Escape", run: dismissCommand },
]);

// --- public extension -----------------------------------------------------

/**
 * Build the inline-autocomplete extension. Pass a `language` callback if you
 * want to send a custom language hint (e.g. `languageLabel(path)`); otherwise
 * the backend falls back to "plain text".
 */
export function inlineAutocomplete(opts: CompleterOptions = {}): Extension {
  return [suggestionField, makePlugin(opts), inlineKeymap];
}
