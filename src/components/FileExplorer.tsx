/**
 * FileExplorer — virtualized file tree with fuzzy search + icon glyphs.
 *
 * Backend: reuses the existing `project_files(path, limit)` Tauri command via
 * `projectFiles()` from `@/lib/projects`. We cap to 1000 entries — the user
 * narrows further via the fuzzy filter at the top.
 *
 * Virtualization: hand-rolled (no `@tanstack/react-virtual` in this repo's
 * deps). We render only the visible window of rows + a small overscan buffer.
 * Tree is currently flat — `project_files` returns first-N entries from the
 * root, which is enough to feed @-mentions; full directory recursion is a
 * follow-up.
 *
 * Click handling: each row click dispatches a `cortex:composer-insert`
 * `CustomEvent` with `{ detail: { value: "@<filename>" } }` on `window`. The
 * composer (ChatPane.tsx, untouched here) can subscribe in a follow-up patch
 * to splice that into its input. Until then, the event is a no-op — but the
 * file explorer remains usable as a project browser.
 *
 * Fuzzy matching: "all chars in order, case-insensitive" — same algorithm as
 * the existing helpers in `at-vocab.ts` (which use `.toLowerCase().includes`
 * under the hood). Kept local to avoid pulling in unused fetchers.
 */

import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import {
  Copy,
  ExternalLink,
  Folder,
  FolderOpen,
  SquareTerminal,
} from "lucide-react";
import { projectFiles, type FileTreeEntry } from "@/lib/projects";
import { humanizeError } from "@/lib/errors";
import { FileIcon, DirIcon } from "@/lib/file-icons";
import {
  copyPath,
  describeOpenError,
  openInEditor,
  openTerminalHere,
  revealInFileManager,
} from "@/lib/open-external";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";

interface FileExplorerProps {
  /** Active project root path; null when no project is selected. */
  root: string | null;
  /** Display name of the active project (used in section header). */
  projectName?: string;
}

// Hard cap on entries fetched per refresh. The backend returns a flat list of
// the first-N entries under `root`; we ask for 1000 and rely on the search
// box for anything deeper.
const FETCH_LIMIT = 1000;

// Pixel height per row. MUST match `.file-explorer-row { height: … }` in CSS.
const ROW_HEIGHT = 24;
// Extra rows rendered above/below the visible window so fast scrolls don't
// flash blank space. 6 each side is plenty at 24px rows.
const OVERSCAN = 6;

/**
 * Subsequence fuzzy match — every character of `q` must appear in `name` in
 * order, case-insensitively. e.g. `flx` matches `FileExplorer.tsx`. Empty
 * query returns true (no filter).
 */
function fuzzyMatch(name: string, q: string): boolean {
  if (!q) return true;
  const hay = name.toLowerCase();
  const needle = q.toLowerCase();
  let i = 0;
  for (const ch of hay) {
    if (ch === needle[i]) i += 1;
    if (i === needle.length) return true;
  }
  return false;
}

/** Format a size in bytes as a short human label for the hover tooltip. */
function formatSize(bytes: number | null): string {
  if (bytes == null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Dispatch the composer-insert event. The composer (ChatPane) is responsible
 * for picking this up — we do NOT touch ChatPane.tsx from here. Until that
 * subscription lands, clicks are visually acknowledged but produce no
 * composer change; the event still fires so other listeners can hook in.
 */
export function emitComposerInsert(value: string): void {
  try {
    window.dispatchEvent(
      new CustomEvent("cortex:composer-insert", { detail: { value } }),
    );
  } catch {
    /* dispatch failures are non-fatal — file explorer still useful as a browser */
  }
}

/** Right-click menu for one entry (or the project root itself). Every action
 *  goes through lib/open-external; a backend without those commands reads as
 *  "not available in this build" instead of a raw IPC error. Paths are passed
 *  verbatim — Windows entries keep their backslashes. */
function entryMenuItems(target: {
  path: string;
  isDir: boolean;
}): ContextMenuItem[] {
  const fail =
    (title: string) =>
    (e: unknown): void => {
      pushToast({ title, body: describeOpenError(e), kind: "error" });
    };
  return [
    {
      id: "editor",
      label: target.isDir ? "Open folder in editor" : "Open in editor",
      icon: <ExternalLink size={13} strokeWidth={1.75} />,
      onSelect: () =>
        openInEditor(target.path)
          .then(() => undefined)
          .catch(fail("Couldn't open editor")),
    },
    {
      id: "reveal",
      label: "Reveal in file manager",
      icon: <FolderOpen size={13} strokeWidth={1.75} />,
      onSelect: () =>
        revealInFileManager(target.path).catch(fail("Couldn't reveal")),
    },
    {
      id: "terminal",
      label: "Open terminal here",
      icon: <SquareTerminal size={13} strokeWidth={1.75} />,
      onSelect: () =>
        openTerminalHere(target.path)
          .then(() => undefined)
          .catch(fail("Couldn't open terminal")),
    },
    {
      id: "copy",
      label: "Copy path",
      icon: <Copy size={13} strokeWidth={1.75} />,
      separatorBefore: true,
      onSelect: () =>
        copyPath(target.path)
          .then(() => {
            pushToast({
              title: "Path copied",
              body: target.path,
              kind: "success",
            });
          })
          .catch(fail("Couldn't copy path")),
    },
  ];
}

interface MenuState {
  x: number;
  y: number;
  label: string;
  items: ContextMenuItem[];
}

export function FileExplorer({ root, projectName }: FileExplorerProps) {
  const [files, setFiles] = useState<FileTreeEntry[]>([]);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);

  const openMenu = (
    e: React.MouseEvent,
    target: { path: string; isDir: boolean; name: string },
  ) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({
      x: e.clientX,
      y: e.clientY,
      label: target.name,
      items: entryMenuItems(target),
    });
  };
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(360);
  const scrollerRef = useRef<HTMLDivElement>(null);

  const fetchFiles = useCallback(
    async (signal?: AbortSignal) => {
      if (!root) {
        setFiles([]);
        setError(null);
        return;
      }
      setLoading(true);
      setError(null);
      try {
        const entries = await projectFiles(root, FETCH_LIMIT);
        if (signal?.aborted) return;
        // Stable ordering: dirs first (alpha), then files (alpha).
        const sorted = entries.slice().sort((a, b) => {
          if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
          return a.name.localeCompare(b.name);
        });
        setFiles(sorted);
      } catch (err) {
        if (signal?.aborted) return;
        setError(humanizeError(err));
        setFiles([]);
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [root],
  );

  // Reload whenever the root changes. Use an AbortController so a fast
  // sequence of project switches doesn't race.
  useEffect(() => {
    const ctrl = new AbortController();
    void fetchFiles(ctrl.signal);
    return () => ctrl.abort();
  }, [fetchFiles]);

  // Observe scroller height so virtualization adapts when the sidebar
  // resizes (panel splitter, window resize, etc.).
  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    const update = () => setViewportH(el.clientHeight || 360);
    update();
    let ro: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined") {
      ro = new ResizeObserver(update);
      ro.observe(el);
    } else {
      window.addEventListener("resize", update);
    }
    return () => {
      if (ro) ro.disconnect();
      else window.removeEventListener("resize", update);
    };
  }, []);

  // Reset scroll when the filter changes so the user always sees matches
  // from the top — surprising to keep them scrolled into the middle of an
  // older result set.
  useEffect(() => {
    if (scrollerRef.current) scrollerRef.current.scrollTop = 0;
    setScrollTop(0);
  }, [query]);

  const visible = useMemo(
    () => files.filter((f) => fuzzyMatch(f.name, query)),
    [files, query],
  );

  // Virtualization window.
  const total = visible.length;
  const startIndex = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const visibleCount = Math.ceil(viewportH / ROW_HEIGHT) + OVERSCAN * 2;
  const endIndex = Math.min(total, startIndex + visibleCount);
  const offsetY = startIndex * ROW_HEIGHT;
  const slice = visible.slice(startIndex, endIndex);

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    setScrollTop(e.currentTarget.scrollTop);
  };

  const onPick = (f: FileTreeEntry) => {
    // Folders just expand-toggle in a future iteration; for now treat like
    // files for the @-insert flow — the path is still useful context.
    emitComposerInsert(`@${f.name}`);
    // Also open the file in the CodeMirror EditorPane. Drive the open
    // through the store (mirrors `openInEditor` in lib/editor.ts) so the
    // first open isn't lost when EditorPane hasn't mounted yet — the pane
    // reads `editorPath` from the store on mount, so it doesn't depend on
    // a live window-event listener. We still dispatch the event for any
    // non-React subscribers.
    if (!f.is_dir) {
      try {
        const store = useCortexStore.getState();
        store.setActivityTab("editor");
        store.openEditorPath(f.path);
        setTimeout(() => {
          window.dispatchEvent(
            new CustomEvent("cortex:editor-open", { detail: { path: f.path } }),
          );
        }, 0);
      } catch {
        /* dispatch failures are non-fatal */
      }
    }
  };

  // ── Render ─────────────────────────────────────────────────────────────
  if (!root) {
    return (
      <div className="file-explorer-empty">
        <div className="file-explorer-empty-icon">
          <Folder size={26} strokeWidth={1.5} aria-hidden="true" />
        </div>
        <div className="file-explorer-empty-title">No active project</div>
        <div className="file-explorer-empty-sub">
          Pick a project above to browse its files.
        </div>
      </div>
    );
  }

  return (
    <div className="file-explorer">
      <div className="file-explorer-toolbar">
        <input
          type="text"
          className="file-explorer-search"
          placeholder={`Filter ${projectName ?? "files"}…`}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="Filter files"
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
        />
        <button
          type="button"
          className="btn-ghost file-explorer-refresh"
          title="Refresh file list"
          aria-label="Refresh file list"
          onClick={() => void fetchFiles()}
          disabled={loading}
        >
          ↻
        </button>
      </div>

      <div className="file-explorer-meta">
        {loading && <span className="muted">scanning…</span>}
        {!loading && error && (
          <span className="file-explorer-error" title={error}>
            error: {error.slice(0, 60)}
          </span>
        )}
        {!loading && !error && (
          <span className="muted">
            {visible.length}
            {visible.length !== files.length ? ` / ${files.length}` : ""}
            {files.length >= FETCH_LIMIT ? "+" : ""} entries
          </span>
        )}
      </div>

      <div
        ref={scrollerRef}
        className="file-explorer-scroller"
        onScroll={onScroll}
        onContextMenu={(e) =>
          openMenu(e, { path: root, isDir: true, name: projectName ?? root })
        }
      >
        {visible.length === 0 && !loading && (
          <div className="file-explorer-no-matches">
            {files.length === 0 ? "(no files)" : "(no matches)"}
          </div>
        )}
        {visible.length > 0 && (
          <div
            className="file-explorer-spacer"
            style={{ height: total * ROW_HEIGHT }}
          >
            <div
              className="file-explorer-window"
              style={{ transform: `translateY(${offsetY}px)` }}
            >
              {slice.map((f) => {
                const title = f.is_dir
                  ? f.path
                  : `${f.path}${
                      f.size_bytes != null
                        ? ` · ${formatSize(f.size_bytes)}`
                        : ""
                    }`;
                return (
                  <button
                    key={f.path}
                    type="button"
                    className={`file-explorer-row ${f.is_dir ? "dir" : "file"}`}
                    style={{ height: ROW_HEIGHT }}
                    onClick={() => onPick(f)}
                    onContextMenu={(e) =>
                      openMenu(e, {
                        path: f.path,
                        isDir: f.is_dir,
                        name: f.name,
                      })
                    }
                    title={title}
                  >
                    <span className="file-explorer-glyph" aria-hidden>
                      {f.is_dir ? (
                        <DirIcon open={false} />
                      ) : (
                        <FileIcon name={f.name} />
                      )}
                    </span>
                    <span className="file-explorer-name">{f.name}</span>
                    {!f.is_dir && f.size_bytes != null && (
                      <span className="file-explorer-size">
                        {formatSize(f.size_bytes)}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          </div>
        )}
      </div>
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menu.items}
          label={menu.label}
          onClose={closeMenu}
        />
      )}
    </div>
  );
}
