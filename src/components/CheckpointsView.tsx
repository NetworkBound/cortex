import { useCallback, useEffect, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import {
  createCheckpoint,
  deleteCheckpoint,
  diffCheckpoint,
  formatBytes,
  listCheckpoints,
  pruneCheckpoints,
  restoreCheckpoint,
  timeAgo,
  type CheckpointDiff,
  type CheckpointInfo,
} from "@/lib/checkpoints";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import { CheckpointDiffModal } from "./CheckpointDiffModal";

export function CheckpointsView() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [items, setItems] = useState<CheckpointInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Pre-restore diff state: the checkpoint under review, its computed diff,
  // and whether the confirmed restore is currently writing.
  const [diffTarget, setDiffTarget] = useState<CheckpointInfo | null>(null);
  const [diff, setDiff] = useState<CheckpointDiff | null>(null);
  const [restoring, setRestoring] = useState(false);

  const refresh = useCallback(async () => {
    if (!activeProject) {
      setItems([]);
      return;
    }
    setLoading(true);
    try {
      const all = await listCheckpoints(activeProject.root);
      setItems(all);
      setError(null);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, [activeProject]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function snapshotNow() {
    if (!activeProject) return;
    setLoading(true);
    try {
      await createCheckpoint(activeProject.root, "manual");
      await pruneCheckpoints(activeProject.root);
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }

  // Step 1 of restore: compute the read-only diff and open the review modal.
  // No files are touched until the user confirms inside the modal.
  async function onReviewRestore(ck: CheckpointInfo) {
    if (!activeProject) return;
    setBusyId(ck.id);
    try {
      const d = await diffCheckpoint(activeProject.root, ck.id);
      setDiffTarget(ck);
      setDiff(d);
      setError(null);
    } catch (e) {
      setError(humanizeError(e));
      pushToast({
        title: "Couldn't compare checkpoint",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setBusyId(null);
    }
  }

  function closeDiff() {
    if (restoring) return;
    setDiffTarget(null);
    setDiff(null);
  }

  // Step 2: the user confirmed from the diff. Run the actual restore. The
  // backend gates on a dirty tree unless `force`; since the user has now seen
  // exactly what changes, we pass force=true to honour their explicit choice.
  async function onConfirmRestore() {
    if (!activeProject || !diffTarget) return;
    setRestoring(true);
    try {
      await restoreCheckpoint(activeProject.root, diffTarget.id, true);
      pushToast({
        title: "Checkpoint restored",
        body: `Restored ${diffTarget.label ?? "auto"} from ${timeAgo(diffTarget.ts)}.`,
        kind: "success",
      });
      setError(null);
      setDiffTarget(null);
      setDiff(null);
    } catch (e) {
      const msg = humanizeError(e);
      setError(msg);
      pushToast({ title: "Restore failed", body: msg, kind: "error" });
    } finally {
      setRestoring(false);
    }
  }

  async function onDelete(ck: CheckpointInfo) {
    if (!activeProject) return;
    if (
      !(await confirmDialog({
        title: "Delete checkpoint?",
        message: `Checkpoint ${ck.id.slice(0, 12)} will be deleted.`,
        confirmLabel: "Delete",
        danger: true,
      }))
    )
      return;
    setBusyId(ck.id);
    try {
      await deleteCheckpoint(activeProject.root, ck.id);
      pushToast({ title: "Checkpoint deleted", kind: "success" });
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
      pushToast({ title: "Delete failed", body: humanizeError(e), kind: "error" });
    } finally {
      setBusyId(null);
    }
  }

  if (!activeProject) {
    return (
      <div className="muted" style={{ padding: "var(--space-4)", textAlign: "center" }}>
        Pick a project to see checkpoints.
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div
        style={{
          padding: "var(--space-2)",
          borderBottom: "1px solid var(--border)",
          display: "flex",
          gap: "var(--space-2)",
          alignItems: "center",
        }}
      >
        <button className="link-btn" onClick={() => void snapshotNow()} disabled={loading}>
          {loading ? "working…" : "snapshot now"}
        </button>
        <button className="link-btn" onClick={() => void refresh()} disabled={loading}>
          Refresh
        </button>
        <span className="muted" style={{ marginLeft: "auto", fontSize: "var(--text-xs)" }}>
          {items.length} stored
        </span>
      </div>
      {error && (
        <div className="muted" style={{ padding: "var(--space-2)", color: "var(--danger)", fontSize: "var(--text-xs)" }}>
          {error}
        </div>
      )}
      <div style={{ flex: 1, overflow: "auto" }}>
        {items.length === 0 && loading && <PanelLoading label="Loading checkpoints" />}
        {items.length === 0 && !loading && !error && (
          <div className="muted" style={{ padding: "var(--space-4)", textAlign: "center" }}>
            No checkpoints yet. They're created automatically after each tool-edit turn.
          </div>
        )}
        <div className="brain-list" style={{ padding: "var(--space-2)" }}>
          {items.map((ck) => (
            <div key={ck.id} className="brain-row">
              <div className="brain-row-head">
                <strong>{ck.label ?? "auto"}</strong>
                <span className="muted">{timeAgo(ck.ts)}</span>
              </div>
              <div className="brain-meta">
                {ck.file_count} files · {formatBytes(ck.size_bytes)} ·{" "}
                {/* Mono hash microlabel — floored at --text-xs per DESIGN-SPEC §3. */}
                <code style={{ fontFamily: "var(--mono, monospace)", fontSize: "var(--text-xs)" }}>
                  {ck.id.slice(0, 12)}
                </code>
              </div>
              <div style={{ display: "flex", gap: 6, marginTop: 6 }}>
                <button
                  className="link-btn"
                  disabled={busyId === ck.id}
                  onClick={() => void onReviewRestore(ck)}
                >
                  {busyId === ck.id ? "comparing…" : "Restore"}
                </button>
                <button
                  className="link-btn danger"
                  disabled={busyId === ck.id}
                  onClick={() => void onDelete(ck)}
                >
                  Delete
                </button>
              </div>
            </div>
          ))}
        </div>
      </div>
      {diffTarget && diff && (
        <CheckpointDiffModal
          checkpoint={diffTarget}
          diff={diff}
          restoring={restoring}
          onConfirm={() => void onConfirmRestore()}
          onCancel={closeDiff}
        />
      )}
    </div>
  );
}
