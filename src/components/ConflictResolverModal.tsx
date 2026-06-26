import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  confidenceTier,
  resolveConflicts,
  stageResolvedFiles,
  type ConflictReport,
  type ResolvedConflict,
} from "@/lib/conflict-resolver";
import { saveFileText } from "@/lib/editor-save";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * AI merge-conflict resolver modal. Renders as a self-mounting portal so the
 * `/conflict` slash command can summon it without App.tsx wiring — same
 * pattern as `RefactorSuggesterModal` / `DocGenModal`.
 *
 * On mount we kick off `resolve_conflicts` against the active project's
 * root. The left pane lists conflicted files with a confidence pill; the
 * right pane shows the original conflicted body next to the AI proposal.
 *
 * Per-file actions:
 *   - "Accept resolution" → `save_file_text` for that path, then mark
 *     accepted in local state (so the "Stage all" button can pick it up).
 *   - "Skip" → flag local-only, leaves the file untouched.
 *
 * Footer action:
 *   - "Stage all accepted" → `stage_resolved_files` over the accepted set,
 *     posts a system note into chat with the per-file outcome.
 */

interface ConflictResolverModalProps {
  projectRoot: string;
  onClose: () => void;
}

type LocalStatus = "pending" | "accepted" | "skipped" | "saving" | "save-failed";

interface FileRow {
  file: ResolvedConflict;
  status: LocalStatus;
  error?: string;
}

export function ConflictResolverModal({
  projectRoot,
  onClose,
}: ConflictResolverModalProps) {
  const [report, setReport] = useState<ConflictReport | null>(null);
  const [rows, setRows] = useState<FileRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [selectedIdx, setSelectedIdx] = useState<number>(0);
  const [staging, setStaging] = useState(false);

  // Kick off the analysis on mount. Cancellation guards against unmount
  // races — the user can hit ESC while the gateway call is still going.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await resolveConflicts(projectRoot);
        if (cancelled) return;
        setReport(out);
        setRows(out.files.map((f) => ({ file: f, status: "pending" })));
        setSelectedIdx(0);
      } catch (e) {
        if (cancelled) return;
        setError(humanizeError(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectRoot]);

  // ESC closes the modal — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const selected = rows[selectedIdx] ?? null;

  const acceptedCount = useMemo(
    () => rows.filter((r) => r.status === "accepted").length,
    [rows],
  );

  const onAccept = useCallback(
    async (idx: number) => {
      const row = rows[idx];
      if (!row) return;
      // Optimistic UI — flip to "saving" before the write so the spinner
      // shows immediately. Failure flips us back with the error attached.
      setRows((prev) =>
        prev.map((r, i) => (i === idx ? { ...r, status: "saving" } : r)),
      );
      try {
        const abs = row.file.path.startsWith("/")
          ? row.file.path
          : `${projectRoot.replace(/\/$/, "")}/${row.file.path}`;
        await saveFileText(abs, row.file.after);
        setRows((prev) =>
          prev.map((r, i) =>
            i === idx ? { ...r, status: "accepted", error: undefined } : r,
          ),
        );
        pushToast({
          title: "Resolution accepted",
          body: row.file.path,
          kind: "success",
        });
      } catch (e) {
        setRows((prev) =>
          prev.map((r, i) =>
            i === idx ? { ...r, status: "save-failed", error: humanizeError(e) } : r,
          ),
        );
        pushToast({
          title: "Save failed",
          body: humanizeError(e),
          kind: "error",
        });
      }
    },
    [rows, projectRoot],
  );

  const onSkip = useCallback((idx: number) => {
    setRows((prev) =>
      prev.map((r, i) => (i === idx ? { ...r, status: "skipped" } : r)),
    );
  }, []);

  const onStageAll = useCallback(async () => {
    const acceptedPaths = rows
      .filter((r) => r.status === "accepted")
      .map((r) => r.file.path);
    if (acceptedPaths.length === 0) {
      pushToast({
        title: "Nothing to stage",
        body: "Accept at least one resolution first.",
        kind: "warning",
      });
      return;
    }
    setStaging(true);
    try {
      const stageReport = await stageResolvedFiles(projectRoot, acceptedPaths);
      // Surface as both a toast (transient) and a chat system note (durable)
      // so the user has a record of what landed.
      const note = [
        `🔀 **Merge conflict resolutions staged** — ${stageReport.staged.length}/${acceptedPaths.length} ok`,
        "",
        ...(stageReport.staged.length > 0
          ? ["Staged:", ...stageReport.staged.map((p) => `- \`${p}\``)]
          : []),
        ...(stageReport.errors.length > 0
          ? ["", "Errors:", ...stageReport.errors.map((e) => `- ${e}`)]
          : []),
      ].join("\n");
      useCortexStore.getState().appendMessage({
        id: `cf-${crypto.randomUUID()}`,
        role: "system",
        content: note,
        tools: [],
      });
      pushToast({
        title: "Stage complete",
        body: `${stageReport.staged.length} ok, ${stageReport.errors.length} errors`,
        kind: stageReport.errors.length === 0 ? "success" : "warning",
      });
    } catch (e) {
      pushToast({
        title: "Stage failed",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setStaging(false);
    }
  }, [rows, projectRoot]);

  return (
    <div className="conflict-resolver-backdrop" onClick={onClose}>
      <div
        className="conflict-resolver-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="conflict-resolver-title"
      >
        <header className="conflict-resolver-header">
          <div>
            <h2 id="conflict-resolver-title">AI merge conflict resolver</h2>
            <div className="conflict-resolver-path" title={projectRoot}>
              {projectRoot}
            </div>
          </div>
          <button
            className="conflict-resolver-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="conflict-resolver-body">
          {loading && (
            <div className="conflict-resolver-loading">
              <span className="conflict-resolver-spinner" aria-hidden /> Scanning for conflicts and asking the gateway for resolutions…
            </div>
          )}
          {error && !loading && (
            <div className="conflict-resolver-error">
              Failed to scan: <pre>{error}</pre>
            </div>
          )}
          {!loading && !error && report && rows.length === 0 && (
            <div className="conflict-resolver-empty">
              {report.errors.length > 0 ? (
                <>
                  <p>No resolvable conflicts found.</p>
                  <pre>{report.errors.join("\n")}</pre>
                </>
              ) : (
                <p>No merge conflicts in this repo.</p>
              )}
            </div>
          )}
          {!loading && !error && rows.length > 0 && selected && (
            <div className="conflict-resolver-split">
              <aside className="conflict-resolver-list" aria-label="Conflicted files">
                {rows.map((row, i) => {
                  const tier = confidenceTier(row.file.confidence);
                  return (
                    <button
                      key={row.file.path}
                      type="button"
                      className="conflict-resolver-list-item"
                      data-active={i === selectedIdx}
                      data-status={row.status}
                      onClick={() => setSelectedIdx(i)}
                    >
                      <span
                        className="conflict-resolver-pill"
                        data-tier={tier}
                        title={`Confidence ${(row.file.confidence * 100).toFixed(0)}%`}
                      >
                        {row.file.ai_chosen_side}
                      </span>
                      <span className="conflict-resolver-list-path" title={row.file.path}>
                        {row.file.path}
                      </span>
                      <span
                        className="conflict-resolver-status"
                        data-status={row.status}
                      >
                        {statusLabel(row.status)}
                      </span>
                    </button>
                  );
                })}
                {report && report.errors.length > 0 && (
                  <details className="conflict-resolver-skipped">
                    <summary>Skipped ({report.errors.length})</summary>
                    <ul>
                      {report.errors.map((e, i) => (
                        <li key={i}>{e}</li>
                      ))}
                    </ul>
                  </details>
                )}
              </aside>

              <section className="conflict-resolver-detail" aria-live="polite">
                <div className="conflict-resolver-detail-head">
                  <div className="conflict-resolver-detail-path" title={selected.file.path}>
                    {selected.file.path}
                  </div>
                  <div className="conflict-resolver-detail-actions">
                    <button
                      type="button"
                      className="conflict-resolver-action conflict-resolver-action-primary"
                      onClick={() => onAccept(selectedIdx)}
                      disabled={
                        selected.status === "accepted" ||
                        selected.status === "saving"
                      }
                    >
                      {selected.status === "saving"
                        ? "Saving…"
                        : selected.status === "accepted"
                          ? "Accepted"
                          : "Accept resolution"}
                    </button>
                    <button
                      type="button"
                      className="conflict-resolver-action"
                      onClick={() => onSkip(selectedIdx)}
                      disabled={selected.status === "skipped"}
                    >
                      Skip
                    </button>
                  </div>
                </div>
                {selected.error && (
                  <div className="conflict-resolver-detail-error">
                    {selected.error}
                  </div>
                )}
                <div className="conflict-resolver-diff">
                  <div className="conflict-resolver-diff-col">
                    <div className="conflict-resolver-diff-label">
                      Original (with markers)
                    </div>
                    <pre>{selected.file.before}</pre>
                  </div>
                  <div className="conflict-resolver-diff-col">
                    <div className="conflict-resolver-diff-label">
                      AI proposed resolution
                    </div>
                    <pre>{selected.file.after}</pre>
                  </div>
                </div>
              </section>
            </div>
          )}
        </div>

        <footer className="conflict-resolver-footer">
          <span className="conflict-resolver-footer-summary">
            {rows.length > 0
              ? `${acceptedCount}/${rows.length} accepted`
              : ""}
          </span>
          <button
            type="button"
            className="conflict-resolver-action conflict-resolver-action-primary"
            onClick={onStageAll}
            disabled={acceptedCount === 0 || staging}
          >
            {staging ? "Staging…" : `Stage all accepted (${acceptedCount})`}
          </button>
          <button className="conflict-resolver-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

function statusLabel(s: LocalStatus): string {
  switch (s) {
    case "accepted":
      return "✓ accepted";
    case "saving":
      return "saving…";
    case "skipped":
      return "skipped";
    case "save-failed":
      return "save failed";
    default:
      return "pending";
  }
}

/**
 * Imperative summoner used by the `/conflict` slash command. Same detached-
 * root pattern as `RefactorSuggesterModal`.
 */
let activeRoot: Root | null = null;

export function openConflictResolverModal(projectRoot: string): void {
  if (activeRoot) return; // already open
  if (!projectRoot) {
    pushToast({
      title: "No project",
      body: "Pick a project from the sidebar before running /conflict.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "conflict-resolver";
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
  root.render(<ConflictResolverModal projectRoot={projectRoot} onClose={close} />);
}
