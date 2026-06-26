/**
 * Zed-style multibuffer panel.
 *
 * Renders N editable excerpts from N files stacked vertically, each in its
 * own CodeMirror view. Edits write back to the source files via the
 * existing `save_file_text` Tauri command — partial-file excerpts are
 * spliced into the live disk contents so other lines stay intact.
 *
 * Wiring:
 *   - Reads / writes `multibufferExcerpts` from `useCortexStore`.
 *   - Listens for `cortex:multibuffer-open` window events so callers
 *     (search / refactor / hunk-review) can replace the buffer.
 *   - Mod-S in any editor saves every dirty excerpt; per-excerpt Save
 *     button covers the single-file case.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { EditorState, type Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { bracketMatching, defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { oneDark } from "@codemirror/theme-one-dark";

import { languageForPath } from "@/lib/editor-langs";
import {
  MULTIBUFFER_OPEN_EVENT,
  addExcerpt,
  buildExcerpt,
  saveExcerpt,
  type MultibufferExcerpt,
  type MultibufferOpenDetail,
} from "@/lib/multibuffer";
import { pushToast } from "@/lib/toast";
import { confirmDialog } from "@/lib/dialogs";
import { pickFileWithRange } from "@/lib/quick-open";
import { useCortexStore } from "@/state/store";

/** Common CodeMirror extensions for a multibuffer cell. Mirrors the
 *  EditorPane base config but trimmed (no autocomplete / lint — those
 *  pull on the full editorPath state and aren't excerpt-aware). */
function cellExtensions(opts: {
  onDocChange: (doc: string) => void;
  onSaveAll: () => void;
}): Extension[] {
  return [
    lineNumbers(),
    highlightActiveLine(),
    history(),
    bracketMatching(),
    highlightSelectionMatches(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    keymap.of([
      {
        key: "Mod-s",
        preventDefault: true,
        run: () => {
          opts.onSaveAll();
          return true;
        },
      },
    ]),
    keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap]),
    EditorView.updateListener.of((u) => {
      if (u.docChanged) opts.onDocChange(u.state.doc.toString());
    }),
    oneDark,
    EditorView.theme({
      "&": { fontSize: "12.5px" },
      ".cm-scroller": {
        fontFamily: "var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
      },
    }),
  ];
}

/** Hook: mount a CodeMirror view inside `hostRef` with `initialBody`, and
 *  expose `getDoc()` for ad-hoc reads (used by the per-excerpt save). The
 *  view is re-mounted whenever `excerptId` changes — different excerpts
 *  get a fresh editor so undo history doesn't bleed between them. */
function useCellEditor(opts: {
  excerptId: string;
  path: string;
  initialBody: string;
  onDocChange: (doc: string) => void;
  onSaveAll: () => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const liveBodyRef = useRef<string>(opts.initialBody);

  useEffect(() => {
    liveBodyRef.current = opts.initialBody;
    let cancelled = false;
    (async () => {
      const host = hostRef.current;
      if (!host) return;
      const langExt = await languageForPath(opts.path);
      if (cancelled) return;
      const extensions: Extension[] = [
        ...cellExtensions({
          onDocChange: (doc) => {
            liveBodyRef.current = doc;
            opts.onDocChange(doc);
          },
          onSaveAll: opts.onSaveAll,
        }),
      ];
      if (langExt) extensions.push(langExt);
      viewRef.current?.destroy();
      const state = EditorState.create({ doc: opts.initialBody, extensions });
      viewRef.current = new EditorView({ state, parent: host });
    })();
    return () => {
      cancelled = true;
      viewRef.current?.destroy();
      viewRef.current = null;
    };
    // We intentionally re-init on excerpt id / path so a buffer swap
    // gives the user a fresh editor with no stale undo stack.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opts.excerptId, opts.path]);

  return { hostRef, liveBodyRef };
}

interface CellProps {
  excerpt: MultibufferExcerpt;
  onChange: (id: string, body: string) => void;
  onSave: (id: string) => Promise<void>;
  onRemove: (id: string) => void;
  onSaveAll: () => void;
}

function MultibufferCell({ excerpt, onChange, onSave, onRemove, onSaveAll }: CellProps) {
  const { hostRef } = useCellEditor({
    excerptId: excerpt.id,
    path: excerpt.path,
    initialBody: excerpt.body,
    onDocChange: (doc) => onChange(excerpt.id, doc),
    onSaveAll,
  });
  const filename = excerpt.path.split(/[/\\]/).pop() ?? excerpt.path;
  return (
    <div className="multibuffer-cell">
      <div className="multibuffer-cell-head">
        <span className="multibuffer-cell-path" title={excerpt.path}>
          {filename}
        </span>
        <span className="multibuffer-cell-range muted">
          {excerpt.start_line}-{excerpt.end_line}
        </span>
        <span className="multibuffer-cell-lang muted">{excerpt.language}</span>
        <span className="multibuffer-cell-spacer" />
        {excerpt.dirty ? (
          <span className="multibuffer-dirty" title="Unsaved changes">
            ●
          </span>
        ) : null}
        <button
          className="link-btn"
          onClick={() => {
            void onSave(excerpt.id);
          }}
          disabled={!excerpt.dirty}
          title="Save this excerpt"
        >
          Save
        </button>
        <button
          className="link-btn"
          onClick={() => onRemove(excerpt.id)}
          aria-label="Remove excerpt"
          title="Remove from multibuffer"
        >
          ×
        </button>
      </div>
      <div ref={hostRef} className="multibuffer-cell-host" />
    </div>
  );
}

/** Top-level multibuffer activity panel. */
export function MultiBuffer() {
  const excerpts = useCortexStore((s) => s.multibufferExcerpts);
  const setExcerpts = useCortexStore((s) => s.setMultibufferExcerpts);
  const [busy, setBusy] = useState<boolean>(false);

  // Track the live edited body for each excerpt outside React state so we
  // don't re-render on every keystroke. Per-excerpt dirty state lives in
  // the store so the header pill stays accurate.
  const liveBodiesRef = useRef<Map<string, string>>(new Map());
  useEffect(() => {
    // Diff by excerpt id: seed NEW excerpts, drop removed ones, and leave
    // existing entries alone. A blanket clear-and-reseed here would overwrite
    // the live (typed) body with the stale store body on every dirty-flag
    // patch — i.e. every keystroke — so Save would write pre-edit content.
    const live = liveBodiesRef.current;
    const ids = new Set(excerpts.map((e) => e.id));
    for (const id of Array.from(live.keys())) {
      if (!ids.has(id)) live.delete(id);
    }
    for (const e of excerpts) {
      if (!live.has(e.id)) live.set(e.id, e.body);
    }
  }, [excerpts]);

  function patchExcerpt(id: string, patch: Partial<MultibufferExcerpt>) {
    const next = useCortexStore.getState().multibufferExcerpts.map((e) =>
      e.id === id ? { ...e, ...patch } : e,
    );
    setExcerpts(next);
  }

  function onChange(id: string, body: string) {
    liveBodiesRef.current.set(id, body);
    const current = useCortexStore.getState().multibufferExcerpts.find((e) => e.id === id);
    if (!current) return;
    const isDirty = body !== current.body || current.dirty;
    // Only patch on dirty transition — keeps re-renders cheap.
    if (current.dirty !== isDirty || current.body !== body) {
      patchExcerpt(id, { dirty: true });
    }
  }

  async function onSave(id: string) {
    const current = useCortexStore.getState().multibufferExcerpts.find((e) => e.id === id);
    if (!current) return;
    const body = liveBodiesRef.current.get(id) ?? current.body;
    setBusy(true);
    try {
      const result = await saveExcerpt({ ...current, body });
      patchExcerpt(id, {
        body: result.body,
        start_line: result.start_line,
        end_line: result.end_line,
        dirty: false,
      });
      pushToast({ title: "Saved", body: current.path, kind: "success" });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setBusy(false);
    }
  }

  async function onSaveAll() {
    const all = useCortexStore.getState().multibufferExcerpts;
    const dirty = all.filter((e) => e.dirty);
    if (dirty.length === 0) {
      pushToast({ title: "Nothing to save", kind: "info" });
      return;
    }
    setBusy(true);
    let ok = 0;
    let fail = 0;
    for (const e of dirty) {
      const body = liveBodiesRef.current.get(e.id) ?? e.body;
      try {
        const result = await saveExcerpt({ ...e, body });
        patchExcerpt(e.id, {
          body: result.body,
          start_line: result.start_line,
          end_line: result.end_line,
          dirty: false,
        });
        ok += 1;
      } catch {
        fail += 1;
      }
    }
    setBusy(false);
    pushToast({
      title: fail === 0 ? "All saved" : `${ok} saved, ${fail} failed`,
      kind: fail === 0 ? "success" : "warning",
    });
  }

  async function onRemove(id: string) {
    const current = useCortexStore.getState().multibufferExcerpts.find((e) => e.id === id);
    if (
      current?.dirty &&
      !(await confirmDialog({
        title: "Discard unsaved changes?",
        message: `${current.path.split(/[/\\]/).pop()} has unsaved edits that will be lost.`,
        confirmLabel: "Discard",
        danger: true,
      }))
    ) {
      return;
    }
    liveBodiesRef.current.delete(id);
    setExcerpts(useCortexStore.getState().multibufferExcerpts.filter((e) => e.id !== id));
  }

  async function promptAdd() {
    // QuickOpen picker (recents / project search / pasted absolute path) with
    // an inline line-range field. Empty range = whole file — `buildExcerpt`
    // clamps the end to the real line count.
    const pick = await pickFileWithRange({ title: "Add excerpt" });
    if (!pick) return;
    setBusy(true);
    try {
      await addExcerpt(
        pick.path,
        pick.range?.start ?? 1,
        pick.range?.end ?? Number.MAX_SAFE_INTEGER,
      );
    } catch (e) {
      pushToast({ title: "Add excerpt failed", body: humanizeError(e), kind: "error" });
    } finally {
      setBusy(false);
    }
  }

  async function clearAll() {
    if (excerpts.length === 0) return;
    if (
      !(await confirmDialog({
        title: "Clear all excerpts?",
        message: `Clear ${excerpts.length} excerpt(s)? Unsaved edits will be lost.`,
        confirmLabel: "Clear",
        danger: true,
      }))
    ) {
      return;
    }
    liveBodiesRef.current.clear();
    setExcerpts([]);
  }

  // Listen for cortex:multibuffer-open. Replaces the current excerpt list
  // so callers (search/refactor/hunk-review) get a deterministic seed.
  useEffect(() => {
    async function onOpen(ev: Event) {
      const detail = (ev as CustomEvent<MultibufferOpenDetail>).detail;
      const incoming = Array.isArray(detail?.excerpts) ? detail.excerpts : [];
      // Replacing the buffer discards any unsaved excerpt edits — confirm
      // first, same contract as the editor's dirty guard.
      const dirtyCount = useCortexStore
        .getState()
        .multibufferExcerpts.filter((e) => e.dirty).length;
      if (
        dirtyCount > 0 &&
        !(await confirmDialog({
          title: "Replace the multibuffer?",
          message: `${dirtyCount} excerpt${dirtyCount === 1 ? " has" : "s have"} unsaved changes that will be lost.`,
          confirmLabel: "Replace",
          danger: true,
        }))
      ) {
        return;
      }
      setExcerpts(incoming);
    }
    window.addEventListener(MULTIBUFFER_OPEN_EVENT, onOpen as EventListener);
    return () =>
      window.removeEventListener(MULTIBUFFER_OPEN_EVENT, onOpen as EventListener);
  }, [setExcerpts]);

  // Window-level Mod-S inside the panel (covers the case where the user
  // pressed Ctrl+S while the toolbar — not an editor cell — held focus).
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const mod = e.ctrlKey || e.metaKey;
      if (mod && (e.key === "s" || e.key === "S")) {
        // Only act when the multibuffer tab is the active one — otherwise
        // we'd hijack Ctrl+S in other panels.
        if (useCortexStore.getState().activityTab !== "multibuffer") return;
        e.preventDefault();
        void onSaveAll();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // onSaveAll is closure-stable enough (reads via store.getState) so
    // we don't need to re-bind on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const dirtyCount = useMemo(() => excerpts.filter((e) => e.dirty).length, [excerpts]);

  return (
    <div className="multibuffer">
      <div className="multibuffer-toolbar">
        <button className="link-btn" onClick={() => void promptAdd()} disabled={busy}>
          + Add excerpt
        </button>
        <button className="link-btn" onClick={() => void onSaveAll()} disabled={busy || dirtyCount === 0}>
          Save all{dirtyCount > 0 ? ` (${dirtyCount})` : ""}
        </button>
        <button className="link-btn" onClick={clearAll} disabled={busy || excerpts.length === 0}>
          Clear all
        </button>
        <span className="multibuffer-toolbar-spacer" />
        <span className="muted multibuffer-toolbar-status">
          {excerpts.length === 0
            ? "empty"
            : `${excerpts.length} excerpt${excerpts.length === 1 ? "" : "s"}${
                dirtyCount > 0 ? ` · ${dirtyCount} dirty` : ""
              }`}
        </span>
      </div>
      <div className="multibuffer-body">
        {excerpts.length === 0 ? (
          <div className="multibuffer-empty">
            <div className="multibuffer-empty-title">No excerpts yet</div>
            <div className="multibuffer-empty-hint">
              Click <strong>+ Add excerpt</strong> or have a search / refactor agent route into
              this tab.
            </div>
          </div>
        ) : (
          excerpts.map((e) => (
            <MultibufferCell
              key={e.id}
              excerpt={e}
              onChange={onChange}
              onSave={onSave}
              onRemove={onRemove}
              onSaveAll={() => void onSaveAll()}
            />
          ))
        )}
      </div>
    </div>
  );
}

// Re-export library helpers so callers that already imported the component
// can grab `addExcerpt` / `buildExcerpt` without a second import line.
export { addExcerpt, buildExcerpt };
