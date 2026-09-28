/**
 * Agent eval / benchmark harness panel.
 *
 * Runs a model against the coding-task set, scoring each answer against a
 * substring rubric, and shows a scored report (pass/total, average score,
 * per-task pass/fail + latency). Past runs are listed so quality is
 * comparable over time — and across models: the header's model picker reuses
 * the composer's model universe (`listModels`, refreshed on `models:changed`),
 * so the Ollama model the Cookbook just pulled is immediately benchmarkable.
 *
 * The run itself lives in the GLOBAL JOB STORE (`state/jobs.ts`,
 * `startEvalRun`) — this panel only renders the store, so switching tabs
 * mid-benchmark loses neither the progress bar nor the finished report.
 *
 * Bindings live in `src/lib/eval.ts`.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Gauge, Check, X } from "lucide-react";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { useJobs, startEvalRun } from "@/state/jobs";
import { PanelLoading } from "./Skeleton";
import {
  listEvalTasks,
  listEvalReports,
  listRetrievalEvalReports,
  listRetrievalEvalTasks,
  runRetrievalEval,
  type EvalReport,
  type RetrievalEvalReport,
} from "@/lib/eval";
import {
  listModels,
  onModelsChanged,
  groupModelsBySource,
  sourceMeta,
  type ModelEntry,
} from "@/lib/models";

import "../styles/eval.css";

/** Last-picked benchmark model survives panel remounts + app restarts. */
const MODEL_PICK_KEY = "cortex.eval.model";

export function EvalPanel() {
  const [taskCount, setTaskCount] = useState(0);
  // History browsing is panel-local; the live run + its report come from the
  // job store so they survive tab switches.
  const [viewedPast, setViewedPast] = useState<EvalReport | null>(null);
  const [history, setHistory] = useState<EvalReport[]>([]);
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [model, setModel] = useState<string>(
    () => localStorage.getItem(MODEL_PICK_KEY) ?? "",
  );
  // Surfaces a backend failure to load the task set / past runs — otherwise a
  // down backend reads as "0 benchmark tasks, no history" (a fresh install).
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  // Retrieval-quality baseline (fixture-driven, embeds via local Ollama). The
  // run is short and panel-local, so it doesn't go through the job store.
  const [retTaskCount, setRetTaskCount] = useState(0);
  const [retHistory, setRetHistory] = useState<RetrievalEvalReport[]>([]);
  const [retViewed, setRetViewed] = useState<RetrievalEvalReport | null>(null);
  const [retRunning, setRetRunning] = useState(false);
  const [retError, setRetError] = useState<string | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const evalRun = useJobs((s) => s.evalRun);

  const running = evalRun.progress !== null;
  const progress = evalRun.progress;
  const error = evalRun.error;
  const report =
    viewedPast ?? evalRun.report ?? (history.length ? history[0] : null);

  // Reload on mount AND whenever a run settles (running flips false) — a run
  // that finished while another tab was open appended to history.
  const reload = useCallback(async () => {
    try {
      const [tasks, reports, retTasks, retReports] = await Promise.all([
        listEvalTasks(),
        listEvalReports(),
        listRetrievalEvalTasks(),
        listRetrievalEvalReports(),
      ]);
      setTaskCount(tasks.length);
      setHistory(reports);
      setRetTaskCount(retTasks.length);
      setRetHistory(retReports);
      setLoadError(null);
    } catch (e) {
      const msg = humanizeError(e);
      setLoadError(msg);
      pushToast({ title: "Couldn't load eval data", body: msg, kind: "error" });
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload, running]);

  // Model universe: same source as the composer picker, refreshed live when a
  // Cookbook pull lands a new tag ("pull it, then benchmark it").
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const refresh = async () => {
      try {
        const list = await listModels();
        if (!disposed) setModels(list);
      } catch {
        if (!disposed) setModels([]);
      }
    };
    void refresh();
    void onModelsChanged(() => void refresh()).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // If the persisted pick vanished from the universe (model removed, server
  // gone), fall back to the default route instead of a blank select.
  useEffect(() => {
    if (model && models.length > 0 && !models.some((m) => m.id === model)) {
      setModel("");
      localStorage.removeItem(MODEL_PICK_KEY);
    }
  }, [models, model]);

  const pickModel = useCallback((id: string) => {
    setModel(id);
    if (id) localStorage.setItem(MODEL_PICK_KEY, id);
    else localStorage.removeItem(MODEL_PICK_KEY);
  }, []);

  const run = useCallback(() => {
    if (running) return;
    setViewedPast(null); // show the new run, not a pinned past one
    void startEvalRun(model ? { model } : undefined);
  }, [running, model]);

  const runRetrieval = useCallback(async () => {
    if (retRunning) return;
    setRetRunning(true);
    setRetError(null);
    setRetViewed(null);
    try {
      const report = await runRetrievalEval();
      if (!mounted.current) return;
      setRetHistory((h) => [report, ...h]);
    } catch (e) {
      if (mounted.current) setRetError(humanizeError(e));
    } finally {
      if (mounted.current) setRetRunning(false);
    }
  }, [retRunning]);
  const retReport = retViewed ?? (retHistory.length ? retHistory[0] : null);

  return (
    <div className="eval-panel">
      <div className="eval-head">
        <span className="eval-tasks-count">{taskCount} benchmark tasks</span>
        <div className="eval-head-actions">
          <select
            className="eval-model-select"
            aria-label="Benchmark model"
            value={model}
            disabled={running}
            onChange={(e) => pickModel(e.target.value)}
          >
            <option value="">Default model</option>
            {groupModelsBySource(models).map((g) => (
              <optgroup key={g.source} label={sourceMeta(g.source).label}>
                {g.models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.label}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
          <button className="eval-run-btn" disabled={running} onClick={run}>
            <Gauge size={14} strokeWidth={1.9} aria-hidden="true" />
            {running ? "Running…" : "Run benchmark"}
          </button>
        </div>
      </div>

      {progress && (
        <div className="eval-progress" role="status">
          <div className="eval-progress-bar">
            <div
              className="eval-progress-fill"
              style={{
                width: `${progress.total ? (progress.done / progress.total) * 100 : 0}%`,
              }}
            />
          </div>
          <span className="eval-progress-label">
            {progress.done}/{progress.total} tasks
          </span>
        </div>
      )}

      {error && <div className="eval-error">{error}</div>}

      {!loaded && !report && !progress && (
        <PanelLoading label="Loading benchmarks" />
      )}

      {loaded && loadError && !report && !progress && (
        <div className="eval-error" role="alert">
          Couldn't load eval data. {loadError}{" "}
          <button className="eval-history-row" onClick={() => void reload()}>
            Retry
          </button>
        </div>
      )}

      {report && (
        <div className="eval-report">
          <div className="eval-summary">
            <div className="eval-stat">
              <span className="eval-stat-val">
                {report.passed}/{report.total}
              </span>
              <span className="eval-stat-label">passed</span>
            </div>
            <div className="eval-stat">
              <span className="eval-stat-val">
                {Math.round(report.score_avg * 100)}%
              </span>
              <span className="eval-stat-label">avg score</span>
            </div>
            <div className="eval-stat">
              <span className="eval-stat-val eval-model">{report.model}</span>
              <span className="eval-stat-label">model</span>
            </div>
          </div>

          <ul className="eval-results">
            {report.results.map((r) => (
              <li key={r.id} className="eval-result">
                <span className={`eval-verdict ${r.passed ? "ok" : "fail"}`}>
                  {r.passed ? (
                    <Check size={13} strokeWidth={2.25} />
                  ) : (
                    <X size={13} strokeWidth={2.25} />
                  )}
                </span>
                <div className="eval-result-body">
                  <div className="eval-result-head">
                    <span className="eval-result-id">{r.id}</span>
                    <span className="eval-result-meta">
                      {Math.round(r.score * 100)}% · {r.latency_ms} ms
                    </span>
                  </div>
                  <details className="eval-result-detail">
                    <summary>{r.prompt}</summary>
                    <pre>{r.error ? `error: ${r.error}` : r.answer}</pre>
                    {r.missed.length > 0 && (
                      <p className="eval-missed">
                        missed: {r.missed.join(", ")}
                      </p>
                    )}
                  </details>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="eval-history">
        <div className="eval-head">
          <h3 className="eval-history-title">Retrieval quality</h3>
          <div className="eval-head-actions">
            <span className="eval-tasks-count">
              {retTaskCount} retrieval tasks
            </span>
            <button
              className="eval-run-btn"
              disabled={retRunning || retTaskCount === 0}
              title={
                retTaskCount === 0
                  ? "Create ~/.cortex/retrieval-eval-tasks.json (see fixtures/retrieval-eval-tasks.example.json)"
                  : "Embed each fixture query and check the expected sources rank in the top-k"
              }
              onClick={() => void runRetrieval()}
            >
              <Gauge size={14} strokeWidth={1.9} aria-hidden="true" />
              {retRunning ? "Running…" : "Run retrieval eval"}
            </button>
          </div>
        </div>
        {retError && <div className="eval-error">{retError}</div>}
        {retReport && (
          <div className="eval-report">
            <div className="eval-summary">
              <div className="eval-stat">
                <span className="eval-stat-val">
                  {retReport.passed}/{retReport.total}
                </span>
                <span className="eval-stat-label">passed</span>
              </div>
              <div className="eval-stat">
                <span className="eval-stat-val">
                  {Math.round(retReport.score_avg * 100)}%
                </span>
                <span className="eval-stat-label">
                  avg score @{retReport.k}
                </span>
              </div>
              <div className="eval-stat">
                <span className="eval-stat-val eval-model">
                  {retReport.embed_model}
                </span>
                <span className="eval-stat-label">embedder</span>
              </div>
            </div>
            <ul className="eval-results">
              {retReport.results.map((r) => (
                <li key={r.id} className="eval-result">
                  <span className={`eval-verdict ${r.passed ? "ok" : "fail"}`}>
                    {r.passed ? (
                      <Check size={13} strokeWidth={2.25} />
                    ) : (
                      <X size={13} strokeWidth={2.25} />
                    )}
                  </span>
                  <div className="eval-result-body">
                    <div className="eval-result-head">
                      <span className="eval-result-id">{r.id}</span>
                      <span className="eval-result-meta">
                        {Math.round(r.score * 100)}% · {r.latency_ms} ms
                        {r.stale_retrieved > 0
                          ? ` · ${r.stale_retrieved} stale`
                          : ""}
                      </span>
                    </div>
                    <details className="eval-result-detail">
                      <summary>{r.query}</summary>
                      <pre>
                        {r.retrieved.join("\n") || "(nothing retrieved)"}
                      </pre>
                      {r.missed.length > 0 && (
                        <p className="eval-missed">
                          missed: {r.missed.join(", ")}
                        </p>
                      )}
                    </details>
                  </div>
                </li>
              ))}
            </ul>
          </div>
        )}
        {retHistory.length > 1 && (
          <ul className="eval-history-list">
            {retHistory.map((h) => (
              <li key={h.run_id}>
                <button
                  className="eval-history-row"
                  onClick={() => setRetViewed(h)}
                >
                  <span className="eval-history-score">
                    {Math.round(h.score_avg * 100)}%
                  </span>
                  <span className="eval-history-meta">
                    {h.passed}/{h.total} · k={h.k} · {h.embed_model}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      {history.length > 1 && (
        <div className="eval-history">
          <h3 className="eval-history-title">Past runs</h3>
          <ul className="eval-history-list">
            {history.map((h) => (
              <li key={h.run_id}>
                <button
                  className="eval-history-row"
                  onClick={() => setViewedPast(h)}
                >
                  <span className="eval-history-score">
                    {Math.round(h.score_avg * 100)}%
                  </span>
                  <span className="eval-history-meta">
                    {h.passed}/{h.total} · {h.model}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
