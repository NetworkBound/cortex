import { useCallback, useEffect, useState } from "react";
import { confirmDialog, promptDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { Camera } from "lucide-react";
import {
  createSnapshot,
  deleteSnapshot,
  formatBytes,
  listSnapshots,
  pruneSnapshots,
  rollbackSnapshot,
  timeAgo,
  type RollbackReport,
  type SnapshotMeta,
} from "@/lib/snapshots";
import { useCortexStore } from "@/state/store";

interface SnapshotsPanelProps {
  /** Called when the panel is dismissed (modal close, Esc, backdrop click). */
  onClose: () => void;
}

/**
 * Memory snapshots panel — rendered as a modal over MemoryExplorer. Lets the
 * user capture a point-in-time tarball of every memory source, browse what's
 * stored, roll back to a prior state, and prune old snapshots.
 */
export function SnapshotsPanel({ onClose }: SnapshotsPanelProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [items, setItems] = useState<SnapshotMeta[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastReport, setLastReport] = useState<RollbackReport | null>(null);
  const [label, setLabel] = useState("manual");

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const all = await listSnapshots();
      setItems(all);
      setError(null);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Esc closes the modal.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  async function snapshotNow() {
    const cleanLabel = label.trim() || "manual";
    setLoading(true);
    try {
      await createSnapshot(cleanLabel, activeProject?.root ?? null);
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }

  async function onRollback(snap: SnapshotMeta) {
    const msg =
      `This restores ${snap.file_count} files across ${snap.roots.length} memory sources.` +
      ` Files newer than the snapshot will be preserved.`;
    if (
      !(await confirmDialog({
        title: `Roll back to "${snap.label}"?`,
        message: `Snapshot from ${timeAgo(snap.created_unix_ms)}.\n${msg}`,
        confirmLabel: "Roll back",
        danger: true,
      }))
    )
      return;
    setBusyId(snap.id);
    setLastReport(null);
    try {
      const report = await rollbackSnapshot(snap.id);
      setLastReport(report);
      setError(null);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyId(null);
    }
  }

  async function onDelete(snap: SnapshotMeta) {
    if (
      !(await confirmDialog({
        title: "Delete snapshot?",
        message: `"${snap.label}" (${timeAgo(snap.created_unix_ms)}) will be deleted.`,
        confirmLabel: "Delete",
        danger: true,
      }))
    )
      return;
    setBusyId(snap.id);
    try {
      await deleteSnapshot(snap.id);
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyId(null);
    }
  }

  async function onPrune() {
    const raw = await promptDialog({
      title: "Prune snapshots",
      message: "Keep newest N snapshots — delete the rest.",
      initialValue: "10",
      confirmLabel: "Prune",
    });
    if (raw === null) return;
    const keep = Math.max(0, Math.floor(Number(raw)));
    if (!Number.isFinite(keep)) {
      setError(`invalid number: ${raw}`);
      return;
    }
    setLoading(true);
    try {
      const removed = await pruneSnapshots(keep);
      await refresh();
      if (removed > 0) setError(null);
      pushToast({
        title: "Snapshots pruned",
        body: `Removed ${removed} snapshot${removed === 1 ? "" : "s"}.`,
        kind: "success",
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div
      className="snapshots-modal"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="snapshots-card">
        <div className="snapshots-head">
          <strong>Memory snapshots</strong>
          <span className="muted">{items.length} stored</span>
          <button
            className="memex-detail-close"
            onClick={onClose}
            title="Close (Esc)"
            style={{ marginLeft: "auto" }}
          >
            ×
          </button>
        </div>
        <div className="snapshots-toolbar">
          <input
            className="snapshots-label-input"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="label (e.g. before-refactor)"
            spellCheck={false}
          />
          <button className="link-btn" onClick={() => void snapshotNow()} disabled={loading}>
            {loading ? "Working…" : <><Camera size={14} strokeWidth={1.75} aria-hidden="true" /> New snapshot</>}
          </button>
          <button className="link-btn" onClick={() => void refresh()} disabled={loading}>
            Refresh
          </button>
          <button className="link-btn" onClick={() => void onPrune()} disabled={loading}>
            Prune older than N
          </button>
        </div>
        {error && <div className="snapshots-error">{error}</div>}
        {lastReport && (
          <div className="snapshots-report">
            Restored {lastReport.files_restored} file{lastReport.files_restored === 1 ? "" : "s"};{" "}
            skipped {lastReport.files_skipped}
            {lastReport.errors.length > 0 && `; ${lastReport.errors.length} error(s)`}.
            {lastReport.errors.length > 0 && (
              <details>
                <summary>errors</summary>
                <pre>{lastReport.errors.join("\n")}</pre>
              </details>
            )}
          </div>
        )}
        <div className="snapshots-list">
          {items.length === 0 && !loading && !error && (
            <div className="muted" style={{ padding: 16, textAlign: "center" }}>
              No snapshots yet. Click <em>New snapshot</em> to capture every memory source.
            </div>
          )}
          {items.map((snap) => (
            <div key={snap.id} className="brain-row snapshots-row">
              <div className="brain-row-head">
                <strong>{snap.label}</strong>
                <span className="muted">{timeAgo(snap.created_unix_ms)}</span>
              </div>
              <div className="brain-meta">
                {snap.file_count} files · {formatBytes(snap.size_bytes)} · {snap.roots.length} sources ·{" "}
                <code style={{ fontFamily: "var(--font-mono, monospace)", fontSize: 10.5 }}>
                  {snap.id.slice(0, 18)}
                </code>
              </div>
              <div style={{ display: "flex", gap: 6, marginTop: 6 }}>
                <button
                  className="link-btn"
                  disabled={busyId === snap.id}
                  onClick={() => void onRollback(snap)}
                >
                  {busyId === snap.id ? "restoring…" : "restore"}
                </button>
                <button
                  className="link-btn danger"
                  disabled={busyId === snap.id}
                  onClick={() => void onDelete(snap)}
                >
                  Delete
                </button>
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
