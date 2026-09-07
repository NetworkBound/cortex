import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { findFiles } from "@/lib/project-search";
import {
  loadRecentFiles,
  pickFile,
  recordRecentFile,
  type RecentFile,
} from "@/lib/quick-open";
import { useCortexStore } from "@/state/store";

/**
 * Ctrl+P-style quick-open modal. Empty query shows the MRU "recent
 * files" list from `~/.cortex/recent-files.json`; a non-empty query
 * routes through the existing `find_files` backend command and renders
 * matching paths under the active project's root.
 *
 * Two modes:
 *   - Open (default): selection records the path as recent and dispatches
 *     a `cortex:editor-open` event via `pickFile`, then closes the modal.
 *   - Pick (`onPick` set): selection is handed to the callback instead of
 *     the editor — used by surfaces that need a file *reference* (e.g. the
 *     multibuffer's "+ Add excerpt"). With `withRange`, an inline line-range
 *     field rides under the search input, and a query that looks like an
 *     absolute path gets a "use this path" fallback row so files outside
 *     the project/recents are still reachable.
 */

/** What pick mode hands back: a path plus an optional 1-based inclusive
 *  line range (`null` = whole file; only set when `withRange` is on). */
export interface QuickOpenPick {
  path: string;
  range: { start: number; end: number } | null;
}

export interface QuickOpenModalProps {
  initialQuery?: string;
  onClose: () => void;
  /** Pick mode: hand the selection to this callback instead of opening it. */
  onPick?: (pick: QuickOpenPick) => void;
  /** Render the inline line-range field (pick mode only). */
  withRange?: boolean;
  /** Accessible label override (defaults to "Quick open file"). */
  title?: string;
}

/** Parse the range field: "" = whole file (null), "10:40" / "10-40" = span,
 *  "12" = single line. Anything else is "invalid". */
function parseRangeText(
  text: string,
): { start: number; end: number } | null | "invalid" {
  const t = text.trim();
  if (!t) return null;
  const span = t.match(/^(\d+)\s*[-:]\s*(\d+)$/);
  if (span) {
    const start = Number.parseInt(span[1], 10);
    const end = Number.parseInt(span[2], 10);
    return start >= 1 && end >= start ? { start, end } : "invalid";
  }
  const single = t.match(/^(\d+)$/);
  if (single) {
    const n = Number.parseInt(single[1], 10);
    return n >= 1 ? { start: n, end: n } : "invalid";
  }
  return "invalid";
}

/** Absolute path on either OS family — the pick-mode escape hatch for files
 *  outside the project root and the recents list. */
function looksAbsolute(q: string): boolean {
  return q.startsWith("/") || /^[A-Za-z]:[\\/]/.test(q);
}

interface Row {
  path: string;
  /** Optional secondary text (e.g. "recent · 2m ago"). */
  hint?: string;
}

const RESULT_LIMIT = 50;

export function QuickOpenModal({
  initialQuery = "",
  onClose,
  onPick,
  withRange = false,
  title,
}: QuickOpenModalProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [query, setQuery] = useState(initialQuery);
  const [idx, setIdx] = useState(0);
  const [recents, setRecents] = useState<RecentFile[]>([]);
  const [results, setResults] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [rangeText, setRangeText] = useState("");
  const [rangeError, setRangeError] = useState(false);
  const rangeRef = useRef<HTMLInputElement | null>(null);
  const findToken = useRef(0);

  // Load recents once on mount. Failures collapse to an empty list — the
  // user still gets a working search-driven mode.
  useEffect(() => {
    let cancelled = false;
    loadRecentFiles()
      .then((rs) => {
        if (!cancelled) setRecents(rs);
      })
      .catch(() => {
        if (!cancelled) setRecents([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Debounced `find_files` lookup. Empty query short-circuits to no
  // backend hit so we don't flood the channel while the user types and
  // deletes a query.
  useEffect(() => {
    const root = activeProject?.root;
    const q = query.trim();
    if (!q || !root) {
      setResults([]);
      setError(null);
      return;
    }
    const tok = ++findToken.current;
    const handle = window.setTimeout(() => {
      findFiles(root, q)
        .then((paths) => {
          if (tok !== findToken.current) return;
          setResults(paths.slice(0, RESULT_LIMIT));
          setError(null);
        })
        .catch((e) => {
          if (tok !== findToken.current) return;
          setResults([]);
          setError(humanizeError(e));
        });
    }, 120);
    return () => window.clearTimeout(handle);
  }, [query, activeProject?.root]);

  // Reset highlight whenever the visible set changes shape.
  useEffect(() => {
    setIdx(0);
  }, [query]);

  const rows: Row[] = useMemo(() => {
    const q = query.trim();
    if (q) {
      const out: Row[] = results.map((path) => ({ path }));
      // Pick mode keeps an absolute-path escape hatch: typing/pasting a full
      // path always yields a selectable row, even with no project active.
      if (onPick && looksAbsolute(q) && !results.includes(q)) {
        out.push({ path: q, hint: "use this path" });
      }
      return out;
    }
    return recents.slice(0, RESULT_LIMIT).map((r) => ({
      path: r.path,
      hint: r.accessed_unix_ms ? `recent · ${timeAgo(r.accessed_unix_ms)}` : "recent",
    }));
  }, [query, results, recents, onPick]);

  const activate = useCallback(
    (row: Row | undefined) => {
      if (!row) return;
      if (onPick) {
        const parsed = withRange ? parseRangeText(rangeText) : null;
        if (parsed === "invalid") {
          setRangeError(true);
          rangeRef.current?.focus();
          return;
        }
        void recordRecentFile(row.path);
        onPick({ path: row.path, range: parsed });
        onClose();
        return;
      }
      pickFile(row.path);
      onClose();
    },
    [onClose, onPick, withRange, rangeText],
  );

  // Keyboard navigation. Escape closes; arrows move the highlight; Enter
  // activates the highlighted row.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setIdx((i) => Math.min(i + 1, Math.max(rows.length - 1, 0)));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setIdx((i) => Math.max(i - 1, 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        activate(rows[idx]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [rows, idx, onClose, activate]);

  const showingRecents = !query.trim();
  const emptyMessage = showingRecents
    ? "No recent files yet — open one to start the list."
    : activeProject?.root
      ? "No matching files."
      : onPick
        ? "No matches — paste an absolute path to use it directly."
        : "No active project — pick one from the sidebar first.";

  return (
    <div className="quick-open-backdrop" onMouseDown={onClose}>
      <div
        className="quick-open-modal"
        role="dialog"
        aria-modal="true"
        aria-label={title ?? "Quick open file"}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <input
          autoFocus
          className="quick-open-search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={
            activeProject?.root
              ? "Search files… (empty = recent files)"
              : onPick
                ? "Search recents, or paste an absolute path"
                : "No active project — recents only"
          }
        />
        {withRange && (
          <div className="quick-open-range">
            <label className="quick-open-range-label" htmlFor="quick-open-range-input">
              Lines
            </label>
            <input
              id="quick-open-range-input"
              ref={rangeRef}
              className={`quick-open-range-input ${rangeError ? "invalid" : ""}`}
              value={rangeText}
              onChange={(e) => {
                setRangeText(e.target.value);
                setRangeError(false);
              }}
              placeholder="10:40"
              spellCheck={false}
            />
            <span className={`quick-open-range-hint ${rangeError ? "error" : "muted"}`}>
              {rangeError ? "Use start:end, e.g. 10:40" : "start:end — empty = whole file"}
            </span>
          </div>
        )}
        {error && <div className="quick-open-error">{error}</div>}
        <ul className="quick-open-results">
          {rows.length === 0 && !error && (
            <li className="quick-open-empty muted">{emptyMessage}</li>
          )}
          {rows.map((row, i) => {
            const base = basename(row.path);
            const dir = dirname(row.path, activeProject?.root ?? null);
            return (
              <li
                key={`${row.path}-${i}`}
                className={`quick-open-result ${i === idx ? "active" : ""}`}
                onMouseEnter={() => setIdx(i)}
                onClick={() => activate(row)}
              >
                <div className="quick-open-result-main">
                  <span className="quick-open-result-name">{base}</span>
                  {dir && <span className="quick-open-result-dir muted">{dir}</span>}
                </div>
                {row.hint && (
                  <span className="quick-open-result-hint muted">{row.hint}</span>
                )}
              </li>
            );
          })}
        </ul>
        <footer className="quick-open-footer muted">
          {showingRecents ? "Recent files" : `${rows.length} match${rows.length === 1 ? "" : "es"}`}
          <span className="quick-open-hotkeys">
            {onPick ? "↑↓ navigate · ↵ pick · esc cancel" : "↑↓ navigate · ↵ open · esc close"}
          </span>
        </footer>
      </div>
    </div>
  );
}

// ---------- path helpers (kept local to avoid pulling node:path) ----------

function basename(path: string): string {
  const sep = path.includes("\\") && !path.includes("/") ? "\\" : "/";
  const i = path.lastIndexOf(sep);
  return i >= 0 ? path.slice(i + 1) : path;
}

function dirname(path: string, projectRoot: string | null): string {
  let rel = path;
  if (projectRoot && path.startsWith(projectRoot)) {
    rel = path.slice(projectRoot.length).replace(/^[\\/]/, "");
  }
  const sep = rel.includes("\\") && !rel.includes("/") ? "\\" : "/";
  const i = rel.lastIndexOf(sep);
  return i >= 0 ? rel.slice(0, i) : "";
}

