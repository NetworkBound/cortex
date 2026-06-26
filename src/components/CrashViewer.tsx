import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  KIND_FILTERS,
  dispatchReplay,
  severityOf,
  timeAgo,
  toDetails,
  truncate,
  type CrashKindFilter,
} from "@/lib/crash-viewer";
import { recentCrashes, type CrashRow } from "@/lib/observability";
import { Chevron } from "@/lib/chevron";
import { pushToast } from "@/lib/toast";

/**
 * Sentry-style crash viewer for both Rust panics and JS errors. Renders as a
 * self-mounting portal modal so `/crashes` can summon it without App.tsx
 * wiring (same pattern as IDEExportModal / AuditLogPanel).
 *
 * The backend (`recent_crashes`) returns a flat list newest-first. Filtering
 * is purely client-side — instant feedback, no re-fetches on every keystroke.
 */

interface CrashViewerProps {
  onClose: () => void;
}

const PAGE_LIMIT = 200;

export function CrashViewer({ onClose }: CrashViewerProps) {
  const [rows, setRows] = useState<CrashRow[]>([]);
  const [kindFilter, setKindFilter] = useState<CrashKindFilter>("all");
  const [search, setSearch] = useState("");
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [stackOpen, setStackOpen] = useState<Set<number>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await recentCrashes(PAGE_LIMIT);
      setRows(data);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // ESC closes the modal.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  /** Per-kind counts used in the filter button labels — built once per fetch. */
  const counts = useMemo(() => {
    const c: Record<string, number> = { all: rows.length };
    for (const r of rows) c[r.kind] = (c[r.kind] ?? 0) + 1;
    return c;
  }, [rows]);

  /** Visible after kind+search filtering. Newest-first ordering is preserved
   * from the backend so we never re-sort here. */
  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    return rows.filter((r) => {
      if (kindFilter !== "all" && r.kind !== kindFilter) return false;
      if (!q) return true;
      return r.message.toLowerCase().includes(q);
    });
  }, [rows, kindFilter, search]);

  const toggleExpand = useCallback((id: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const toggleStack = useCallback((id: number) => {
    setStackOpen((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const onCopyJson = useCallback(async (row: CrashRow) => {
    try {
      const json = JSON.stringify(toDetails(row), null, 2);
      await navigator.clipboard.writeText(json);
      pushToast({ title: "Copied", body: "Crash details on clipboard.", kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, []);

  const onReplay = useCallback((row: CrashRow) => {
    const d = toDetails(row);
    if (!d.last_user_message) return; // button guard — shouldn't render
    dispatchReplay(d.last_user_message);
    pushToast({
      title: "Replayed",
      body: "Sent the last user message back to the chat.",
      kind: "success",
    });
  }, []);

  return (
    <div className="crash-backdrop" onMouseDown={onClose}>
      <div
        className="crash-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="crash-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="crash-header">
          <h2 id="crash-title">Crash Reports</h2>
          <button className="crash-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="crash-summary">
          Local-only crash log — Rust panics and JS errors captured on this device. Showing the most
          recent {rows.length} of {PAGE_LIMIT} max.
        </p>

        {(!error || rows.length > 0) && (
        <section className="crash-filters" role="tablist" aria-label="Crash kind filter">
          {KIND_FILTERS.map((f) => {
            const n = counts[f.id] ?? 0;
            const active = kindFilter === f.id;
            return (
              <button
                key={f.id}
                type="button"
                role="tab"
                aria-selected={active}
                className={`crash-filter${active ? " crash-filter-active" : ""}`}
                onClick={() => setKindFilter(f.id)}
                disabled={f.id !== "all" && n === 0}
              >
                {f.label} <span className="crash-filter-count">{n}</span>
              </button>
            );
          })}
        </section>
        )}

        <section className="crash-controls">
          <input
            className="crash-search"
            type="text"
            value={search}
            placeholder="Filter by message substring…"
            onChange={(e) => setSearch(e.target.value)}
          />
          <button
            className="crash-refresh"
            onClick={() => void load()}
            disabled={loading}
            aria-label="Reload crash log"
          >
            {loading ? "Loading…" : "Reload"}
          </button>
        </section>

        {error && <div className="crash-error">{error}</div>}

        <div className="crash-list">
          {visible.length === 0 && !loading && !error && (
            <div className="crash-empty">
              {rows.length === 0
                ? "No crashes captured. 🎉"
                : "No crashes match the current filter."}
            </div>
          )}

          {visible.map((row) => {
            const d = toDetails(row);
            const isOpen = expanded.has(row.id);
            const isStackOpen = stackOpen.has(row.id);
            const sev = severityOf(row.kind);
            return (
              <article key={row.id} className={`crash-row${isOpen ? " crash-row-open" : ""}`}>
                <button
                  type="button"
                  className="crash-row-head"
                  onClick={() => toggleExpand(row.id)}
                  aria-expanded={isOpen}
                >
                  <span className={`crash-sev crash-sev-${sev}`}>{sev}</span>
                  <span className="crash-kind">{row.kind}</span>
                  <span className="crash-time" title={d.ts_iso}>
                    {timeAgo(row.ts)}
                  </span>
                  <span className="crash-msg" title={row.message}>
                    {isOpen ? row.message : truncate(row.message, 120)}
                  </span>
                </button>

                {isOpen && (
                  <div className="crash-row-body">
                    <dl className="crash-meta">
                      {d.file_line && (
                        <>
                          <dt>Location</dt>
                          <dd>
                            <code>{d.file_line}</code>
                          </dd>
                        </>
                      )}
                      <dt>Version</dt>
                      <dd>
                        <code>{d.version ?? "—"}</code>
                      </dd>
                      <dt>OS</dt>
                      <dd>{d.os}</dd>
                      <dt>Timestamp</dt>
                      <dd title={String(row.ts)}>{d.ts_iso}</dd>
                    </dl>

                    {row.stack && (
                      <div className="crash-stack-wrap">
                        <button
                          type="button"
                          className="crash-stack-toggle"
                          onClick={() => toggleStack(row.id)}
                          aria-expanded={isStackOpen}
                        >
                          <Chevron open={isStackOpen} size={13} /> Stack trace
                        </button>
                        {isStackOpen && <pre className="crash-stack">{row.stack}</pre>}
                      </div>
                    )}

                    <div className="crash-actions">
                      <button
                        type="button"
                        className="crash-action"
                        onClick={() => void onCopyJson(row)}
                      >
                        Copy as JSON
                      </button>
                      {d.last_user_message && (
                        <button
                          type="button"
                          className="crash-action crash-action-primary"
                          onClick={() => onReplay(row)}
                        >
                          Replay last user message
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </article>
            );
          })}
        </div>

        <footer className="crash-footer">
          <span className="crash-status">
            {loading ? "Loading…" : `${visible.length} shown · ${rows.length} loaded`}
          </span>
        </footer>
      </div>
    </div>
  );
}

/**
 * Mount/unmount a detached React root on document.body. Re-entrant: a second
 * call while a viewer is already open is a no-op rather than stacking modals.
 */
let activeRoot: Root | null = null;

export function mountCrashViewer(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "crash-viewer";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<CrashViewer onClose={close} />);
}
