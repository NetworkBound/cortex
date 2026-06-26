import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createWorktree, listWorktrees, removeWorktree, type Worktree } from "@/lib/worktrees";
import { useCortexStore } from "@/state/store";

interface Props {
  open: boolean;
  onClose: () => void;
}

export function WorktreePicker({ open, onClose }: Props) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const currentId = useCortexStore((s) => s.currentWorktreeId);
  const setCurrent = useCortexStore((s) => s.setCurrentWorktree);
  const [list, setList] = useState<Worktree[]>([]);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !activeProject) return;
    refresh();
  }, [open, activeProject]);

  async function refresh() {
    if (!activeProject) return;
    try {
      const ws = await listWorktrees(activeProject.root);
      setList(ws);
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  async function spawn() {
    if (!activeProject) return;
    setBusy(true);
    setErr(null);
    try {
      const wt = await createWorktree(activeProject.root, note.trim() || undefined);
      setNote("");
      await refresh();
      setCurrent(wt.id, wt.path);
      onClose();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function pick(wt: Worktree) {
    setCurrent(wt.id, wt.path);
    onClose();
  }

  async function detach() {
    setCurrent(null, null);
    onClose();
  }

  async function removeOne(wt: Worktree) {
    setBusy(true);
    setErr(null);
    try {
      await removeWorktree(wt.id, true);
      if (currentId === wt.id) setCurrent(null, null);
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  if (!open) return null;
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal worktree-modal" onClick={(e) => e.stopPropagation()}>
        <h2>Worktrees{activeProject ? ` — ${activeProject.name}` : ""}</h2>
        {!activeProject && (
          <div className="muted">Pick an active project first to manage worktrees.</div>
        )}
        {activeProject && (
          <>
            <div className="worktree-list">
              <button
                className={`worktree-row ${!currentId ? "active" : ""}`}
                onClick={() => void detach()}
              >
                <div>
                  <strong>main</strong>
                  <div className="muted">no worktree — uses the project root directly</div>
                </div>
              </button>
              {list.map((wt) => (
                <div key={wt.id} className={`worktree-row ${currentId === wt.id ? "active" : ""}`}>
                  <button className="worktree-pick" onClick={() => void pick(wt)}>
                    <strong>{wt.branch}</strong>
                    <div className="muted" style={{ fontSize: 11 }}>
                      {wt.path}
                    </div>
                    {wt.notes && <div className="muted" style={{ fontSize: 11 }}>“{wt.notes}”</div>}
                  </button>
                  <button
                    className="link-btn danger"
                    onClick={() => void removeOne(wt)}
                    disabled={busy}
                    title="archive + remove"
                  >
                    Remove
                  </button>
                </div>
              ))}
            </div>
            <hr />
            <label>
              Note (optional)
              <input
                value={note}
                onChange={(e) => setNote(e.target.value)}
                placeholder="what is this branch for?"
              />
            </label>
            <div className="modal-actions">
              <button onClick={onClose} disabled={busy}>Cancel</button>
              <button
                className="btn-primary"
                onClick={() => void spawn()}
                disabled={busy}
              >
                {busy ? "Creating…" : "Create worktree"}
              </button>
            </div>
            {err && <div style={{ color: "var(--danger)" }}>{err}</div>}
          </>
        )}
      </div>
    </div>
  );
}
