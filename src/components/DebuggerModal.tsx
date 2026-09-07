import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  confidenceTier,
  debugError,
  DEBUG_SOURCE_LABELS,
  type DebugResult,
  type DebugSource,
} from "@/lib/ai-debugger";
import { openInEditor } from "@/lib/editor";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * AI debugger modal. Same self-mounting portal pattern as
 * `RefactorSuggesterModal` / `ExplainModal` — the `/fix` (alias `/debug`)
 * slash command summons it without any App.tsx wiring.
 *
 * The user picks an error source (defaults to whichever the slash passed in),
 * optionally pastes a manual error / stack, then clicks Analyse. The backend
 * pulls the error, finds the source line, asks the gateway for a JSON-shaped
 * suggestion, and we render the four pieces (root cause, suggested fix,
 * patch, confidence pill) plus three actions: Apply (posts a system note to
 * the chat — only available at ≥0.6 confidence so we don't nudge users to
 * apply low-confidence patches), Copy patch, and Open file at error location.
 */

interface DebuggerModalProps {
  initialSource: DebugSource;
  initialErrorText?: string;
  initialErrorStack?: string;
  onClose: () => void;
}

const ALL_SOURCES: DebugSource[] = [
  "recent_crash",
  "recent_issue",
  "last_test_failure",
  "chat_error",
  "manual",
];

export function DebuggerModal({
  initialSource,
  initialErrorText,
  initialErrorStack,
  onClose,
}: DebuggerModalProps) {
  const [source, setSource] = useState<DebugSource>(initialSource);
  const [manualText, setManualText] = useState<string>(initialErrorText ?? "");
  const [manualStack, setManualStack] = useState<string>(initialErrorStack ?? "");
  const [result, setResult] = useState<DebugResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const activeProject = useCortexStore((s) => s.activeProject);
  const projectRoot = activeProject?.root ?? "";

  const showsManualPaste = source === "manual" || source === "chat_error";

  // Auto-fire when the slash command pre-populated `chat_error` text — we
  // know the user's intent already, no need to make them click Analyse.
  useEffect(() => {
    if (initialSource === "chat_error" && initialErrorText) {
      void runAnalysis(initialSource, initialErrorText, initialErrorStack ?? "");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ESC closes — matches every other portal modal in the app.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const runAnalysis = useCallback(
    async (src: DebugSource, text: string, stack: string) => {
      setLoading(true);
      setError(null);
      setResult(null);
      try {
        const out = await debugError({
          projectRoot,
          errorSource: src,
          errorText: text || null,
          errorStack: stack || null,
        });
        setResult(out);
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setLoading(false);
      }
    },
    [projectRoot],
  );

  const onAnalyse = useCallback(() => {
    void runAnalysis(source, manualText.trim(), manualStack.trim());
  }, [runAnalysis, source, manualText, manualStack]);

  const tier = useMemo(
    () => (result ? confidenceTier(result.confidence) : null),
    [result],
  );

  const applyDisabled = !result || result.confidence < 0.6 || !result.code_patch.trim();

  const onApply = useCallback(() => {
    if (!result) return;
    const lines = [
      `🛠 **AI debugger suggestion** — ${result.error_summary}`,
      "",
      `**Root cause:** ${result.root_cause}`,
      `**Suggested fix:** ${result.suggested_fix}`,
      "",
      "**Patch (review before applying):**",
      "```diff",
      result.code_patch.trim() || "(no patch returned)",
      "```",
      "",
      `_Confidence ${(result.confidence * 100).toFixed(0)}% — the debugger does not auto-edit; review and apply manually._`,
    ];
    useCortexStore.getState().appendMessage({
      id: `d-${crypto.randomUUID()}`,
      role: "system",
      content: lines.join("\n"),
      tools: [],
    });
    pushToast({
      title: "Posted to chat",
      body: "Review the proposed patch in the editor.",
      kind: "success",
    });
  }, [result]);

  const onCopyPatch = useCallback(async () => {
    if (!result?.code_patch) {
      pushToast({ title: "Nothing to copy", body: "Patch is empty.", kind: "warning" });
      return;
    }
    try {
      await navigator.clipboard.writeText(result.code_patch);
      pushToast({ title: "Patch copied", body: "Unified diff on clipboard.", kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onOpenSource = useCallback(() => {
    if (!result?.source_path) {
      pushToast({
        title: "No source location",
        body: "The debugger couldn't extract a path:line from the error.",
        kind: "warning",
      });
      return;
    }
    openInEditor(result.source_path);
    onClose();
  }, [result, onClose]);

  return (
    <div className="ai-debugger-backdrop" onClick={onClose}>
      <div
        className="ai-debugger-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="ai-debugger-title"
      >
        <header className="ai-debugger-header">
          <div>
            <h2 id="ai-debugger-title">AI debugger</h2>
            <div className="ai-debugger-sub">
              Suggest a fix from the chosen error source.
            </div>
          </div>
          <button className="ai-debugger-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="ai-debugger-controls">
          <label className="ai-debugger-source-label">
            <span>Error source</span>
            <select
              value={source}
              onChange={(e) => setSource(e.target.value as DebugSource)}
              disabled={loading}
            >
              {ALL_SOURCES.map((s) => (
                <option key={s} value={s}>
                  {DEBUG_SOURCE_LABELS[s]}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="ai-debugger-analyse"
            onClick={onAnalyse}
            disabled={loading || (showsManualPaste && !manualText.trim())}
          >
            {loading ? "Analysing…" : "Analyse"}
          </button>
        </div>

        {showsManualPaste && (
          <div className="ai-debugger-paste">
            <label>
              <span>Error message</span>
              <textarea
                value={manualText}
                onChange={(e) => setManualText(e.target.value)}
                placeholder="Paste the error message here…"
                rows={4}
                disabled={loading}
              />
            </label>
            <label>
              <span>Stack trace (optional)</span>
              <textarea
                value={manualStack}
                onChange={(e) => setManualStack(e.target.value)}
                placeholder="at MyFn (src/foo.ts:42:7)…"
                rows={3}
                disabled={loading}
              />
            </label>
          </div>
        )}

        <div className="ai-debugger-body">
          {loading && (
            <div className="ai-debugger-loading">
              <span className="ai-debugger-spinner" aria-hidden /> Diagnosing…
            </div>
          )}

          {error && !loading && (
            <div className="ai-debugger-error">
              Failed to analyse.
              <pre>{error}</pre>
            </div>
          )}

          {result && !loading && (
            <div className="ai-debugger-result">
              <div className="ai-debugger-summary">
                <span
                  className="ai-debugger-pill"
                  data-tier={tier ?? "low"}
                  title={`Confidence ${(result.confidence * 100).toFixed(0)}%`}
                >
                  {tier} · {(result.confidence * 100).toFixed(0)}%
                </span>
                <span className="ai-debugger-summary-text" title={result.error_summary}>
                  {result.error_summary}
                </span>
                {result.source_path && (
                  <span className="ai-debugger-location" title={result.source_path}>
                    {shortenPath(result.source_path)}
                    {result.source_line ? `:${result.source_line}` : ""}
                  </span>
                )}
              </div>

              <section className="ai-debugger-section">
                <h3>Root cause</h3>
                <p>{result.root_cause || "(model returned no root cause)"}</p>
              </section>

              <section className="ai-debugger-section">
                <h3>Suggested fix</h3>
                <p>{result.suggested_fix || "(model returned no suggested fix)"}</p>
              </section>

              <section className="ai-debugger-section">
                <h3>Code patch</h3>
                {result.code_patch.trim() ? (
                  <pre className="ai-debugger-patch">{result.code_patch}</pre>
                ) : (
                  <p className="ai-debugger-patch-empty">
                    Model returned no patch — the suggested fix above is your starting point.
                  </p>
                )}
              </section>

              <div className="ai-debugger-actions">
                <button
                  type="button"
                  className="ai-debugger-action"
                  onClick={onApply}
                  disabled={applyDisabled}
                  title={
                    applyDisabled
                      ? "Apply is gated at ≥ 0.6 confidence with a non-empty patch"
                      : "Post the patch as a system note in chat"
                  }
                >
                  Apply patch
                </button>
                <button
                  type="button"
                  className="ai-debugger-action"
                  onClick={onCopyPatch}
                  disabled={!result.code_patch.trim()}
                >
                  Copy patch
                </button>
                <button
                  type="button"
                  className="ai-debugger-action"
                  onClick={onOpenSource}
                  disabled={!result.source_path}
                >
                  Open file at error location
                </button>
              </div>
            </div>
          )}

          {!loading && !result && !error && (
            <div className="ai-debugger-empty">
              Pick a source above and click Analyse to start.
            </div>
          )}
        </div>

        <footer className="ai-debugger-footer">
          <button className="ai-debugger-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/** Shorten an absolute path to the last two segments — keeps the header
 *  scannable without hiding which file the model picked. */
function shortenPath(p: string): string {
  const parts = p.split(/[/\\]/).filter(Boolean);
  if (parts.length <= 2) return p;
  return ".../" + parts.slice(-2).join("/");
}

let activeRoot: Root | null = null;

/** Imperative summoner used by the `/fix` slash command. Returns silently
 *  when a modal is already mounted so back-to-back slashes don't double-open. */
export function openDebuggerModal(
  initialSource: DebugSource = "recent_crash",
  opts?: { errorText?: string; errorStack?: string },
): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "ai-debugger";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(
    <DebuggerModal
      initialSource={initialSource}
      initialErrorText={opts?.errorText}
      initialErrorStack={opts?.errorStack}
      onClose={close}
    />,
  );
}
