import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  gitStashApply,
  gitStashDrop,
  gitStashList,
  gitStashPop,
  gitStashSave,
  gitStashShow,
  summarizeStashOp,
  type Stash,
  type StashOpResult,
} from "@/lib/git-stash";
import { pushToast } from "@/lib/toast";
import { confirmDialog } from "@/lib/dialogs";
import { useCortexStore } from "@/state/store";

/**
 * Modal for the git stash manager (`/stash`). Self-mounting portal — same
 * pattern as IDEExportModal / DebuggerModal / KeyVaultPanel so App.tsx
 * doesn't have to wire anything up.
 *
 * Top section: "Stash current changes" action with optional message +
 * include-untracked toggle. Below: per-stash rows with Apply / Pop /
 * Drop / Diff buttons. Diff text is loaded on demand and truncated to
 * 32 KiB backend-side.
 */

interface StashManagerModalProps {
  onClose: () => void;
}

export function StashManagerModal({ onClose }: StashManagerModalProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [stashes, setStashes] = useState<Stash[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Header form state.
  const [message, setMessage] = useState("");
  const [includeUntracked, setIncludeUntracked] = useState(false);
  const [busy, setBusy] = useState(false);

  // Per-row in-flight + open diff state.
  const [activeRef, setActiveRef] = useState<string | null>(null);
  const [diffRef, setDiffRef] = useState<string | null>(null);
  const [diffText, setDiffText] = useState<string>("");
  const [diffLoading, setDiffLoading] = useState(false);

  // ESC closes the modal — standard transient-surface behaviour.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const refresh = useCallback(async () => {
    if (!activeProject) {
      setStashes([]);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const rows = await gitStashList(activeProject.root);
      setStashes(rows);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, [activeProject]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const reportOp = useCallback(
    (label: string, result: StashOpResult) => {
      const summary = summarizeStashOp(result);
      pushToast({
        title: result.ok ? `${label} ok` : `${label} failed`,
        body: summary,
        kind: result.ok ? "success" : "error",
      });
    },
    [],
  );

  const onSave = useCallback(async () => {
    if (!activeProject) {
      setError("No active project — pick one from the sidebar first.");
      return;
    }
    setBusy(true);
    try {
      const result = await gitStashSave(
        activeProject.root,
        message.trim() || null,
        includeUntracked,
      );
      reportOp("stash save", result);
      if (result.ok) {
        setMessage("");
      }
      await refresh();
    } catch (e) {
      pushToast({ title: "stash save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setBusy(false);
    }
  }, [activeProject, message, includeUntracked, refresh, reportOp]);

  const runRowAction = useCallback(
    async (
      label: string,
      refId: string,
      fn: (root: string, ref: string) => Promise<StashOpResult>,
      confirmFirst: boolean,
    ) => {
      if (!activeProject) return;
      if (
        confirmFirst &&
        !(await confirmDialog({
          title: `${label[0].toUpperCase()}${label.slice(1)} stash?`,
          message: `${label} ${refId}?`,
          confirmLabel: `${label[0].toUpperCase()}${label.slice(1)}`,
          danger: true,
        }))
      )
        return;
      setActiveRef(refId);
      try {
        const result = await fn(activeProject.root, refId);
        reportOp(`stash ${label}`, result);
        // If we just popped/dropped the entry whose diff is open, close it.
        if ((label === "pop" || label === "drop") && diffRef === refId) {
          setDiffRef(null);
          setDiffText("");
        }
        await refresh();
      } catch (e) {
        pushToast({
          title: `stash ${label} failed`,
          body: humanizeError(e),
          kind: "error",
        });
      } finally {
        setActiveRef(null);
      }
    },
    [activeProject, refresh, reportOp, diffRef],
  );

  const onDiff = useCallback(
    async (refId: string) => {
      if (!activeProject) return;
      if (diffRef === refId) {
        setDiffRef(null);
        setDiffText("");
        return;
      }
      setDiffRef(refId);
      setDiffText("");
      setDiffLoading(true);
      try {
        const text = await gitStashShow(activeProject.root, refId);
        setDiffText(text);
      } catch (e) {
        setDiffText(`# failed to load diff: ${humanizeError(e)}`);
      } finally {
        setDiffLoading(false);
      }
    },
    [activeProject, diffRef],
  );

  return (
    <div className="stash-manager-backdrop" onMouseDown={onClose}>
      <div
        className="stash-manager-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="stash-manager-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="stash-manager-header">
          <h2 id="stash-manager-title">Stash Manager</h2>
          <button
            className="stash-manager-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        {!activeProject ? (
          <p className="stash-manager-summary">
            <em>No active project — pick one from the sidebar first.</em>
          </p>
        ) : (
          <p className="stash-manager-summary">
            <code>{activeProject.name}</code> · {stashes.length} stash
            {stashes.length === 1 ? "" : "es"}
          </p>
        )}

        <section className="stash-manager-save">
          <input
            className="stash-manager-msg"
            type="text"
            placeholder="optional message (e.g. WIP refactor auth)"
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            disabled={!activeProject || busy}
          />
          <label className="stash-manager-toggle">
            <input
              type="checkbox"
              checked={includeUntracked}
              onChange={(e) => setIncludeUntracked(e.target.checked)}
              disabled={!activeProject || busy}
            />
            <span>include untracked</span>
          </label>
          <button
            className="stash-manager-save-btn"
            onClick={onSave}
            disabled={!activeProject || busy}
          >
            {busy ? "Stashing…" : "Stash current changes"}
          </button>
        </section>

        {error && <div className="stash-manager-error">{error}</div>}

        <section className="stash-manager-list">
          {loading && stashes.length === 0 ? (
            <p className="stash-manager-empty">Loading…</p>
          ) : stashes.length === 0 && !error ? (
            <p className="stash-manager-empty">No stashes yet.</p>
          ) : (
            <ul>
              {stashes.map((s) => {
                const rowBusy = activeRef === s.ref_id;
                const open = diffRef === s.ref_id;
                return (
                  <li key={s.ref_id} className="stash-manager-row">
                    <div className="stash-manager-row-head">
                      <code className="stash-manager-ref">{s.ref_id}</code>
                      <span className="stash-manager-subject">{s.subject}</span>
                      <span className="stash-manager-age">{s.age}</span>
                      <span
                        className="stash-manager-files"
                        title={`${s.files_changed} files changed`}
                      >
                        {s.files_changed} files
                      </span>
                    </div>
                    <div className="stash-manager-actions">
                      <button
                        onClick={() =>
                          void runRowAction("apply", s.ref_id, gitStashApply, false)
                        }
                        disabled={rowBusy}
                      >
                        Apply
                      </button>
                      <button
                        onClick={() =>
                          void runRowAction("pop", s.ref_id, gitStashPop, true)
                        }
                        disabled={rowBusy}
                      >
                        Pop
                      </button>
                      <button
                        className="stash-manager-danger"
                        onClick={() =>
                          void runRowAction("drop", s.ref_id, gitStashDrop, true)
                        }
                        disabled={rowBusy}
                      >
                        Drop
                      </button>
                      <button
                        onClick={() => void onDiff(s.ref_id)}
                        disabled={rowBusy}
                      >
                        {open ? "Hide diff" : "Diff"}
                      </button>
                    </div>
                    {open && (
                      <pre className="stash-manager-diff">
                        <code>
                          {diffLoading ? "loading diff…" : diffText || "(empty diff)"}
                        </code>
                      </pre>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </section>

        <footer className="stash-manager-footer">
          <button
            className="stash-manager-secondary"
            onClick={() => void refresh()}
            disabled={loading || !activeProject}
          >
            Refresh
          </button>
          <button className="stash-manager-secondary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/stash` slash. Creates a detached root
 * on `document.body` and tears it down on close. Idempotent — calling
 * twice in a row is a no-op while the modal is already open.
 */
let activeRoot: Root | null = null;

export function openStashManagerModal(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "stash-manager";
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
  root.render(<StashManagerModal onClose={close} />);
}
