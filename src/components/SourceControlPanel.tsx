import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { useCortexStore } from "@/state/store";
import {
  gitCommit,
  gitDiscardChanges,
  gitFileDiff,
  gitStageFile,
  gitUnstageFile,
  gitWorkingStatus,
  type DiffMode,
  type FileEntry,
  type WorkingStatus,
} from "@/lib/git";
import { gitPush, summarizePushResult } from "@/lib/git-push";
import { gitPull, summarizePullResult } from "@/lib/git-pull";
import { parseUnifiedDiff } from "@/lib/diff";
import { PanelLoading } from "./Skeleton";
import "@/styles/source-control.css";

/** A file row the user picked to inspect: which file, against which side. */
interface DiffSelection {
  path: string;
  mode: DiffMode;
}

/**
 * VSCode-style source control panel:
 *   - Branch chip with ahead/behind counters
 *   - Sections: Staged / Unstaged / Untracked
 *   - Click a file row to open its unified diff inline (staged rows diff
 *     index↔HEAD, unstaged rows working-tree↔index, untracked rows show the
 *     whole file as additions)
 *   - Commit message + "Commit staged" button at the bottom
 *
 * Polls `git status` every 5s; manually re-fetches after every mutation.
 */
export function SourceControlPanel() {
  const project = useCortexStore((s) => s.activeProject);
  const root = project?.root ?? null;

  const [status, setStatus] = useState<WorkingStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [netBusy, setNetBusy] = useState<null | "push" | "pull">(null);
  const [selected, setSelected] = useState<DiffSelection | null>(null);

  const refresh = useCallback(async () => {
    if (!root) {
      setStatus(null);
      return;
    }
    try {
      const s = await gitWorkingStatus(root);
      setStatus(s);
      setError(null);
    } catch (e) {
      setError(humanizeError(e));
    }
  }, [root]);

  useEffect(() => {
    void refresh();
    setSelected(null); // switching projects invalidates any open diff
    if (!root) return;
    const id = setInterval(refresh, 5_000);
    return () => clearInterval(id);
  }, [refresh, root]);

  // If the inspected file leaves its section (staged, discarded, committed…),
  // close the diff pane instead of showing a stale or empty patch.
  useEffect(() => {
    if (!selected || !status) return;
    const stillThere =
      selected.mode === "staged"
        ? status.staged.some((f) => f.path === selected.path)
        : selected.mode === "unstaged"
          ? status.unstaged.some((f) => f.path === selected.path)
          : status.untracked.includes(selected.path);
    if (!stillThere) setSelected(null);
  }, [status, selected]);

  function toggleDiff(path: string, mode: DiffMode) {
    setSelected((prev) =>
      prev && prev.path === path && prev.mode === mode ? null : { path, mode },
    );
  }

  async function run(action: () => Promise<void>) {
    if (!root) return;
    setBusy(true);
    try {
      await action();
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function handlePush() {
    if (!root) return;
    setNetBusy("push");
    try {
      const r = await gitPush(root);
      setError(r.ok ? null : `Push failed: ${summarizePushResult(r)}`);
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setNetBusy(null);
    }
  }

  async function handlePull() {
    if (!root) return;
    setNetBusy("pull");
    try {
      const r = await gitPull(root);
      setError(r.ok ? null : `Pull failed: ${summarizePullResult(r)}`);
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setNetBusy(null);
    }
  }

  if (!root) {
    return (
      <div className="git-scm-empty muted">
        Open a project to see its working tree.
      </div>
    );
  }

  const canCommit =
    !!status && status.staged.length > 0 && message.trim().length > 0 && !busy;

  return (
    <div className="git-scm">
      <div className="git-scm-branch">
        <span className="git-scm-branch-name">
          {status?.branch ?? "(loading)"}
        </span>
        {status && (status.ahead > 0 || status.behind > 0) && (
          <span className="git-scm-aheadbehind">
            {status.ahead > 0 && <span title="ahead">↑{status.ahead}</span>}
            {status.behind > 0 && <span title="behind">↓{status.behind}</span>}
          </span>
        )}
        <span className="git-scm-syncbtns">
          <button
            type="button"
            className="git-scm-sync-btn"
            title="git pull --ff-only"
            disabled={netBusy !== null || (!!status && status.behind === 0)}
            onClick={() => void handlePull()}
          >
            {netBusy === "pull" ? "Pulling…" : `↓ Pull`}
          </button>
          <button
            type="button"
            className="git-scm-sync-btn"
            title="git push origin HEAD"
            disabled={netBusy !== null || (!!status && status.ahead === 0)}
            onClick={() => void handlePush()}
          >
            {netBusy === "push" ? "Pushing…" : `↑ Push`}
          </button>
        </span>
      </div>

      {error && <div className="git-scm-error">{error}</div>}

      {!error && (
        <>
          <Section
            title="Staged"
            files={status?.staged ?? []}
            actionLabel="Unstage"
            onAction={(path) =>
              run(() => gitUnstageFile(root, path))
            }
            selectedPath={selected?.mode === "staged" ? selected.path : null}
            onOpenDiff={(path) => toggleDiff(path, "staged")}
          />
          <Section
            title="Unstaged"
            files={status?.unstaged ?? []}
            actionLabel="Stage"
            onAction={(path) =>
              run(() => gitStageFile(root, path))
            }
            extraAction={{
              label: "Discard",
              onAction: (path) =>
                run(async () => {
                  const ok = await confirmDialog({
                    title: "Discard changes?",
                    message: `Local changes to ${path} will be discarded. This cannot be undone.`,
                    confirmLabel: "Discard",
                    danger: true,
                  });
                  if (ok) await gitDiscardChanges(root, path);
                }),
            }}
            selectedPath={selected?.mode === "unstaged" ? selected.path : null}
            onOpenDiff={(path) => toggleDiff(path, "unstaged")}
          />
          <UntrackedSection
            files={status?.untracked ?? []}
            onStage={(path) => run(() => gitStageFile(root, path))}
            selectedPath={selected?.mode === "untracked" ? selected.path : null}
            onOpenDiff={(path) => toggleDiff(path, "untracked")}
          />
          {selected && (
            <DiffPane
              root={root}
              selection={selected}
              refreshToken={status}
              onClose={() => setSelected(null)}
            />
          )}
        </>
      )}

      <div className="git-scm-commit">
        <textarea
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder="Commit message…"
          rows={3}
          className="git-scm-commit-input"
        />
        <button
          type="button"
          className="git-scm-commit-btn"
          disabled={!canCommit}
          onClick={() =>
            run(async () => {
              await gitCommit(root, message);
              setMessage("");
            })
          }
        >
          Commit staged
        </button>
      </div>
    </div>
  );
}

function Section({
  title,
  files,
  actionLabel,
  onAction,
  extraAction,
  selectedPath,
  onOpenDiff,
}: {
  title: string;
  files: FileEntry[];
  actionLabel: string;
  onAction: (path: string) => void;
  extraAction?: { label: string; onAction: (path: string) => void };
  selectedPath: string | null;
  onOpenDiff: (path: string) => void;
}) {
  return (
    <div className="git-scm-section">
      <div className="git-scm-section-head">
        <span>{title}</span>
        <span className="muted">{files.length}</span>
      </div>
      {files.length === 0 ? (
        <div className="muted git-scm-empty-row">—</div>
      ) : (
        files.map((f) => (
          <div
            key={`${title}-${f.path}`}
            className={`git-scm-row${
              selectedPath === f.path ? " git-scm-row-selected" : ""
            }`}
          >
            <span className="git-scm-status">{f.status}</span>
            <button
              type="button"
              className="git-scm-path git-scm-path-btn"
              title={`Show diff for ${f.path}`}
              onClick={() => onOpenDiff(f.path)}
            >
              {f.path}
            </button>
            <button
              type="button"
              className="git-scm-row-btn"
              onClick={() => onAction(f.path)}
            >
              {actionLabel}
            </button>
            {extraAction && (
              <button
                type="button"
                className="git-scm-row-btn git-scm-row-btn-danger"
                onClick={() => extraAction.onAction(f.path)}
              >
                {extraAction.label}
              </button>
            )}
          </div>
        ))
      )}
    </div>
  );
}

function UntrackedSection({
  files,
  onStage,
  selectedPath,
  onOpenDiff,
}: {
  files: string[];
  onStage: (path: string) => void;
  selectedPath: string | null;
  onOpenDiff: (path: string) => void;
}) {
  return (
    <div className="git-scm-section">
      <div className="git-scm-section-head">
        <span>Untracked</span>
        <span className="muted">{files.length}</span>
      </div>
      {files.length === 0 ? (
        <div className="muted git-scm-empty-row">—</div>
      ) : (
        files.map((path) => (
          <div
            key={`untracked-${path}`}
            className={`git-scm-row${
              selectedPath === path ? " git-scm-row-selected" : ""
            }`}
          >
            <span className="git-scm-status">?</span>
            <button
              type="button"
              className="git-scm-path git-scm-path-btn"
              title={`Show contents of ${path}`}
              onClick={() => onOpenDiff(path)}
            >
              {path}
            </button>
            <button
              type="button"
              className="git-scm-row-btn"
              onClick={() => onStage(path)}
            >
              Stage
            </button>
          </div>
        ))
      )}
    </div>
  );
}

const MODE_LABEL: Record<DiffMode, string> = {
  staged: "index ↔ HEAD",
  unstaged: "working tree ↔ index",
  untracked: "new file",
};

/**
 * Inline read-only diff for the selected file. Fetches `git diff` (staged or
 * unstaged as appropriate; untracked files come back as an all-additions
 * patch) and renders it through the same parser + row palette the chat's
 * hunk review uses, so SCM diffs match the rest of the app.
 *
 * Re-fetches when the selection changes and on every status poll tick
 * (`refreshToken`) so the patch tracks ongoing edits; the skeleton only
 * shows on the first load of a selection so refreshes never flicker.
 */
function DiffPane({
  root,
  selection,
  refreshToken,
  onClose,
}: {
  root: string;
  selection: DiffSelection;
  /** Any value that changes when the working tree may have moved. */
  refreshToken: unknown;
  onClose: () => void;
}) {
  const [diff, setDiff] = useState<string | null>(null);
  const [diffError, setDiffError] = useState<string | null>(null);
  const loadedKeyRef = useRef<string | null>(null);

  const key = `${selection.mode}\0${selection.path}`;
  const loading = diff === null && diffError === null;

  useEffect(() => {
    let cancelled = false;
    if (loadedKeyRef.current !== key) {
      // New selection — show the skeleton until the first payload lands.
      loadedKeyRef.current = key;
      setDiff(null);
      setDiffError(null);
    }
    gitFileDiff(root, selection.path, selection.mode)
      .then((text) => {
        if (cancelled) return;
        setDiff(text);
        setDiffError(null);
      })
      .catch((e) => {
        if (cancelled) return;
        setDiffError(humanizeError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [root, key, selection.path, selection.mode, refreshToken]);

  const parsed = useMemo(
    () => (diff !== null ? parseUnifiedDiff(diff) : null),
    [diff],
  );

  return (
    <div className="git-scm-diff">
      <div className="git-scm-diff-head">
        <span className="git-scm-diff-path" title={selection.path}>
          {selection.path}
        </span>
        <span className="git-scm-diff-mode">{MODE_LABEL[selection.mode]}</span>
        <button
          type="button"
          className="git-scm-diff-close"
          title="Close diff"
          aria-label="Close diff"
          onClick={onClose}
        >
          ×
        </button>
      </div>

      {diffError && <div className="git-scm-error">{diffError}</div>}

      {loading && !diffError && (
        <PanelLoading lines={4} label="Loading diff" />
      )}

      {parsed && !diffError && (
        parsed.hunks.length === 0 ? (
          diff !== null && diff.trim().length > 0 ? (
            // No @@ hunks but git said something — binary file, mode change,
            // or our truncation stub. Show the raw text rather than nothing.
            <pre className="git-scm-diff-raw">{diff.trim()}</pre>
          ) : (
            <div className="muted git-scm-diff-empty">
              No changes to show — this file matches the{" "}
              {selection.mode === "staged" ? "last commit" : "index"}.
            </div>
          )
        ) : (
          <div className="git-scm-diff-body">
            {parsed.hunks.map((hunk, hi) => (
              <div key={hi} className="git-scm-diff-hunk">
                <code className="git-scm-diff-hunkhead">
                  @@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},
                  {hunk.newCount} @@
                </code>
                <pre className="hunk-body">
                  {hunk.rows.map((row, ri) => (
                    <div key={ri} className={`hunk-row hunk-row-${row.kind}`}>
                      <span className="hunk-marker">
                        {row.kind === "add"
                          ? "+"
                          : row.kind === "del"
                            ? "-"
                            : " "}
                      </span>
                      <span className="hunk-text">{row.text}</span>
                    </div>
                  ))}
                </pre>
              </div>
            ))}
          </div>
        )
      )}
    </div>
  );
}
