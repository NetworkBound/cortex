import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { recentAudit, type AuditRow } from "@/lib/observability";
import { timeAgo } from "@/lib/time";
import { pushToast } from "@/lib/toast";

/**
 * Read-only viewer for the agent audit log (`audit_log` table backed by the
 * tracing store). Renders as a self-mounting modal so the `/audit` slash
 * command can summon it without App.tsx wiring — same portal pattern as
 * IDEExportModal / KeyVaultPanel.
 *
 * Pagination is "load N at a time" — the SQL command has a hard `LIMIT`, so
 * we re-query with a growing `limit` whenever the user scrolls near the
 * bottom or clicks "Load more". The list is then post-filtered client-side
 * by action radio + detail substring, so filter changes are instant and
 * never re-hit the backend.
 */

interface AuditLogPanelProps {
  onClose: () => void;
}

const PAGE_SIZE = 100;
const HARD_LIMIT = 1000; // matches the backend clamp

/** Visual badge colour per action category. Anything we don't recognise gets
 * the neutral "default" pill — no need to chase every action name. */
function badgeKind(action: string): "edit" | "exec" | "approve" | "agent" | "default" {
  const a = action.toLowerCase();
  if (a.includes("edit") || a.includes("write") || a.includes("patch")) return "edit";
  if (a.includes("exec") || a.includes("run") || a.includes("shell")) return "exec";
  if (a.includes("approve") || a.includes("deny") || a.includes("approval")) return "approve";
  if (a.includes("agent") || a.includes("spawn")) return "agent";
  return "default";
}

function truncate(s: string | null, max = 120): string {
  if (!s) return "";
  return s.length <= max ? s : `${s.slice(0, max - 1)}…`;
}

/** RFC 4180-ish: wrap in quotes, double internal quotes. */
function csvCell(s: string): string {
  if (/[",\n\r]/.test(s)) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

function toCsv(rows: AuditRow[]): string {
  const header = "ts_iso,session_id,agent_id,action,detail";
  const lines = rows.map((r) => {
    const ts = new Date(r.ts).toISOString();
    return [
      csvCell(ts),
      csvCell(r.session_id ?? ""),
      csvCell(r.agent_id ?? ""),
      csvCell(r.action),
      csvCell(r.detail ?? ""),
    ].join(",");
  });
  return [header, ...lines].join("\n");
}

export function AuditLogPanel({ onClose }: AuditLogPanelProps) {
  const [rows, setRows] = useState<AuditRow[]>([]);
  const [limit, setLimit] = useState<number>(PAGE_SIZE);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionFilter, setActionFilter] = useState<string>("all");
  const [search, setSearch] = useState<string>("");
  const listRef = useRef<HTMLDivElement | null>(null);

  const load = useCallback(async (nextLimit: number) => {
    setLoading(true);
    setError(null);
    try {
      const data = await recentAudit(nextLimit);
      setRows(data);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(limit);
  }, [load, limit]);

  // ESC closes the modal.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  /** Unique set of actions in the current result set, sorted alphabetically.
   * Powers the radio filter so the user only sees real options. */
  const actionOptions = useMemo(() => {
    const set = new Set<string>();
    for (const r of rows) set.add(r.action);
    return Array.from(set).sort();
  }, [rows]);

  /** Apply action + search filters client-side so the UI feels instant. */
  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    return rows.filter((r) => {
      if (actionFilter !== "all" && r.action !== actionFilter) return false;
      if (!q) return true;
      const hay = `${r.action} ${r.detail ?? ""} ${r.agent_id ?? ""} ${r.session_id ?? ""}`.toLowerCase();
      return hay.includes(q);
    });
  }, [rows, actionFilter, search]);

  /** Infinite-scroll: when the user scrolls to within 80px of the bottom and
   * we haven't yet hit the backend's hard cap, request another page. */
  const onScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      if (loading) return;
      if (limit >= HARD_LIMIT) return;
      const el = e.currentTarget;
      const remaining = el.scrollHeight - (el.scrollTop + el.clientHeight);
      if (remaining < 80 && rows.length >= limit) {
        setLimit((prev) => Math.min(prev + PAGE_SIZE, HARD_LIMIT));
      }
    },
    [loading, limit, rows.length],
  );

  const onExport = useCallback(async () => {
    if (visible.length === 0) {
      pushToast({ title: "Nothing to export", body: "No rows match the current filter.", kind: "info" });
      return;
    }
    try {
      await navigator.clipboard.writeText(toCsv(visible));
      pushToast({
        title: "Audit log copied",
        body: `${visible.length} row${visible.length === 1 ? "" : "s"} on clipboard as CSV.`,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [visible]);

  const onLoadMore = useCallback(() => {
    if (limit >= HARD_LIMIT) return;
    setLimit((prev) => Math.min(prev + PAGE_SIZE, HARD_LIMIT));
  }, [limit]);

  return (
    <div className="audit-backdrop" onMouseDown={onClose}>
      <div
        className="audit-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="audit-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="audit-header">
          <h2 id="audit-title">Audit Log</h2>
          <button className="audit-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="audit-summary">
          Append-only record of agent tool calls, file edits, and shell execs. Showing the most
          recent {rows.length} of {HARD_LIMIT} max.
        </p>

        <section className="audit-controls">
          <input
            className="audit-search"
            type="text"
            value={search}
            placeholder="Search detail / agent / session…"
            onChange={(e) => setSearch(e.target.value)}
          />
          <button className="audit-export" onClick={onExport} disabled={visible.length === 0}>
            Export CSV
          </button>
        </section>

        {(!error || rows.length > 0) && (
        <section className="audit-filter">
          <label className="audit-filter-row">
            <input
              type="radio"
              name="audit-action"
              value="all"
              checked={actionFilter === "all"}
              onChange={() => setActionFilter("all")}
            />
            <span>all ({rows.length})</span>
          </label>
          {actionOptions.map((a) => {
            const count = rows.filter((r) => r.action === a).length;
            return (
              <label key={a} className="audit-filter-row">
                <input
                  type="radio"
                  name="audit-action"
                  value={a}
                  checked={actionFilter === a}
                  onChange={() => setActionFilter(a)}
                />
                <span className={`audit-badge audit-badge-${badgeKind(a)}`}>{a}</span>
                <span className="audit-count">{count}</span>
              </label>
            );
          })}
        </section>
        )}

        {error && <div className="audit-error">{error}</div>}

        <div className="audit-table-wrap" ref={listRef} onScroll={onScroll}>
          <table className="audit-table">
            <thead>
              <tr>
                <th>Time</th>
                <th>Action</th>
                <th>Detail</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((r, i) => (
                <tr key={`${r.ts}-${i}`}>
                  <td className="audit-ts" title={new Date(r.ts).toISOString()}>
                    {timeAgo(r.ts, { absoluteAfterDays: 30 })}
                  </td>
                  <td>
                    <span className={`audit-badge audit-badge-${badgeKind(r.action)}`}>
                      {r.action}
                    </span>
                  </td>
                  <td className="audit-detail" title={r.detail ?? ""}>
                    {truncate(r.detail)}
                  </td>
                </tr>
              ))}
              {!loading && !error && visible.length === 0 && (
                <tr>
                  <td colSpan={3} className="audit-empty">
                    {rows.length === 0
                      ? "No audit entries yet."
                      : "No rows match the current filter."}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>

        <footer className="audit-footer">
          <span className="audit-status">
            {loading ? "Loading…" : `${visible.length} shown · ${rows.length} loaded`}
          </span>
          <button
            className="audit-loadmore"
            onClick={onLoadMore}
            disabled={loading || limit >= HARD_LIMIT || rows.length < limit}
          >
            {limit >= HARD_LIMIT ? "Max loaded" : "Load more"}
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner — mounts a detached React root on document.body and
 * tears it down on close. Mirrors `openIDEExportModal` so we don't need to
 * touch App.tsx.
 */
let activeRoot: Root | null = null;

export function openAuditLogPanel(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "audit-log";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<AuditLogPanel onClose={close} />);
}
