import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { generateChangelog, type ChangelogResult } from "@/lib/changelog";
import { saveFileText } from "@/lib/editor-save";
import { pushToast } from "@/lib/toast";
import { confirmDialog } from "@/lib/dialogs";
import { MarkdownView } from "@/components/MarkdownView";
import { useCortexStore } from "@/state/store";

/**
 * AI changelog generator modal. Self-mounting portal so the `/changelog`
 * slash command can summon it without touching App.tsx — same pattern as
 * DocGenModal / RefactorSuggesterModal.
 *
 * The user picks a `since` (free-form `git log --since=` syntax) and an
 * optional `until` (ditto). Hitting Generate calls the backend, which
 * returns a Keep-a-Changelog markdown doc that we render via MarkdownView.
 * Actions:
 *   - Copy as markdown → clipboard
 *   - Save to CHANGELOG.md → writes `<projectRoot>/CHANGELOG.md` via
 *     `save_file_text` (the same command the editor pane uses).
 *
 * `until` is a UI affordance: the backend currently only consumes `since`,
 * so when both are set we encode `until` as a trailing `..<until>` so the
 * model still sees it. The user can also just type plain `git log --since`
 * syntax into the since box ("yesterday", "2026-01-01", "2 weeks ago").
 */

interface ChangelogModalProps {
  initialSince: string;
  onClose: () => void;
}

export function ChangelogModal({ initialSince, onClose }: ChangelogModalProps) {
  const project = useCortexStore((s) => s.activeProject);
  const [since, setSince] = useState(initialSince || "2 weeks ago");
  const [until, setUntil] = useState("");
  const [result, setResult] = useState<ChangelogResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // ESC closes — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onGenerate = useCallback(async () => {
    if (!project) {
      setError("No active project — pick one from the sidebar first.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      // When the user provides an `until`, fold it into the since arg so the
      // backend still sees it (the gateway prompt walks the whole string).
      const sinceArg = until.trim()
        ? `${since.trim() || "2 weeks ago"} until=${until.trim()}`
        : since.trim() || "2 weeks ago";
      const out = await generateChangelog(String(project.root), sinceArg);
      setResult(out);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, [project, since, until]);

  const onCopy = useCallback(async () => {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.markdown);
      pushToast({
        title: "Copied",
        body: "Changelog copied as markdown.",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onSave = useCallback(async () => {
    if (!result || !project) return;
    // Resolve `<projectRoot>/CHANGELOG.md` as a forward-slash path. The
    // backend tolerates either separator; we pick `/` to stay consistent
    // with the editor pane's saved paths.
    const root = String(project.root).replace(/\\/g, "/").replace(/\/$/, "");
    const target = `${root}/CHANGELOG.md`;
    const ok = await confirmDialog({
      title: "Save changelog",
      message: `Write the changelog to ${target}?\n\nIf the file already exists it will be overwritten.`,
      confirmLabel: "Overwrite",
      danger: true,
    });
    if (!ok) return;
    try {
      const written = await saveFileText(target, result.markdown);
      pushToast({ title: "Saved", body: written, kind: "success" });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    }
  }, [result, project]);

  return (
    <div className="changelog-backdrop" onClick={onClose}>
      <div
        className="changelog-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="changelog-title"
      >
        <header className="changelog-header">
          <div>
            <h2 id="changelog-title">Changelog generator</h2>
            <div className="changelog-path" title={String(project?.root ?? "")}>
              {project ? project.name : "no active project"}
            </div>
          </div>
          <button className="changelog-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="changelog-toolbar">
          <label className="changelog-field">
            <span>Since</span>
            <input
              type="text"
              value={since}
              onChange={(e) => setSince(e.target.value)}
              placeholder="2 weeks ago"
              disabled={loading}
            />
          </label>
          <label className="changelog-field">
            <span>Until (optional)</span>
            <input
              type="text"
              value={until}
              onChange={(e) => setUntil(e.target.value)}
              placeholder="now"
              disabled={loading}
            />
          </label>
          <button
            className="changelog-primary"
            onClick={() => void onGenerate()}
            disabled={loading || !project}
          >
            {loading ? "Generating…" : "Generate"}
          </button>
        </div>

        <div className="changelog-body">
          {!result && !loading && !error && (
            <div className="changelog-empty">
              Pick a date range and hit <strong>Generate</strong>. The
              changelog is grouped by Added / Changed / Fixed / Deprecated /
              Removed / Security.
            </div>
          )}

          {loading && (
            <div className="changelog-loading">
              <span className="changelog-spinner" aria-hidden /> Generating
              changelog from recent commits…
            </div>
          )}

          {error && !loading && (
            <div className="changelog-error">
              Failed to generate changelog.
              <pre>{error}</pre>
            </div>
          )}

          {result && !loading && (
            <div className="changelog-result">
              <div className="changelog-meta">
                <span>{result.commit_count} commits</span>
                <span aria-hidden>·</span>
                <span>since {result.since}</span>
              </div>
              <div className="changelog-markdown">
                <MarkdownView source={result.markdown} />
              </div>
            </div>
          )}
        </div>

        <footer className="changelog-footer">
          <button
            className="changelog-action"
            onClick={onCopy}
            disabled={!result || loading}
          >
            Copy as markdown
          </button>
          <button
            className="changelog-action"
            onClick={onSave}
            disabled={!result || loading || !project}
          >
            Save to CHANGELOG.md
          </button>
          <button className="changelog-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/changelog` slash command. Same detached-
 * root pattern as `DocGenModal`.
 */
let activeRoot: Root | null = null;

export function openChangelogModal(initialSince?: string): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "changelog";
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
    <ChangelogModal initialSince={initialSince ?? ""} onClose={close} />,
  );
}
