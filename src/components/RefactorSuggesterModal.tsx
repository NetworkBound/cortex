import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  confidenceTier,
  suggestRefactors,
  type Refactor,
  type RefactorReport,
} from "@/lib/refactor-suggester";
import { useCortexStore } from "@/state/store";
import { pushToast } from "@/lib/toast";
import { Chevron } from "@/lib/chevron";

/**
 * AI refactor suggester modal. Renders as a self-mounting portal so the
 * slash command can summon it without App.tsx wiring — same pattern as
 * `SessionSummaryModal` / `TestRunnerPanel`.
 *
 * On mount we kick off `suggest_refactors` for the file. The intent input at
 * the top re-fires the analysis so the user can iteratively narrow the focus
 * ("testability" → "extract pure helpers"). "Apply" posts a system note with
 * the proposed diff into the chat scroll — the editor isn't auto-patched
 * because the user gets the final say on what lands.
 */

interface RefactorSuggesterModalProps {
  path: string;
  initialIntent?: string;
  onClose: () => void;
}

export function RefactorSuggesterModal({
  path,
  initialIntent,
  onClose,
}: RefactorSuggesterModalProps) {
  const [intent, setIntent] = useState<string>(initialIntent ?? "");
  const [pendingIntent, setPendingIntent] = useState<string>(initialIntent ?? "");
  const [report, setReport] = useState<RefactorReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<Set<number>>(new Set([0]));

  // Kick off the analysis on mount and whenever `pendingIntent` changes (i.e.
  // the user submits a new focus). We treat the empty string as "no focus".
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await suggestRefactors(path, pendingIntent);
        if (cancelled) return;
        setReport(out);
        setExpanded(new Set(out.refactors.length > 0 ? [0] : []));
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
  }, [path, pendingIntent]);

  // ESC closes the modal — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const toggleExpanded = useCallback((i: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  const onSubmitIntent = useCallback(
    (e: React.FormEvent) => {
      e.preventDefault();
      setPendingIntent(intent.trim());
    },
    [intent],
  );

  const onApply = useCallback(
    (r: Refactor) => {
      const note = [
        `🛠 **Refactor suggestion** — \`${path}\``,
        `**${r.name}** _(confidence ${(r.confidence * 100).toFixed(0)}%)_`,
        "",
        r.rationale,
        "",
        "**Before:**",
        "```",
        r.before_snippet,
        "```",
        "",
        "**After:**",
        "```",
        r.after_snippet,
        "```",
        "",
        "_Review and apply manually — the suggester does not auto-edit._",
      ].join("\n");
      useCortexStore.getState().appendMessage({
        id: `r-${crypto.randomUUID()}`,
        role: "system",
        content: note,
        tools: [],
      });
      pushToast({
        title: "Posted to chat",
        body: "Apply the suggested diff in the editor when ready.",
        kind: "success",
      });
    },
    [path],
  );

  const onCopy = useCallback(async (r: Refactor) => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(r, null, 2));
      pushToast({ title: "Copied", body: "Refactor JSON on clipboard.", kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, []);

  return (
    <div className="refactor-suggester-backdrop" onClick={onClose}>
      <div
        className="refactor-suggester-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="refactor-suggester-title"
      >
        <header className="refactor-suggester-header">
          <div>
            <h2 id="refactor-suggester-title">Refactor suggester</h2>
            <div className="refactor-suggester-path" title={path}>
              {path}
            </div>
          </div>
          <button
            className="refactor-suggester-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <form className="refactor-suggester-intent" onSubmit={onSubmitIntent}>
          <input
            type="text"
            placeholder="Optional focus (e.g. testability, minimise allocs)…"
            value={intent}
            onChange={(e) => setIntent(e.target.value)}
            disabled={loading}
            aria-label="Refactor focus"
          />
          <button type="submit" disabled={loading}>
            {loading ? "Analysing…" : "Re-analyse"}
          </button>
        </form>

        <div className="refactor-suggester-body">
          {loading && (
            <div className="refactor-suggester-loading">
              <span className="refactor-suggester-spinner" aria-hidden /> Generating refactors…
            </div>
          )}

          {error && !loading && (
            <div className="refactor-suggester-error">
              Failed to analyse this file.
              <pre>{error}</pre>
            </div>
          )}

          {report && !loading && report.refactors.length === 0 && (
            <div className="refactor-suggester-empty">
              AI returned no parseable refactors. Try narrowing the focus above.
            </div>
          )}

          {report && !loading && report.refactors.length > 0 && (
            <ul className="refactor-suggester-list">
              {report.refactors.map((r, i) => {
                const open = expanded.has(i);
                const tier = confidenceTier(r.confidence);
                return (
                  <li key={i} className="refactor-suggester-card" data-open={open}>
                    <button
                      type="button"
                      className="refactor-suggester-card-head"
                      onClick={() => toggleExpanded(i)}
                      aria-expanded={open}
                    >
                      <span className="refactor-suggester-card-title">{r.name}</span>
                      <span
                        className="refactor-suggester-pill"
                        data-tier={tier}
                        title={`Confidence ${(r.confidence * 100).toFixed(0)}%`}
                      >
                        {tier}
                      </span>
                      <span className="refactor-suggester-card-caret" aria-hidden>
                        <Chevron open={open} size={14} />
                      </span>
                    </button>
                    {open && (
                      <div className="refactor-suggester-card-body">
                        <p className="refactor-suggester-rationale">{r.rationale}</p>
                        <div className="refactor-suggester-diff">
                          <div className="refactor-suggester-diff-col">
                            <div className="refactor-suggester-diff-label">Before</div>
                            <pre>{r.before_snippet}</pre>
                          </div>
                          <div className="refactor-suggester-diff-col">
                            <div className="refactor-suggester-diff-label">After</div>
                            <pre>{r.after_snippet}</pre>
                          </div>
                        </div>
                        <div className="refactor-suggester-card-actions">
                          <button
                            type="button"
                            className="refactor-suggester-action"
                            onClick={() => onApply(r)}
                          >
                            Apply (post to chat)
                          </button>
                          <button
                            type="button"
                            className="refactor-suggester-action"
                            onClick={() => onCopy(r)}
                          >
                            Copy JSON
                          </button>
                        </div>
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <footer className="refactor-suggester-footer">
          <button className="refactor-suggester-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/refactor` slash command. Same detached-
 * root pattern as `SessionSummaryModal`.
 */
let activeRoot: Root | null = null;

export function openRefactorSuggesterModal(path: string, initialIntent?: string): void {
  if (activeRoot) return; // already open
  if (!path) {
    pushToast({
      title: "No file",
      body: "Open a file in the editor or pass a path: /refactor <path>.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "refactor-suggester";
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
    <RefactorSuggesterModal path={path} initialIntent={initialIntent} onClose={close} />,
  );
}
