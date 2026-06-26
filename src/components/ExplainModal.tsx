import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import {
  explainCode,
  saveExplanation,
  type ExplainAudience,
  type ExplainResult,
} from "@/lib/explain";
import { pushToast } from "@/lib/toast";

/**
 * AI explain-mode modal. Self-mounting portal so the `/explain` and `/why`
 * slash commands can summon it without touching App.tsx — same pattern as
 * `DocGenModal` / `RefactorSuggesterModal`.
 *
 * On mount we kick off `explain_code` for the file (and optional line range).
 * The audience radio + line-range inputs re-fire the call so the user can
 * dial in scope and depth without leaving the modal. Body renders the code
 * on the left (with line numbers, capped at the visible range) and the
 * markdown explanation on the right (react-markdown + GFM + highlight).
 *
 * Actions: copy the markdown to clipboard, or save it into the Cortex Brain
 * vault under `explanations/`.
 */

interface ExplainModalProps {
  path: string;
  initialLineStart: number | null;
  initialLineEnd: number | null;
  onClose: () => void;
}

const AUDIENCE_OPTIONS: { value: ExplainAudience; label: string }[] = [
  { value: "beginner", label: "Beginner" },
  { value: "intermediate", label: "Intermediate" },
  { value: "expert", label: "Expert" },
];

/**
 * Debounce a value so the line-range inputs don't fire a backend call on
 * every keystroke. 400ms feels snappy without thrashing the gateway.
 */
function useDebounced<T>(value: T, delay = 400): T {
  const [out, setOut] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setOut(value), delay);
    return () => window.clearTimeout(t);
  }, [value, delay]);
  return out;
}

export function ExplainModal({
  path,
  initialLineStart,
  initialLineEnd,
  onClose,
}: ExplainModalProps) {
  const [audience, setAudience] = useState<ExplainAudience>("beginner");
  // Inputs are strings so empty == "unset". We coerce to numbers when sending.
  const [startStr, setStartStr] = useState<string>(
    initialLineStart != null ? String(initialLineStart) : "",
  );
  const [endStr, setEndStr] = useState<string>(
    initialLineEnd != null ? String(initialLineEnd) : "",
  );
  const [result, setResult] = useState<ExplainResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const debouncedStart = useDebounced(startStr);
  const debouncedEnd = useDebounced(endStr);

  // Re-fires on path / audience / debounced line-range change.
  useEffect(() => {
    let cancelled = false;
    const ls = parseLine(debouncedStart);
    const le = parseLine(debouncedEnd);
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await explainCode({
          path,
          line_start: ls,
          line_end: le,
          audience,
        });
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
  }, [path, audience, debouncedStart, debouncedEnd]);

  // ESC closes — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onCopy = useCallback(async () => {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.markdown);
      pushToast({
        title: "Copied",
        body: "Explanation on clipboard.",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onSave = useCallback(async () => {
    if (!result) return;
    try {
      const saved = await saveExplanation({
        path: result.path,
        line_start: result.line_start,
        line_end: result.line_end,
        language: result.language,
        audience: result.audience,
        markdown: result.markdown,
      });
      pushToast({
        title: "Saved to Brain",
        body: saved.written_path,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  // Build the numbered code panel from the backend's clamped range.
  const codePanel = useMemo(() => {
    if (!result) return null;
    return (
      <CodePanel
        path={result.path}
        lineStart={result.line_start}
        lineEnd={result.line_end}
      />
    );
  }, [result]);

  return (
    <div className="explain-backdrop" onClick={onClose}>
      <div
        className="explain-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="explain-title"
      >
        <header className="explain-header">
          <div>
            <h2 id="explain-title">Explain code</h2>
            <div className="explain-path" title={path}>
              {path}
            </div>
          </div>
          <button className="explain-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="explain-toolbar">
          <div className="explain-audience" role="radiogroup" aria-label="Audience">
            {AUDIENCE_OPTIONS.map((opt) => (
              <label
                key={opt.value}
                className={`explain-audience-opt${
                  audience === opt.value ? " active" : ""
                }`}
              >
                <input
                  type="radio"
                  name="explain-audience"
                  value={opt.value}
                  checked={audience === opt.value}
                  onChange={() => setAudience(opt.value)}
                  disabled={loading}
                />
                {opt.label}
              </label>
            ))}
          </div>
          <div className="explain-range">
            <label>
              From line
              <input
                type="number"
                min={1}
                value={startStr}
                onChange={(e) => setStartStr(e.target.value)}
                aria-label="Line range start"
                placeholder="1"
              />
            </label>
            <label>
              to
              <input
                type="number"
                min={1}
                value={endStr}
                onChange={(e) => setEndStr(e.target.value)}
                aria-label="Line range end"
                placeholder="end"
              />
            </label>
          </div>
          {result && !loading && (
            <div className="explain-meta">
              <span title="Detected language">{result.language}</span>
              <span aria-hidden>·</span>
              <span title="Audience">{result.audience}</span>
            </div>
          )}
        </div>

        <div className="explain-body">
          <div className="explain-col explain-col-code">
            <div className="explain-col-label">
              Code
              {result?.line_start != null && result?.line_end != null && (
                <span className="explain-col-range">
                  {" "}
                  · L{result.line_start}-{result.line_end}
                </span>
              )}
            </div>
            <div className="explain-col-body">{codePanel}</div>
          </div>
          <div className="explain-col explain-col-explain">
            <div className="explain-col-label">Explanation</div>
            <div className="explain-col-body">
              {loading && (
                <div className="explain-loading">
                  <span className="explain-spinner" aria-hidden /> Generating
                  explanation…
                </div>
              )}
              {error && !loading && (
                <div className="explain-error">
                  Failed to generate explanation.
                  <pre>{error}</pre>
                </div>
              )}
              {result && !loading && (
                <div className="explain-markdown">
                  <ReactMarkdown
                    remarkPlugins={[remarkGfm]}
                    rehypePlugins={[rehypeHighlight]}
                  >
                    {result.markdown}
                  </ReactMarkdown>
                </div>
              )}
            </div>
          </div>
        </div>

        <footer className="explain-footer">
          <button
            className="explain-action"
            onClick={onCopy}
            disabled={!result || loading}
          >
            Copy explanation
          </button>
          <button
            className="explain-action"
            onClick={onSave}
            disabled={!result || loading}
          >
            Save to memory
          </button>
          <button className="explain-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/** Parse `"12"` → `12`; empty / invalid → `null`. Clamped to `>= 1`. */
function parseLine(raw: string): number | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const n = Number.parseInt(trimmed, 10);
  if (!Number.isFinite(n) || n < 1) return null;
  return n;
}

/**
 * Read the file contents via the Tauri FS plugin and render the visible
 * range with line-number gutters. Loading lives inside the component so the
 * markdown panel can render before the file body has streamed off disk —
 * this keeps the UX snappy when the file is large.
 */
function CodePanel({
  path,
  lineStart,
  lineEnd,
}: {
  path: string;
  lineStart: number | null;
  lineEnd: number | null;
}) {
  const [body, setBody] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const lastPath = useRef<string>("");

  useEffect(() => {
    let cancelled = false;
    if (lastPath.current === path && body !== null) return; // already cached
    lastPath.current = path;
    (async () => {
      try {
        // Tauri 2 FS plugin — read text. Keep the import dynamic so the
        // explain modal stays out of the main bundle.
        const { readTextFile } = await import("@tauri-apps/plugin-fs");
        const text = await readTextFile(path);
        if (cancelled) return;
        setBody(text);
      } catch (e) {
        if (cancelled) return;
        setErr(humanizeError(e));
      }
    })();
    return () => {
      cancelled = true;
    };
    // We deliberately watch `path` only — line range changes don't need a
    // re-read since we slice in memory below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  if (err) {
    return <pre className="explain-code-err">read failed: {err}</pre>;
  }
  if (body === null) {
    return <pre className="explain-code-err">loading…</pre>;
  }

  const lines = body.split("\n");
  const max = lines.length;
  const s = lineStart != null ? Math.max(1, Math.min(lineStart, max)) : 1;
  const e = lineEnd != null ? Math.max(s, Math.min(lineEnd, max)) : max;
  const slice = lines.slice(s - 1, e);
  const gutterWidth = String(e).length;

  return (
    <pre className="explain-code">
      {slice.map((line, idx) => {
        const lineNo = s + idx;
        return (
          <span className="explain-code-line" key={lineNo}>
            <span
              className="explain-code-gutter"
              style={{ width: `${gutterWidth}ch` }}
            >
              {lineNo}
            </span>
            <span className="explain-code-text">{line || " "}</span>
          </span>
        );
      })}
    </pre>
  );
}

/**
 * Imperative summoner used by the `/explain` and `/why` slash commands.
 * Same detached-root pattern as `DocGenModal` / `RefactorSuggesterModal`.
 */
let activeRoot: Root | null = null;

export function openExplainModal(
  path: string,
  lineStart: number | null = null,
  lineEnd: number | null = null,
): void {
  if (activeRoot) return; // already open
  if (!path) {
    pushToast({
      title: "No file",
      body: "Open a file in the editor or pass a path: /explain <path>.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "explain";
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
    <ExplainModal
      path={path}
      initialLineStart={lineStart}
      initialLineEnd={lineEnd}
      onClose={close}
    />,
  );
}
