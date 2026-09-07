import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { SkeletonText } from "./Skeleton";
import {
  formatBytes,
  formatDate,
  groupByKind,
  kindLabel,
  memoryStats,
  syncMemory,
  type MemoryStats,
  type SyncReport,
} from "@/lib/memory-stats";
import { pushToast } from "@/lib/toast";

/**
 * Memory-bridge stats panel. Portal modal mirroring IDEExportModal so the
 * `/memstats` slash command can summon it without App.tsx wiring. Shows:
 *
 *   - Top totals (file count + total size) and a CSS bar chart of files per
 *     source kind.
 *   - Per-source table with label, root path, file count, total size, and
 *     oldest/newest file age range.
 *   - Chroma DB row (claude-mem's semantic index) — surface size + presence.
 *   - "Sync now" button that triggers the `import_claude_mem` backend via
 *     `sync_memory` and renders the report inline.
 */

interface MemoryStatsPanelProps {
  onClose: () => void;
}

export function MemoryStatsPanel({ onClose }: MemoryStatsPanelProps) {
  const [stats, setStats] = useState<MemoryStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [report, setReport] = useState<SyncReport | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await memoryStats();
      setStats(data);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // ESC closes the modal — matches IDEExportModal's transient-surface
  // convention.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onSync = useCallback(async () => {
    setSyncing(true);
    setReport(null);
    try {
      const result = await syncMemory();
      setReport(result);
      pushToast({
        title: "Memory sync complete",
        body: `${result.imported} imported, ${result.skipped} skipped${
          result.errors.length ? `, ${result.errors.length} error(s)` : ""
        }.`,
        kind: result.errors.length === 0 ? "success" : "warning",
      });
      // Refresh totals after a successful sync so the user sees the new files
      // reflected in the per-source table without manually re-opening.
      await refresh();
    } catch (e) {
      const msg = humanizeError(e);
      setReport({ imported: 0, skipped: 0, errors: [msg] });
      pushToast({ title: "Sync failed", body: msg, kind: "error" });
    } finally {
      setSyncing(false);
    }
  }, [refresh]);

  const kindRows = useMemo(() => (stats ? groupByKind(stats.sources) : []), [stats]);
  const maxKindCount = useMemo(
    () => kindRows.reduce((acc, r) => Math.max(acc, r.count), 0),
    [kindRows],
  );

  return (
    <div className="memstats-backdrop" onMouseDown={onClose}>
      <div
        className="memstats-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="memstats-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="memstats-header">
          <h2 id="memstats-title">Memory Bridge Stats</h2>
          <button className="memstats-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>
        <p className="memstats-summary">
          Walks every configured memory source (Claude project memory, runbooks,
          global / project instruction files, Obsidian) and reports counts +
          sizes. Use <strong>Sync now</strong> to mirror new Claude-memory
          markdown into Cortex's imported store.
        </p>

        {loading && <SkeletonText lines={5} className="memstats-loading" />}
        {error && <div className="memstats-error">{error}</div>}

        {stats && !loading && (
          <>
            <section className="memstats-totals">
              <div className="memstats-total-card">
                <div className="memstats-total-num">
                  {stats.total_file_count.toLocaleString()}
                </div>
                <div className="memstats-total-lbl">files</div>
              </div>
              <div className="memstats-total-card">
                <div className="memstats-total-num">{formatBytes(stats.total_bytes)}</div>
                <div className="memstats-total-lbl">total size</div>
              </div>
              <div className="memstats-total-card">
                <div className="memstats-total-num">
                  {stats.chroma.exists ? formatBytes(stats.chroma.bytes) : "absent"}
                </div>
                <div className="memstats-total-lbl">chroma db</div>
              </div>
            </section>

            {kindRows.length > 0 && (
              <section className="memstats-chart">
                <h3>Files by source kind</h3>
                <ul className="memstats-bars">
                  {kindRows.map((row) => {
                    const pct = maxKindCount > 0 ? (row.count / maxKindCount) * 100 : 0;
                    return (
                      <li key={row.kind} className="memstats-bar-row">
                        <span className="memstats-bar-label">{kindLabel(row.kind)}</span>
                        <span className="memstats-bar-track">
                          <span
                            className="memstats-bar-fill"
                            style={{ width: `${pct}%` }}
                            aria-hidden="true"
                          />
                        </span>
                        <span className="memstats-bar-count">
                          {row.count.toLocaleString()} · {formatBytes(row.bytes)}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              </section>
            )}

            <section className="memstats-table-wrap">
              <h3>Sources ({stats.sources.length})</h3>
              {stats.sources.length === 0 ? (
                <p className="memstats-empty">No memory sources found on disk.</p>
              ) : (
                <table className="memstats-table">
                  <thead>
                    <tr>
                      <th>Label</th>
                      <th>Path</th>
                      <th className="memstats-num">Files</th>
                      <th className="memstats-num">Size</th>
                      <th>Age range</th>
                    </tr>
                  </thead>
                  <tbody>
                    {stats.sources.map((s) => (
                      <tr key={`${s.label}-${s.root_path}`}>
                        <td>
                          <span className="memstats-kind">{kindLabel(s.kind)}</span>
                          <span className="memstats-label">{s.label}</span>
                        </td>
                        <td>
                          <code className="memstats-path">{s.root_path}</code>
                        </td>
                        <td className="memstats-num">{s.file_count.toLocaleString()}</td>
                        <td className="memstats-num">{formatBytes(s.total_bytes)}</td>
                        <td>
                          {formatDate(s.oldest_unix_ms)} → {formatDate(s.newest_unix_ms)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </section>

            {report && (
              <section className="memstats-report">
                <strong>Sync report:</strong> {report.imported} imported,{" "}
                {report.skipped} skipped
                {report.errors.length > 0 && (
                  <ul>
                    {report.errors.map((e, i) => (
                      <li key={i}>
                        <code>{e}</code>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            )}
          </>
        )}

        <footer className="memstats-footer">
          <button className="memstats-secondary" onClick={onClose} disabled={syncing}>
            Close
          </button>
          <button
            className="memstats-secondary"
            onClick={() => void refresh()}
            disabled={loading || syncing}
          >
            Refresh
          </button>
          <button
            className="memstats-primary"
            onClick={() => void onSync()}
            disabled={syncing || loading}
          >
            {syncing ? "Syncing…" : "Sync now"}
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/memstats` slash command. Same self-mounting
 * portal pattern as IDEExportModal — keeps App.tsx untouched.
 */
let activeRoot: Root | null = null;

export function openMemoryStatsPanel(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "memstats";
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
  root.render(<MemoryStatsPanel onClose={close} />);
}
