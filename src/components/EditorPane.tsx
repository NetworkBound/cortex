/**
 * Inline CodeMirror 6 editor pane.
 *
 * Read-only for v1 — opens the file at `editorPath` in the store and
 * renders it with syntax highlighting + line numbers. Editing /
 * persistence is a follow-up.
 *
 * Wiring:
 *   - Reads `editorPath` from `useCortexStore` and re-mounts the doc
 *     whenever it changes.
 *   - Also listens for a `cortex:editor-open` window event with
 *     `detail: { path }` so callers (file explorer, slash commands,
 *     etc.) can request an open without touching the store directly.
 */
import { useEffect, useRef, useState } from "react";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { searchKeymap, highlightSelectionMatches, openSearchPanel } from "@codemirror/search";
import { bracketMatching } from "@codemirror/language";
import { invoke } from "@tauri-apps/api/core";
import { readTextFile } from "@tauri-apps/plugin-fs";

import { Eye, MessageSquarePlus, Search, Sparkles, X } from "lucide-react";
import { humanizeError } from "@/lib/errors";

import { useCortexStore } from "@/state/store";
import { THEME_CHANGED_EVENT } from "@/lib/theme-engine";
import { cortexEditorTheme } from "@/lib/editor-theme";
import { getMemoryEntry } from "@/lib/memory";
import { pushToast } from "@/lib/toast";
import { EDITOR_OPEN_EVENT, type EditorOpenDetail } from "@/lib/editor";
import { addSelectionToChat, selectionInfo, type SelectionInfo } from "@/lib/editor-assist";
import { InlineAssist } from "./InlineAssist";
import { extOf, languageForPath, languageLabel } from "@/lib/editor-langs";
import { inlineAutocomplete } from "@/lib/editor-autocomplete";
import { editPredictor } from "@/lib/edit-predictor";
import { lintExtension } from "@/lib/editor-lint";
import { saveFileText } from "@/lib/editor-save";
import {
  MARKDOWN_PREVIEW_TOGGLE_EVENT,
  isMarkdownPath,
  type MarkdownPreviewToggleDetail,
} from "@/lib/markdown-preview";
import { MarkdownPreview } from "./MarkdownPreview";

/** Fetch the text body of a file using the best mechanism available.
 *
 *  Order:
 *    1. `read_file_text` Rust command (if the backend exposes one)
 *    2. `@tauri-apps/plugin-fs` `readTextFile` (capability-scoped)
 *    3. `get_memory_entry` (markdown-only fallback, returns `.body`) */
async function fetchFileBody(path: string): Promise<string> {
  // 1. Custom Rust command — silently swallow "command not found".
  try {
    const text = await invoke<string>("read_file_text", { path });
    if (typeof text === "string") return text;
  } catch {
    /* command may not exist; fall through */
  }

  // 2. plugin-fs readTextFile (works for anything in the fs:scope allowlist)
  try {
    const text = await readTextFile(path);
    return text;
  } catch (err) {
    // 3. Markdown-only fallback for paths the fs scope rejects.
    if (extOf(path) === "md" || extOf(path) === "markdown") {
      try {
        const entry = await getMemoryEntry(path);
        return entry.body ?? "";
      } catch {
        /* swallow — re-throw the original below */
      }
    }
    throw err;
  }
}

function baseExtensions(opts: {
  onDocChange: (doc: string) => void;
  onSave: () => void;
  onAddToChat: (view: EditorView) => void;
  onInlineAssist: (view: EditorView) => void;
  /** Theme-aware chrome + highlight style, pre-wrapped in a Compartment so
   *  the pane can reconfigure it live on app theme changes. */
  theme: Extension;
}): Extension[] {
  return [
    lineNumbers(),
    highlightActiveLine(),
    history(),
    bracketMatching(),
    highlightSelectionMatches(),
    // Ctrl/Cmd-S triggers save. Listed *before* defaultKeymap so it wins
    // over any default binding (defaultKeymap doesn't bind Mod-s today,
    // but we want to be explicit).
    keymap.of([
      {
        key: "Mod-s",
        preventDefault: true,
        run: () => {
          opts.onSave();
          return true;
        },
      },
      // Editor↔agent loop (P0-FINAL Wave 1): Ctrl/Cmd+L attaches the
      // selection to the chat composer as an `@file:…:L<a>-L<b>` mention;
      // Ctrl/Cmd+I opens the inline-assist popover on the selection.
      {
        key: "Mod-l",
        preventDefault: true,
        run: (view) => {
          opts.onAddToChat(view);
          return true;
        },
      },
      {
        key: "Mod-i",
        preventDefault: true,
        run: (view) => {
          opts.onInlineAssist(view);
          return true;
        },
      },
    ]),
    keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap]),
    // Dirty-state listener — fires on every doc-affecting transaction.
    EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        opts.onDocChange(update.state.doc.toString());
      }
    }),
    opts.theme,
    EditorView.theme({
      "&": { height: "100%", fontSize: "12.5px" },
      ".cm-scroller": { fontFamily: "var(--mono, ui-monospace, SFMono-Regular, Menlo, monospace)" },
    }),
  ];
}

export function EditorPane() {
  const editorPath = useCortexStore((s) => s.editorPath);
  const openPath = useCortexStore((s) => s.openEditorPath);

  const hostRef = useRef<HTMLDivElement | null>(null);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  // Theme compartment — lets the live view swap its dark/light flag and
  // highlight palette in place when the app theme changes (the pane is
  // keep-alive mounted, so a remount never happens on theme switch).
  const themeCompartmentRef = useRef(new Compartment());
  // `originalBodyRef` is the file contents at the most recent load/save —
  // dirty tracking compares the live doc against it. Stored in a ref so
  // we don't re-render on doc changes (just on dirty *transitions*).
  const originalBodyRef = useRef<string>("");
  // Live doc snapshot maintained by the updateListener. Lets the Mod-s
  // handler grab the current text without reaching into the view ref.
  const liveBodyRef = useRef<string>("");

  const [status, setStatus] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [error, setError] = useState<string | null>(null);
  const [dirty, setDirty] = useState<boolean>(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Preview pane is markdown-only; defaults off and auto-hides when the
  // open file isn't markdown. `previewSource` mirrors `liveBodyRef` but
  // as state so the preview re-renders on every keystroke.
  const [showPreview, setShowPreview] = useState<boolean>(false);
  const [previewSource, setPreviewSource] = useState<string>("");
  // Inline-assist popover: selection captured at open time plus its anchor
  // position relative to the editor body.
  const [assist, setAssist] = useState<{
    selection: SelectionInfo;
    top: number;
    left: number;
  } | null>(null);
  const isMarkdown = isMarkdownPath(editorPath);

  // Ctrl/Cmd+L — attach the current selection (or whole file when nothing is
  // selected) to the chat composer. Reads the path from the store at call
  // time so the keymap closure never goes stale.
  function handleAddToChat(view: EditorView) {
    const path = useCortexStore.getState().editorPath;
    if (!path) return;
    addSelectionToChat(view, path);
  }

  // Ctrl/Cmd+I — open the inline-assist popover anchored at the selection.
  function handleInlineAssist(view: EditorView) {
    const info = selectionInfo(view);
    if (info.empty) {
      pushToast({
        title: "Select code first",
        body: "Inline assist rewrites the selected code (Ctrl+I).",
        kind: "info",
      });
      return;
    }
    const rect = bodyRef.current?.getBoundingClientRect();
    const coords = view.coordsAtPos(info.from);
    let top = 8;
    let left = 8;
    if (rect && coords) {
      top = Math.min(
        Math.max(coords.bottom - rect.top + 6, 4),
        Math.max(8, rect.height - 240),
      );
      left = Math.min(
        Math.max(coords.left - rect.left, 4),
        Math.max(8, rect.width - 420),
      );
    }
    setAssist({ selection: info, top, left });
  }

  // Save the live buffer back to disk. Wired up via Mod-s and the
  // header save action below. Uses refs (not state) so the keymap
  // doesn't need to re-mount when the path or buffer changes.
  async function saveNow() {
    const path = useCortexStore.getState().editorPath;
    if (!path) return;
    const body = liveBodyRef.current;
    try {
      await saveFileText(path, body);
      originalBodyRef.current = body;
      setDirty(false);
      setSaveError(null);
    } catch (err) {
      setSaveError(humanizeError(err));
    }
  }

  // Mirror the local dirty flag into the store so `openEditorPath` can
  // confirm before replacing/closing a buffer with unsaved edits.
  useEffect(() => {
    useCortexStore.getState().setEditorDirty(dirty);
  }, [dirty]);

  // Re-derive the editor theme when the app theme changes. The chrome colors
  // are var(--token) references and re-resolve on their own; this swap is for
  // the parts CodeMirror bakes at construction time (the dark/light flag and
  // the syntax highlight palette). Applies in place to the live view, so
  // buffer contents, undo history, and scroll position all survive.
  useEffect(() => {
    function onThemeChanged() {
      const view = viewRef.current;
      if (!view) return;
      view.dispatch({
        effects: themeCompartmentRef.current.reconfigure(cortexEditorTheme()),
      });
    }
    window.addEventListener(THEME_CHANGED_EVENT, onThemeChanged);
    return () => window.removeEventListener(THEME_CHANGED_EVENT, onThemeChanged);
  }, []);

  // Listen for cortex:editor-open events from non-React callers.
  useEffect(() => {
    function onOpen(ev: Event) {
      const detail = (ev as CustomEvent<EditorOpenDetail>).detail;
      if (detail?.path) openPath(detail.path);
    }
    window.addEventListener(EDITOR_OPEN_EVENT, onOpen as EventListener);
    return () => window.removeEventListener(EDITOR_OPEN_EVENT, onOpen as EventListener);
  }, [openPath]);

  // Listen for `/preview` (and friends) — flips the preview pane when
  // the current file is markdown. Non-markdown files ignore the toggle
  // so we don't briefly show an empty pane.
  useEffect(() => {
    function onToggle(ev: Event) {
      const detail = (ev as CustomEvent<MarkdownPreviewToggleDetail>).detail;
      const path = useCortexStore.getState().editorPath;
      if (!isMarkdownPath(path)) {
        setShowPreview(false);
        return;
      }
      if (detail?.mode === "set") {
        setShowPreview(Boolean(detail.value));
      } else {
        setShowPreview((v) => !v);
      }
    }
    window.addEventListener(
      MARKDOWN_PREVIEW_TOGGLE_EVENT,
      onToggle as EventListener,
    );
    return () =>
      window.removeEventListener(
        MARKDOWN_PREVIEW_TOGGLE_EVENT,
        onToggle as EventListener,
      );
  }, []);

  // (Re)mount the CodeMirror view whenever `editorPath` changes.
  useEffect(() => {
    if (!editorPath) {
      // Tear down any existing view when path clears.
      viewRef.current?.destroy();
      viewRef.current = null;
      setStatus("idle");
      setError(null);
      setDirty(false);
      setSaveError(null);
      setAssist(null);
      setPreviewSource("");
      originalBodyRef.current = "";
      liveBodyRef.current = "";
      return;
    }
    // Auto-hide preview when the open file isn't markdown — keeps the
    // toggle from "remembering" a stale on-state across file switches.
    if (!isMarkdownPath(editorPath)) {
      setShowPreview(false);
    }

    let cancelled = false;
    setStatus("loading");
    setError(null);
    setDirty(false);
    setSaveError(null);
    // The popover's captured offsets are meaningless in a new buffer.
    setAssist(null);

    (async () => {
      let body = "";
      try {
        body = await fetchFileBody(editorPath);
      } catch (err) {
        if (cancelled) return;
        setStatus("error");
        setError(humanizeError(err));
        return;
      }
      const langExt = await languageForPath(editorPath);
      if (cancelled) return;

      // Seed both refs with the freshly-loaded body so dirty tracking
      // has a stable baseline.
      originalBodyRef.current = body;
      liveBodyRef.current = body;
      // Seed the preview state with the initial body so the right pane
      // renders the file's content immediately on first toggle.
      setPreviewSource(body);

      const pathIsMd = isMarkdownPath(editorPath);
      const extensions: Extension[] = [
        ...baseExtensions({
          onDocChange: (doc) => {
            liveBodyRef.current = doc;
            const isDirty = doc !== originalBodyRef.current;
            // Only call setDirty on transitions to avoid render churn
            // on every keystroke.
            setDirty((prev) => (prev === isDirty ? prev : isDirty));
            // Keep the markdown preview in sync. We pay the React
            // render cost only when the file is actually markdown — for
            // every other language the preview pane is hidden and
            // `previewSource` is unused.
            if (pathIsMd) setPreviewSource(doc);
          },
          onSave: () => {
            void saveNow();
          },
          onAddToChat: handleAddToChat,
          onInlineAssist: handleInlineAssist,
          theme: themeCompartmentRef.current.of(cortexEditorTheme()),
        }),
      ];
      if (langExt) extensions.push(langExt);
      extensions.push(inlineAutocomplete({ language: () => languageLabel(editorPath) }));
      extensions.push(editPredictor());
      extensions.push(lintExtension({}));

      // Replace any prior view.
      viewRef.current?.destroy();
      viewRef.current = null;

      const host = hostRef.current;
      if (!host) return;
      const state = EditorState.create({ doc: body, extensions });
      viewRef.current = new EditorView({ state, parent: host });
      setStatus("ready");
    })();

    return () => {
      cancelled = true;
    };
  }, [editorPath]);

  // Final teardown on unmount.
  useEffect(() => {
    return () => {
      viewRef.current?.destroy();
      viewRef.current = null;
    };
  }, []);

  if (!editorPath) {
    return (
      <div className="editor-pane editor-pane-empty">
        <div className="editor-pane-empty-inner">
          <svg
            className="editor-pane-empty-icon"
            width="28"
            height="28"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
            <polyline points="14 2 14 8 20 8" />
            <line x1="9" y1="13" x2="15" y2="13" />
            <line x1="9" y1="17" x2="13" y2="17" />
          </svg>
          <div className="editor-pane-empty-title">No file open</div>
          <div className="editor-pane-empty-hint">Click a file in the explorer to open it here</div>
        </div>
      </div>
    );
  }

  const filename = editorPath.split(/[/\\]/).pop() ?? editorPath;

  return (
    <div className="editor-pane">
      <div className="editor-pane-head">
        <span className="editor-pane-filename" title={editorPath}>
          {filename}
        </span>
        <span className="editor-pane-lang muted">{languageLabel(editorPath)}</span>
        <span className="editor-pane-spacer" />
        {saveError ? (
          <span
            className="editor-save-indicator editor-save-error"
            title={saveError}
          >
            save failed
          </span>
        ) : dirty ? (
          <span className="editor-save-indicator editor-save-dirty" title="Unsaved changes (Ctrl+S to save)">
            ● unsaved
          </span>
        ) : status === "ready" ? (
          <span className="editor-save-indicator editor-save-clean" title="All changes saved">
            ✓ saved
          </span>
        ) : null}
        <button
          className="link-btn editor-add-to-chat"
          onClick={() => {
            const view = viewRef.current;
            if (view) handleAddToChat(view);
          }}
          disabled={status !== "ready"}
          aria-label="Add selection to chat"
          title="Add selection to chat (Ctrl+L) — attaches the selected lines, or the whole file when nothing is selected"
        >
          <MessageSquarePlus size={14} strokeWidth={1.75} aria-hidden /> Chat
        </button>
        <button
          className="link-btn editor-assist-btn"
          onClick={() => {
            const view = viewRef.current;
            if (view) handleInlineAssist(view);
          }}
          disabled={status !== "ready"}
          aria-label="Inline assist"
          title="Inline assist (Ctrl+I) — rewrite the selected code with AI"
        >
          <Sparkles size={14} strokeWidth={1.75} aria-hidden /> Assist
        </button>
        {isMarkdown ? (
          <button
            className={
              showPreview
                ? "link-btn editor-preview-toggle is-on"
                : "link-btn editor-preview-toggle"
            }
            onClick={() => setShowPreview((v) => !v)}
            aria-pressed={showPreview}
            aria-label={showPreview ? "Hide preview" : "Show preview"}
            title={showPreview ? "Hide markdown preview" : "Show markdown preview"}
          >
            <Eye size={14} strokeWidth={1.75} aria-hidden /> Preview
          </button>
        ) : null}
        <button
          className="link-btn editor-find-btn"
          onClick={() => {
            const view = viewRef.current;
            if (view) {
              try {
                openSearchPanel(view);
                view.focus();
              } catch {
                // Fallback: synthesise Ctrl+F on the editor DOM so the bundled
                // keymap picks it up. Used when openSearchPanel ever gets
                // tree-shaken or fails for some reason.
                const target = view.dom;
                target.dispatchEvent(
                  new KeyboardEvent("keydown", {
                    key: "f",
                    code: "KeyF",
                    ctrlKey: true,
                    bubbles: true,
                  }),
                );
              }
            }
          }}
          aria-label="Find in editor"
          title="Find in editor (Ctrl+F)"
        >
          <Search size={14} strokeWidth={1.75} aria-hidden /> Find
        </button>
        <button
          className="link-btn"
          onClick={() => openPath(null)}
          aria-label="Close editor"
          title="Close"
        >
          <X size={14} strokeWidth={1.75} aria-hidden />
        </button>
      </div>
      <div
        ref={bodyRef}
        className={
          showPreview && isMarkdown
            ? "editor-pane-body editor-pane-body--split"
            : "editor-pane-body"
        }
      >
        {status === "loading" && (
          <div className="editor-pane-status muted">loading…</div>
        )}
        {status === "error" && (
          <div className="editor-pane-status editor-pane-error">
            failed to open: {error ?? "unknown error"}
          </div>
        )}
        <div
          ref={hostRef}
          className={
            showPreview && isMarkdown
              ? "editor-pane-host editor-pane-host--split"
              : "editor-pane-host"
          }
        />
        {showPreview && isMarkdown ? (
          <MarkdownPreview source={previewSource} />
        ) : null}
        {assist && viewRef.current ? (
          <InlineAssist
            view={viewRef.current}
            path={editorPath}
            language={languageLabel(editorPath)}
            selection={assist.selection}
            anchor={{ top: assist.top, left: assist.left }}
            onClose={() => setAssist(null)}
          />
        ) : null}
      </div>
    </div>
  );
}
