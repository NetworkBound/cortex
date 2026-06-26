import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { summarizeSession, type SessionSummary } from "@/lib/session-summary";
import { pushToast } from "@/lib/toast";

/**
 * AI session summarizer modal. Renders as a self-mounting portal so the
 * slash command can summon it without App.tsx wiring — same pattern as
 * `IDEExportModal` / `KeyVaultPanel` / `BackupPanel`.
 *
 * On mount we kick off `summarize_session` for the current session id. The
 * loading spinner stays up while the gateway streams; once we have a summary the
 * user can save it to the Brain vault, copy it as markdown, or close.
 */

interface SessionSummaryModalProps {
  sessionId: string;
  onClose: () => void;
}

export function SessionSummaryModal({ sessionId, onClose }: SessionSummaryModalProps) {
  const [summary, setSummary] = useState<SessionSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);

  // Kick off the summary as soon as we mount. The gateway call lives behind a
  // 30s timeout in the backend, so the worst case is a `setError` toast.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await summarizeSession(sessionId, false);
        if (cancelled) return;
        setSummary(out);
        setSavedPath(out.saved_path);
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
  }, [sessionId]);

  // ESC closes the modal — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onSaveToBrain = useCallback(async () => {
    if (!summary || saving || savedPath) return;
    setSaving(true);
    try {
      // Re-invoke with save_to_brain=true to materialise the markdown file.
      // The backend is idempotent; calling it twice just overwrites.
      const out = await summarizeSession(summary.session_id, true);
      setSummary(out);
      setSavedPath(out.saved_path);
      pushToast({
        title: "Saved to Brain",
        body: out.saved_path ?? "summary saved",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setSaving(false);
    }
  }, [summary, saving, savedPath]);

  const onCopy = useCallback(async () => {
    if (!summary) return;
    const md = `# ${summary.headline}\n\n${summary.body}\n`;
    try {
      await navigator.clipboard.writeText(md);
      pushToast({ title: "Copied", body: "Summary copied as markdown.", kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [summary]);

  return (
    <div className="session-summary-backdrop" onClick={onClose}>
      <div
        className="session-summary-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="session-summary-title"
      >
        <header className="session-summary-header">
          <h2 id="session-summary-title">Session summary</h2>
          <button className="session-summary-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="session-summary-body">
          {loading && (
            <div className="session-summary-loading">
              <span className="session-summary-spinner" aria-hidden /> Summarizing…
            </div>
          )}

          {error && !loading && (
            <div className="session-summary-error">
              Failed to summarize this session.
              <pre>{error}</pre>
            </div>
          )}

          {summary && !loading && (
            <article className="session-summary-card">
              <h3 className="session-summary-headline">{summary.headline}</h3>
              <div className="session-summary-content">
                {summary.body.split("\n").map((line, i) => (
                  <div key={i} className="session-summary-line">
                    {line || " "}
                  </div>
                ))}
              </div>
              {savedPath && (
                <div className="session-summary-saved">
                  Saved to <code>{savedPath}</code>
                </div>
              )}
            </article>
          )}
        </div>

        <footer className="session-summary-footer">
          <button
            className="session-summary-secondary"
            onClick={onSaveToBrain}
            disabled={!summary || saving || !!savedPath}
          >
            {savedPath ? "Saved" : saving ? "Saving…" : "Save to brain"}
          </button>
          <button
            className="session-summary-secondary"
            onClick={onCopy}
            disabled={!summary}
          >
            Copy as markdown
          </button>
          <button className="session-summary-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/summary` slash command. Same detached-root
 * pattern as IDEExportModal / KeyVaultPanel — no App.tsx wiring needed.
 */
let activeRoot: Root | null = null;

export function openSessionSummaryModal(sessionId: string): void {
  if (activeRoot) return; // already open
  if (!sessionId) {
    pushToast({
      title: "No session",
      body: "Start chatting first — there's nothing to summarize.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "session-summary";
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
  root.render(<SessionSummaryModal sessionId={sessionId} onClose={close} />);
}
