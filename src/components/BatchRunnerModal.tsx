import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  BATCH_DEFAULT_PARALLELISM,
  BATCH_MAX_ITEMS,
  BATCH_MAX_PARALLELISM,
  BATCH_MIN_PARALLELISM,
  clampParallelism,
  formatBatchAsMarkdown,
  listenBatchProgress,
  runBatch,
  type BatchItem,
  type BatchProgressEvent,
  type BatchRunReport,
  type BatchStatus,
} from "@/lib/batch-runner";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * CrewAI-style `kickoff_for_each` modal — run one prompt across N items in
 * parallel via the gateway with per-item live progress. Self-mounting portal so
 * `/batch` can summon it without touching App.tsx wiring (same pattern as
 * `/test`, `/refactor`, `/docgen`, etc.).
 *
 * Items input is one-per-line so users can paste file lists, ticket IDs,
 * URLs, etc. without quoting. The "Pick files" button opens the OS
 * multi-select dialog scoped to the active project root when present.
 *
 * Progress events flow over `batch:progress:<run_id>` and update each row's
 * status pill + (collapsed) output preview as deltas stream in. The run_id
 * is generated server-side; we subscribe AFTER kickoff with a tiny race
 * window where the first few "queued" events for a tight batch may arrive
 * before our listener is attached — acceptable because the final report
 * carries the authoritative state.
 */

interface Props {
  initialItems?: string[];
  initialPrompt?: string;
  onClose: () => void;
}

interface RowState {
  index: number;
  item: string;
  status: BatchStatus;
  output: string;
  error?: string;
  tokens?: number;
  latencyMs?: number;
}

function statusLabel(s: BatchStatus): string {
  switch (s) {
    case "queued":
      return "queued";
    case "running":
      return "running…";
    case "done":
      return "done";
    case "error":
      return "error";
  }
}

export function BatchRunnerModal({
  initialItems,
  initialPrompt,
  onClose,
}: Props) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [itemsText, setItemsText] = useState<string>(
    (initialItems ?? []).join("\n"),
  );
  const [prompt, setPrompt] = useState<string>(initialPrompt ?? "");
  const [parallelism, setParallelism] = useState<number>(
    BATCH_DEFAULT_PARALLELISM,
  );
  const [running, setRunning] = useState(false);
  const [rows, setRows] = useState<RowState[]>([]);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [report, setReport] = useState<BatchRunReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !running) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, running]);

  // Make sure we always reap the progress listener on unmount even if the
  // run is still inflight when the user force-closes (Escape is blocked
  // mid-run but the user can `cmd+w` the window).
  useEffect(() => {
    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, []);

  const parsedItems = useMemo<string[]>(() => {
    return itemsText
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean);
  }, [itemsText]);

  const canRun =
    !running &&
    parsedItems.length > 0 &&
    parsedItems.length <= BATCH_MAX_ITEMS &&
    prompt.trim().length > 0;

  const onPickFiles = useCallback(async () => {
    try {
      const picked = await openFileDialog({
        multiple: true,
        directory: false,
        defaultPath: activeProject?.root,
        title: "Pick files for batch",
      });
      if (!picked) return;
      const paths = Array.isArray(picked) ? picked : [picked];
      const next = [...parsedItems, ...paths].slice(0, BATCH_MAX_ITEMS);
      setItemsText(next.join("\n"));
    } catch (e) {
      pushToast({ title: "Pick files failed", body: humanizeError(e), kind: "error" });
    }
  }, [activeProject, parsedItems]);

  const onRun = useCallback(async () => {
    if (!canRun) return;
    setRunning(true);
    setError(null);
    setReport(null);
    setExpanded(new Set());

    // Seed the table immediately so the user sees something even before
    // the first "queued" event arrives. The Rust side emits one queued
    // event per item up front so we'll quickly converge.
    const seed: RowState[] = parsedItems.map((item, index) => ({
      index,
      item,
      status: "queued",
      output: "",
    }));
    setRows(seed);

    try {
      // The backend's run_id isn't known until the invoke resolves, but
      // the final report's items[] is the source of truth for terminal
      // state — we reconcile from it below, so any missed mid-flight
      // deltas don't leave rows stuck in "running".
      const rep = await runBatch(parsedItems, prompt, parallelism);
      setRows(rep.items.map((it) => rowFromItem(it)));
      setReport(rep);
      pushToast({
        title: "Batch complete",
        body: `${rep.items.filter((i) => i.status === "done").length}/${rep.items.length} succeeded`,
        kind: rep.items.some((i) => i.status === "error") ? "warning" : "success",
      });
    } catch (e) {
      setError(humanizeError(e));
      pushToast({ title: "Batch failed", body: humanizeError(e), kind: "error" });
    } finally {
      setRunning(false);
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    }
  }, [canRun, parsedItems, prompt, parallelism]);

  useEffect(() => {
    if (!report) return;
    // Re-attach the listener using the now-known run_id so the modal can
    // re-render rows if the user re-runs the same batch. No-op if the
    // backend has already finished emitting.
    let unlisten: UnlistenFn | null = null;
    (async () => {
      unlisten = await listenBatchProgress(report.run_id, applyProgress);
    })();
    return () => {
      if (unlisten) unlisten();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [report?.run_id]);

  const applyProgress = useCallback((ev: BatchProgressEvent) => {
    setRows((prev) => {
      const next = prev.slice();
      const i = next.findIndex((r) => r.index === ev.item_index);
      if (i < 0) return prev;
      next[i] = {
        ...next[i],
        status: ev.status,
        output: ev.partial_output ?? next[i].output,
        error: ev.error ?? next[i].error,
      };
      return next;
    });
  }, []);

  const toggle = (idx: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) next.delete(idx);
      else next.add(idx);
      return next;
    });
  };

  const onCopyAll = useCallback(async () => {
    if (!report) return;
    const md = formatBatchAsMarkdown(report);
    try {
      await navigator.clipboard.writeText(md);
      pushToast({ title: "Copied", body: "Outputs on clipboard as markdown.", kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [report]);

  const counts = useMemo(() => {
    let queued = 0;
    let runningN = 0;
    let done = 0;
    let errored = 0;
    for (const r of rows) {
      switch (r.status) {
        case "queued":
          queued++;
          break;
        case "running":
          runningN++;
          break;
        case "done":
          done++;
          break;
        case "error":
          errored++;
          break;
      }
    }
    return { queued, running: runningN, done, errored };
  }, [rows]);

  return (
    <div className="batch-runner-backdrop" onMouseDown={running ? undefined : onClose}>
      <div
        className="batch-runner-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="batch-runner-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="batch-runner-header">
          <h2 id="batch-runner-title">Batch run</h2>
          <button
            className="batch-runner-close"
            onClick={onClose}
            disabled={running}
            title={running ? "Wait for the batch to finish" : "Close"}
          >
            ×
          </button>
        </header>

        <section className="batch-runner-config">
          <label className="batch-runner-label">
            Prompt template (<code>{"{{item}}"}</code> is substituted per row)
            <textarea
              className="batch-runner-textarea"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="Summarise {{item}} in one sentence."
              rows={4}
              disabled={running}
            />
          </label>

          <label className="batch-runner-label">
            Items (one per line — file paths, URLs, ticket IDs, anything)
            <textarea
              className="batch-runner-textarea"
              value={itemsText}
              onChange={(e) => setItemsText(e.target.value)}
              placeholder={"src/lib/foo.ts\nsrc/lib/bar.ts\nALPHA-42"}
              rows={6}
              disabled={running}
            />
          </label>

          <div className="batch-runner-controls">
            <button
              className="batch-runner-secondary"
              onClick={onPickFiles}
              disabled={running}
              type="button"
            >
              + Pick files
            </button>

            <label className="batch-runner-slider-label">
              Parallelism: <strong>{parallelism}</strong>
              <input
                type="range"
                min={BATCH_MIN_PARALLELISM}
                max={BATCH_MAX_PARALLELISM}
                step={1}
                value={parallelism}
                onChange={(e) =>
                  setParallelism(clampParallelism(Number(e.target.value)))
                }
                disabled={running}
              />
            </label>

            <span className="batch-runner-count">
              {parsedItems.length} item{parsedItems.length === 1 ? "" : "s"}
              {parsedItems.length > BATCH_MAX_ITEMS && (
                <span className="batch-runner-warn">
                  {" "}
                  (over {BATCH_MAX_ITEMS} cap)
                </span>
              )}
            </span>

            <button
              className="batch-runner-primary"
              onClick={onRun}
              disabled={!canRun}
              type="button"
            >
              {running ? "Running…" : "Run batch"}
            </button>
          </div>
        </section>

        {error && <div className="batch-runner-error">{error}</div>}

        {rows.length > 0 && (
          <section className="batch-runner-rows">
            <header className="batch-runner-summary">
              <span>queued: {counts.queued}</span>
              <span>running: {counts.running}</span>
              <span>done: {counts.done}</span>
              <span>errors: {counts.errored}</span>
            </header>
            <ul className="batch-runner-list">
              {rows.map((r) => (
                <li
                  key={r.index}
                  className={`batch-runner-row batch-runner-row-${r.status}`}
                >
                  <button
                    className="batch-runner-row-head"
                    onClick={() => toggle(r.index)}
                    type="button"
                  >
                    <span className={`batch-runner-pill batch-runner-pill-${r.status}`}>
                      {statusLabel(r.status)}
                    </span>
                    <span className="batch-runner-item-name" title={r.item}>
                      {r.item}
                    </span>
                    {r.tokens !== undefined && r.tokens > 0 && (
                      <span className="batch-runner-tokens">{r.tokens} tok</span>
                    )}
                    {r.latencyMs !== undefined && r.latencyMs > 0 && (
                      <span className="batch-runner-latency">
                        {(r.latencyMs / 1000).toFixed(1)}s
                      </span>
                    )}
                  </button>
                  {expanded.has(r.index) && (
                    <div className="batch-runner-row-body">
                      {r.error ? (
                        <pre className="batch-runner-output batch-runner-output-error">
                          {r.error}
                        </pre>
                      ) : (
                        <pre className="batch-runner-output">
                          {r.output || (r.status === "running" ? "…" : "(empty)")}
                        </pre>
                      )}
                    </div>
                  )}
                </li>
              ))}
            </ul>
          </section>
        )}

        {report && (
          <footer className="batch-runner-footer">
            <button
              className="batch-runner-secondary"
              onClick={onCopyAll}
              type="button"
            >
              Copy all outputs as markdown
            </button>
          </footer>
        )}
      </div>
    </div>
  );
}

function rowFromItem(it: BatchItem): RowState {
  return {
    index: it.index,
    item: it.item,
    status: it.status,
    output: it.output,
    error: it.error ?? undefined,
    tokens: it.tokens,
    latencyMs: it.latency_ms,
  };
}

// ---------- imperative summoner ----------

let activeRoot: Root | null = null;

export function openBatchRunnerModal(
  initial?: { items?: string[]; prompt?: string },
): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "batch-runner";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(
    <BatchRunnerModal
      initialItems={initial?.items}
      initialPrompt={initial?.prompt}
      onClose={close}
    />,
  );
}
