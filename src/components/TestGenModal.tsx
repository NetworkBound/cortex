import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { generateTests, type TestFramework, type TestGenResult } from "@/lib/test-gen";
import { saveFileText } from "@/lib/editor-save";
import { runTests } from "@/lib/test-runner";
import { pushToast } from "@/lib/toast";
import { promptDialog } from "@/lib/dialogs";

/**
 * AI test generator modal. Renders as a self-mounting portal so the
 * `/gentest` slash command can summon it without touching App.tsx — same
 * pattern as `DocGenModal` / `RefactorSuggesterModal`.
 *
 * On mount we call `generate_tests(path, function_name)`. The framework
 * dropdown + function-name input both re-fire the call so the user can
 * swap targets without remounting. Actions:
 *   - Copy → clipboard
 *   - Save to suggested path → `save_file_text(result.suggested_test_path, result.test_code)`
 *   - Run tests → invokes the existing `run_tests` command and reports the
 *     summary as a toast.
 */

interface TestGenModalProps {
  path: string;
  initialFunctionName?: string | null;
  onClose: () => void;
}

const FRAMEWORK_OPTIONS: { value: TestFramework; label: string }[] = [
  { value: "auto", label: "Auto (by extension)" },
  { value: "vitest", label: "Vitest (TS/JS)" },
  { value: "jest", label: "Jest (TS/JS)" },
  { value: "mocha", label: "Mocha (TS/JS)" },
  { value: "cargo", label: "Cargo (Rust)" },
  { value: "pytest", label: "Pytest (Python)" },
];

export function TestGenModal({
  path,
  initialFunctionName,
  onClose,
}: TestGenModalProps) {
  const [framework, setFramework] = useState<TestFramework>("auto");
  const [functionName, setFunctionName] = useState<string>(
    initialFunctionName ?? "",
  );
  // Buffered text input so we don't re-fire the AI on every keystroke.
  const [functionInput, setFunctionInput] = useState<string>(
    initialFunctionName ?? "",
  );
  const [result, setResult] = useState<TestGenResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);

  // Kick off generation on mount and whenever framework / committed function
  // name changes. The text input commits on Enter / blur — see below.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await generateTests(path, functionName || null, framework);
        if (cancelled) return;
        setResult(out);
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
  }, [path, functionName, framework]);

  // ESC closes — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const commitFunctionInput = useCallback(() => {
    const cleaned = functionInput.trim();
    if (cleaned !== functionName) setFunctionName(cleaned);
  }, [functionInput, functionName]);

  const onCopy = useCallback(async () => {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.test_code);
      pushToast({
        title: "Copied",
        body: "Test code on clipboard.",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onSaveSuggested = useCallback(async () => {
    if (!result) return;
    const target = await promptDialog({
      title: "Save generated tests",
      message: "Save generated tests to:",
      initialValue: result.suggested_test_path,
    });
    if (!target || !target.trim()) return;
    try {
      const written = await saveFileText(target.trim(), result.test_code);
      pushToast({ title: "Saved", body: written, kind: "success" });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onRunTests = useCallback(async () => {
    if (!result) return;
    // Derive a plausible project root: the directory containing the suggested
    // test path is usually the crate/project root for cargo/pytest, and the
    // parent of `__tests__/` for vitest/jest. The backend re-detects anyway.
    const sep = Math.max(
      result.suggested_test_path.lastIndexOf("/"),
      result.suggested_test_path.lastIndexOf("\\"),
    );
    const projectRoot =
      sep > 0
        ? result.suggested_test_path.slice(0, sep)
        : sep === 0
          ? result.suggested_test_path.slice(0, 1) // separator at root, e.g. "/foo" -> "/"
          : "."; // no separator: bare filename, fall back to current directory
    setRunning(true);
    try {
      const report = await runTests(projectRoot, result.framework);
      const kind =
        report.failed === 0 && report.exit_code === 0
          ? "success"
          : report.failed > 0
            ? "error"
            : "warning";
      pushToast({
        title: `Tests: ${report.passed} passed, ${report.failed} failed`,
        body: `${report.framework} · exit ${report.exit_code} · ${report.duration_ms}ms`,
        kind,
      });
    } catch (e) {
      pushToast({ title: "Run tests failed", body: humanizeError(e), kind: "error" });
    } finally {
      setRunning(false);
    }
  }, [result]);

  return (
    <div className="testgen-backdrop" onClick={onClose}>
      <div
        className="testgen-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="testgen-title"
      >
        <header className="testgen-header">
          <div>
            <h2 id="testgen-title">Test generator</h2>
            <div className="testgen-path" title={path}>
              {path}
            </div>
          </div>
          <button
            className="testgen-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="testgen-toolbar">
          <label className="testgen-field">
            Framework:
            <select
              value={framework}
              onChange={(e) => setFramework(e.target.value as TestFramework)}
              disabled={loading}
              aria-label="Test framework"
            >
              {FRAMEWORK_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </label>
          <label className="testgen-field">
            Function:
            <input
              type="text"
              placeholder="(whole file)"
              value={functionInput}
              onChange={(e) => setFunctionInput(e.target.value)}
              onBlur={commitFunctionInput}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commitFunctionInput();
                }
              }}
              disabled={loading}
              aria-label="Function name"
            />
          </label>
          {result && !loading && (
            <div className="testgen-meta">
              <span title="Detected language">{result.language}</span>
              <span aria-hidden>·</span>
              <span title="Resolved framework">{result.framework}</span>
            </div>
          )}
        </div>

        <div className="testgen-body">
          {loading && (
            <div className="testgen-loading">
              <span className="testgen-spinner" aria-hidden /> Generating tests…
            </div>
          )}

          {error && !loading && (
            <div className="testgen-error">
              Failed to generate tests.
              <pre>{error}</pre>
            </div>
          )}

          {result && !loading && (
            <>
              <div className="testgen-suggested">
                Suggested path:{" "}
                <code title={result.suggested_test_path}>
                  {result.suggested_test_path}
                </code>
              </div>
              <pre className="testgen-code">{result.test_code}</pre>
            </>
          )}
        </div>

        <footer className="testgen-footer">
          <button
            className="testgen-action"
            onClick={onCopy}
            disabled={!result || loading}
          >
            Copy
          </button>
          <button
            className="testgen-action"
            onClick={onSaveSuggested}
            disabled={!result || loading}
          >
            Save to suggested path
          </button>
          <button
            className="testgen-action"
            onClick={onRunTests}
            disabled={!result || loading || running}
          >
            {running ? "Running…" : "Run tests"}
          </button>
          <button className="testgen-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/gentest` slash command. Same detached-
 * root pattern as `DocGenModal`.
 */
let activeRoot: Root | null = null;

export function openTestGenModal(
  path: string,
  functionName?: string | null,
): void {
  if (activeRoot) return; // already open
  if (!path) {
    pushToast({
      title: "No file",
      body: "Open a file in the editor or pass a path: /gentest <path>.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "testgen";
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
    <TestGenModal
      path={path}
      initialFunctionName={functionName ?? null}
      onClose={close}
    />,
  );
}
