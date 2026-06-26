import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { EDITOR_OPEN_EVENT } from "@/lib/editor";
import { Chevron } from "@/lib/chevron";
import {
  parseLocation,
  runTests,
  statusLabel,
  type TestFailure,
  type TestResult,
} from "@/lib/test-runner";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Inline test runner modal — same self-mounting portal pattern as
 * `DedupePanel` / `IDEExportModal` so `/test` can summon it without App.tsx
 * wiring. Renders the detected framework + command, a Run button, colored
 * passed/failed/skipped pills, and an expandable failure list with
 * "Open file" links wired to the editor pane.
 */

interface TestRunnerPanelProps {
  initialFramework?: string;
  onClose: () => void;
}

function openInEditor(path: string): void {
  try {
    window.dispatchEvent(
      new CustomEvent(EDITOR_OPEN_EVENT, { detail: { path } }),
    );
  } catch {
    /* not in a browser-like env — best-effort */
  }
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(2)} s`;
  const m = Math.floor(s / 60);
  return `${m}m ${(s - m * 60).toFixed(0)}s`;
}

export function TestRunnerPanel({
  initialFramework,
  onClose,
}: TestRunnerPanelProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<TestResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [framework, setFramework] = useState<string>(initialFramework ?? "");
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [showOutput, setShowOutput] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onRun = useCallback(async () => {
    if (!activeProject) {
      setError("No active project — pick one from the sidebar first.");
      return;
    }
    setRunning(true);
    setError(null);
    setResult(null);
    setExpanded(new Set());
    try {
      const r = await runTests(activeProject.root, framework || undefined);
      setResult(r);
      const kind: "success" | "warning" | "info" =
        r.failed > 0 ? "warning" : r.exit_code !== 0 ? "warning" : "success";
      pushToast({
        title: `Tests ${statusLabel(r, false)}`,
        body: `${r.passed} passed · ${r.failed} failed · ${r.skipped} skipped (${formatDuration(r.duration_ms)})`,
        kind,
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setRunning(false);
    }
  }, [activeProject, framework]);

  // Kick off automatically when the panel opens with an explicit project.
  useEffect(() => {
    if (activeProject && !result && !error && !running) {
      void onRun();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const toggleExpanded = (idx: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) next.delete(idx);
      else next.add(idx);
      return next;
    });
  };

  const status = statusLabel(result, running);
  const headerClass = `test-runner-status test-runner-status-${status}`;

  return (
    <div className="test-runner-backdrop" onMouseDown={onClose}>
      <div
        className="test-runner-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="test-runner-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="test-runner-header">
          <h2 id="test-runner-title">Test runner</h2>
          <button
            className="test-runner-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="test-runner-controls">
          <div className="test-runner-detect">
            <span className="test-runner-label">Framework</span>
            <code className="test-runner-fw">
              {result?.framework ?? framework ?? "auto-detect"}
            </code>
            {result?.command && (
              <code className="test-runner-cmd" title={result.command}>
                $ {result.command}
              </code>
            )}
          </div>
          <div className="test-runner-actions">
            <select
              className="test-runner-select"
              value={framework}
              onChange={(e) => setFramework(e.target.value)}
              disabled={running}
              aria-label="Test framework override"
            >
              <option value="">auto-detect</option>
              <option value="cargo">cargo</option>
              <option value="vitest">vitest</option>
              <option value="jest">jest</option>
              <option value="mocha">mocha</option>
              <option value="pytest">pytest</option>
            </select>
            <button
              className="test-runner-primary"
              onClick={onRun}
              disabled={running || !activeProject}
            >
              {running ? "Running…" : "Run"}
            </button>
          </div>
        </div>

        <div className={headerClass}>
          <span className="test-runner-pill test-runner-pill-status">
            {status}
          </span>
          {result && (
            <>
              <span className="test-runner-pill test-runner-pill-pass">
                {result.passed} passed
              </span>
              <span className="test-runner-pill test-runner-pill-fail">
                {result.failed} failed
              </span>
              <span className="test-runner-pill test-runner-pill-skip">
                {result.skipped} skipped
              </span>
              <span className="test-runner-pill test-runner-pill-duration">
                {formatDuration(result.duration_ms)}
              </span>
              <span
                className="test-runner-pill test-runner-pill-exit"
                title={`exit code ${result.exit_code}`}
              >
                exit {result.exit_code}
              </span>
            </>
          )}
        </div>

        {error && <div className="test-runner-error">{error}</div>}

        {result && result.failures.length === 0 && result.failed === 0 && (
          <div className="test-runner-empty">
            {result.passed > 0
              ? "All green — no failures reported."
              : "No tests ran. Check the output tail below."}
          </div>
        )}

        {result && result.failures.length > 0 && (
          <ul className="test-runner-failures">
            {result.failures.map((f, idx) => (
              <FailureRow
                key={`${f.name}-${idx}`}
                failure={f}
                expanded={expanded.has(idx)}
                onToggle={() => toggleExpanded(idx)}
              />
            ))}
          </ul>
        )}

        {result && (
          <details
            className="test-runner-output"
            open={showOutput}
            onToggle={(e) => setShowOutput((e.target as HTMLDetailsElement).open)}
          >
            <summary>
              {showOutput ? "Hide" : "Show"} full output (stdout +{" "}
              {result.stderr_tail.length > 0 ? "stderr" : "no stderr"})
            </summary>
            <div className="test-runner-stream">
              <h3>stdout</h3>
              <pre>{result.stdout_tail || "(empty)"}</pre>
              {result.stderr_tail.length > 0 && (
                <>
                  <h3>stderr</h3>
                  <pre>{result.stderr_tail}</pre>
                </>
              )}
            </div>
          </details>
        )}

        <footer className="test-runner-footer">
          <button
            className="test-runner-secondary"
            onClick={onClose}
            disabled={running}
          >
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

interface FailureRowProps {
  failure: TestFailure;
  expanded: boolean;
  onToggle: () => void;
}

function FailureRow({ failure, expanded, onToggle }: FailureRowProps) {
  const parsed = parseLocation(failure.location);
  return (
    <li className="test-runner-failure">
      <button
        className="test-runner-failure-head"
        onClick={onToggle}
        aria-expanded={expanded}
      >
        <span className="test-runner-failure-caret"><Chevron open={expanded} size={13} /></span>
        <span className="test-runner-failure-name">{failure.name}</span>
        {failure.location && (
          <code className="test-runner-failure-loc">{failure.location}</code>
        )}
      </button>
      {expanded && (
        <div className="test-runner-failure-body">
          {failure.message && (
            <pre className="test-runner-failure-msg">{failure.message}</pre>
          )}
          {parsed && (
            <button
              className="test-runner-open"
              onClick={() => openInEditor(parsed.path)}
            >
              Open file
            </button>
          )}
        </div>
      )}
    </li>
  );
}

/** Imperative summoner — mirrors `openDedupePanel`. */
let activeRoot: Root | null = null;

export function openTestRunnerPanel(framework?: string): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "test-runner";
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
  root.render(
    <TestRunnerPanel initialFramework={framework} onClose={close} />,
  );
}
