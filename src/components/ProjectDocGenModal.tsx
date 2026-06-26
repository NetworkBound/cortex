import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  generateProjectDoc,
  type ProjectDocResult,
  type ProjectDocType,
} from "@/lib/project-doc-gen";
import { saveFileText } from "@/lib/editor-save";
import { pushToast } from "@/lib/toast";
import { confirmDialog } from "@/lib/dialogs";
import { MarkdownView } from "@/components/MarkdownView";
import { useCortexStore } from "@/state/store";

/**
 * AI project-doc generator modal (README.md / CLAUDE.md / CONTRIBUTING.md).
 *
 * Self-mounting portal so the `/readme` and `/claude-md` slash commands can
 * summon it without touching App.tsx — same pattern as ChangelogModal /
 * DocGenModal. On mount we kick off `generate_project_doc` for the requested
 * doc_type. The radio at the top re-fires the call so the user can swap
 * between readme / claude-md / contributing in one place.
 *
 * The body has a "Preview / Source" toggle: preview renders the generated
 * markdown via MarkdownView, source shows the raw text in a `<pre>` so the
 * user can verify what'll actually land on disk before clicking Save.
 *
 * Actions:
 *   - Copy → clipboard
 *   - Save to suggested path → writes `<suggested_path>` via the existing
 *     `save_file_text` Tauri command (same command the editor pane uses).
 */

const DOC_TYPES: { value: ProjectDocType; label: string }[] = [
  { value: "readme", label: "README.md" },
  { value: "claude-md", label: "CLAUDE.md" },
  { value: "contributing", label: "CONTRIBUTING.md" },
];

interface ProjectDocGenModalProps {
  initialDocType: ProjectDocType;
  onClose: () => void;
}

export function ProjectDocGenModal({
  initialDocType,
  onClose,
}: ProjectDocGenModalProps) {
  const project = useCortexStore((s) => s.activeProject);
  const [docType, setDocType] = useState<ProjectDocType>(initialDocType);
  const [view, setView] = useState<"preview" | "source">("preview");
  const [result, setResult] = useState<ProjectDocResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // Kick off (or re-fire) the generation whenever doc_type changes — the
  // backend's prompt + suggested path both differ per type, so we need a
  // fresh call rather than re-rendering the same payload.
  useEffect(() => {
    if (!project) {
      setError("No active project — pick one from the sidebar first.");
      setLoading(false);
      return;
    }
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      setResult(null);
      try {
        const out = await generateProjectDoc(String(project.root), docType);
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
  }, [project, docType]);

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
        body: `${docTypeLabel(result.doc_type)} on clipboard.`,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onSave = useCallback(async () => {
    if (!result) return;
    const ok = await confirmDialog({
      title: "Save document",
      message: `Write ${docTypeLabel(result.doc_type)} to ${result.suggested_path}?\n\nIf the file already exists it will be overwritten.`,
      confirmLabel: "Overwrite",
      danger: true,
    });
    if (!ok) return;
    try {
      const written = await saveFileText(result.suggested_path, result.markdown);
      pushToast({ title: "Saved", body: written, kind: "success" });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  return (
    <div className="projectdoc-backdrop" onClick={onClose}>
      <div
        className="projectdoc-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="projectdoc-title"
      >
        <header className="projectdoc-header">
          <div>
            <h2 id="projectdoc-title">Project doc generator</h2>
            <div
              className="projectdoc-path"
              title={String(project?.root ?? "")}
            >
              {project ? project.name : "no active project"}
            </div>
          </div>
          <button
            className="projectdoc-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="projectdoc-toolbar">
          <fieldset
            className="projectdoc-radio-group"
            aria-label="Document type"
          >
            <legend className="projectdoc-radio-legend">Doc type</legend>
            {DOC_TYPES.map((opt) => (
              <label key={opt.value} className="projectdoc-radio">
                <input
                  type="radio"
                  name="projectdoc-type"
                  value={opt.value}
                  checked={docType === opt.value}
                  onChange={() => setDocType(opt.value)}
                  disabled={loading}
                />
                <span>{opt.label}</span>
              </label>
            ))}
          </fieldset>

          <div className="projectdoc-view-toggle" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={view === "preview"}
              className={
                view === "preview"
                  ? "projectdoc-view-btn projectdoc-view-btn-active"
                  : "projectdoc-view-btn"
              }
              onClick={() => setView("preview")}
              disabled={loading || !result}
            >
              Preview
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={view === "source"}
              className={
                view === "source"
                  ? "projectdoc-view-btn projectdoc-view-btn-active"
                  : "projectdoc-view-btn"
              }
              onClick={() => setView("source")}
              disabled={loading || !result}
            >
              Source
            </button>
          </div>
        </div>

        <div className="projectdoc-body">
          {loading && (
            <div className="projectdoc-loading">
              <span className="projectdoc-spinner" aria-hidden /> Generating{" "}
              {docTypeLabel(docType)}…
            </div>
          )}

          {error && !loading && (
            <div className="projectdoc-error">
              Failed to generate document.
              <pre>{error}</pre>
            </div>
          )}

          {result && !loading && (
            <div className="projectdoc-result">
              <div className="projectdoc-meta">
                <span title="Suggested save path">{result.suggested_path}</span>
              </div>
              {view === "preview" ? (
                <div className="projectdoc-markdown">
                  <MarkdownView source={result.markdown} />
                </div>
              ) : (
                <pre className="projectdoc-source">{result.markdown}</pre>
              )}
            </div>
          )}
        </div>

        <footer className="projectdoc-footer">
          <button
            className="projectdoc-action"
            onClick={onCopy}
            disabled={!result || loading}
          >
            Copy
          </button>
          <button
            className="projectdoc-action"
            onClick={onSave}
            disabled={!result || loading}
          >
            Save to suggested path
          </button>
          <button className="projectdoc-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

function docTypeLabel(t: ProjectDocType): string {
  switch (t) {
    case "readme":
      return "README.md";
    case "claude-md":
      return "CLAUDE.md";
    case "contributing":
      return "CONTRIBUTING.md";
  }
}

/**
 * Imperative summoner used by the `/readme` and `/claude-md` slash commands.
 * Same detached-root pattern as ChangelogModal / DocGenModal.
 */
let activeRoot: Root | null = null;

export function openProjectDocGenModal(docType: ProjectDocType): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "projectdoc";
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
    <ProjectDocGenModal initialDocType={docType} onClose={close} />,
  );
}
