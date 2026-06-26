import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { generateDocs, type DocResult, type DocStyle } from "@/lib/doc-gen";
import { saveFileText } from "@/lib/editor-save";
import { pushToast } from "@/lib/toast";
import { confirmDialog, promptDialog } from "@/lib/dialogs";

/**
 * AI doc generator modal. Renders as a self-mounting portal so the
 * `/docgen` slash command can summon it without touching App.tsx — same
 * pattern as `RefactorSuggesterModal` / `SessionSummaryModal`.
 *
 * On mount we kick off `generate_docs` for the file. The style dropdown at
 * the top re-fires the call so the user can swap between auto-detection and
 * a specific style (rust / jsdoc / python / markdown / generic). The body
 * renders the original on the left and the documented version on the right
 * in monospace. Actions:
 *   - Copy with docs → clipboard
 *   - Save as new file → prompts for a sibling filename
 *   - Replace original → confirm, then `save_file_text(path, with_docs)`
 */

interface DocGenModalProps {
  path: string;
  onClose: () => void;
}

const STYLE_OPTIONS: { value: DocStyle; label: string }[] = [
  { value: "auto", label: "Auto (by extension)" },
  { value: "rust", label: "Rust (///)" },
  { value: "jsdoc", label: "JSDoc (@param/@returns)" },
  { value: "python", label: "Python docstring" },
  { value: "markdown", label: "Markdown outline" },
  { value: "generic", label: "Generic comments" },
];

export function DocGenModal({ path, onClose }: DocGenModalProps) {
  const [style, setStyle] = useState<DocStyle>("auto");
  const [result, setResult] = useState<DocResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // Kick off the analysis on mount and whenever the style dropdown changes.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await generateDocs(path, style);
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
  }, [path, style]);

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
      await navigator.clipboard.writeText(result.with_docs);
      pushToast({
        title: "Copied",
        body: "Documented file on clipboard.",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [result]);

  const onSaveNew = useCallback(async () => {
    if (!result) return;
    // Default: insert `.docs` before the extension so the user gets a
    // sensible filename without typing. `foo.rs` → `foo.docs.rs`.
    const dot = path.lastIndexOf(".");
    const sep = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    const suggested =
      dot > sep ? `${path.slice(0, dot)}.docs${path.slice(dot)}` : `${path}.docs`;
    const target = await promptDialog({
      title: "Save documented file",
      message: "Save documented file as:",
      initialValue: suggested,
    });
    if (!target || !target.trim()) return;
    try {
      const written = await saveFileText(target.trim(), result.with_docs);
      pushToast({
        title: "Saved",
        body: written,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    }
  }, [result, path]);

  const onReplace = useCallback(async () => {
    if (!result) return;
    const ok = await confirmDialog({
      title: "Replace original file",
      message: `Replace ${path} with the documented version?\n\nThe original will be overwritten on disk.`,
      confirmLabel: "Overwrite",
      danger: true,
    });
    if (!ok) return;
    try {
      await saveFileText(path, result.with_docs);
      pushToast({
        title: "Replaced",
        body: path,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Replace failed", body: humanizeError(e), kind: "error" });
    }
  }, [result, path]);

  return (
    <div className="docgen-backdrop" onClick={onClose}>
      <div
        className="docgen-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="docgen-title"
      >
        <header className="docgen-header">
          <div>
            <h2 id="docgen-title">Documentation generator</h2>
            <div className="docgen-path" title={path}>
              {path}
            </div>
          </div>
          <button className="docgen-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="docgen-toolbar">
          <label className="docgen-style-label">
            Style:
            <select
              value={style}
              onChange={(e) => setStyle(e.target.value as DocStyle)}
              disabled={loading}
              aria-label="Documentation style"
            >
              {STYLE_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </label>
          {result && !loading && (
            <div className="docgen-meta">
              <span title="Detected language">{result.language}</span>
              <span aria-hidden>·</span>
              <span title="Resolved doc style">{result.style}</span>
            </div>
          )}
        </div>

        <div className="docgen-body">
          {loading && (
            <div className="docgen-loading">
              <span className="docgen-spinner" aria-hidden /> Generating documentation…
            </div>
          )}

          {error && !loading && (
            <div className="docgen-error">
              Failed to generate docs.
              <pre>{error}</pre>
            </div>
          )}

          {result && !loading && (
            <div className="docgen-diff">
              <div className="docgen-diff-col">
                <div className="docgen-diff-label">Original</div>
                <pre className="docgen-code">{result.original}</pre>
              </div>
              <div className="docgen-diff-col">
                <div className="docgen-diff-label">With docs</div>
                <pre className="docgen-code">{result.with_docs}</pre>
              </div>
            </div>
          )}
        </div>

        <footer className="docgen-footer">
          <button
            className="docgen-action"
            onClick={onCopy}
            disabled={!result || loading}
          >
            Copy with docs
          </button>
          <button
            className="docgen-action"
            onClick={onSaveNew}
            disabled={!result || loading}
          >
            Save as new file
          </button>
          <button
            className="docgen-action docgen-action-danger"
            onClick={onReplace}
            disabled={!result || loading}
          >
            Replace original
          </button>
          <button className="docgen-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/docgen` slash command. Same detached-
 * root pattern as `RefactorSuggesterModal`.
 */
let activeRoot: Root | null = null;

export function openDocGenModal(path: string): void {
  if (activeRoot) return; // already open
  if (!path) {
    pushToast({
      title: "No file",
      body: "Open a file in the editor or pass a path: /docgen <path>.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "docgen";
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
  root.render(<DocGenModal path={path} onClose={close} />);
}
