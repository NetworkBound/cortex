import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";

import {
  brainToc,
  formatTocAsMarkdown,
  kindLabel,
  type TocFile,
  type TocResult,
  type TocSource,
} from "@/lib/brain-toc";
import { EDITOR_OPEN_EVENT } from "@/lib/editor";
import { pushToast } from "@/lib/toast";
import { Chevron } from "@/lib/chevron";

/**
 * `/toc` Cortex Brain table-of-contents modal.
 *
 * One backend call (`brain_toc`) builds the entire outline; clicking a heading
 * dispatches a `cortex:editor-open` window event with the path + 1-based line
 * hint so the editor pane can pick it up. Self-mounting portal — same pattern
 * as `DedupePanel` / `IDEExportModal` so App.tsx stays untouched.
 *
 * Sources and files are both collapsible. The search box filters on file
 * title, file path, and heading text (case-insensitive substring); when a
 * filter is active the matching files auto-expand so headings stay reachable
 * without an extra click.
 */

interface BrainTocModalProps {
  onClose: () => void;
}

function openInEditor(path: string, line?: number) {
  try {
    window.dispatchEvent(
      // EDITOR_OPEN_EVENT carries `path`; we tack on `line` so future versions
      // of EditorPane can scroll to it. Unknown fields are ignored today.
      new CustomEvent(EDITOR_OPEN_EVENT, { detail: { path, line } }),
    );
  } catch {
    /* not in a browser-like env — best-effort */
  }
}

function matchesFilter(file: TocFile, needle: string): boolean {
  if (!needle) return true;
  const q = needle.toLowerCase();
  if (file.title.toLowerCase().includes(q)) return true;
  if (file.path.toLowerCase().includes(q)) return true;
  return file.headings.some((h) => h.text.toLowerCase().includes(q));
}

function filterHeadings(file: TocFile, needle: string) {
  if (!needle) return file.headings;
  const q = needle.toLowerCase();
  // When the file's title/path matches we keep all headings; otherwise we
  // narrow to the ones that match so the user sees what hit.
  if (
    file.title.toLowerCase().includes(q) ||
    file.path.toLowerCase().includes(q)
  ) {
    return file.headings;
  }
  return file.headings.filter((h) => h.text.toLowerCase().includes(q));
}

export function BrainTocModal({ onClose }: BrainTocModalProps) {
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<TocResult | null>(null);
  const [needle, setNeedle] = useState("");
  const [collapsedSources, setCollapsedSources] = useState<Set<string>>(new Set());
  const [collapsedFiles, setCollapsedFiles] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await brainToc();
        if (cancelled) return;
        setResult(r);
        // Default: first source expanded, rest collapsed. Files default
        // collapsed too — large vaults render in tens of seconds otherwise.
        const srcs = new Set<string>();
        r.sources.forEach((s, i) => {
          if (i > 0) srcs.add(s.label);
        });
        setCollapsedSources(srcs);
        const files = new Set<string>();
        r.sources.forEach((s) => {
          s.files.forEach((f) => files.add(f.path));
        });
        setCollapsedFiles(files);
        setStatus("ready");
      } catch (e) {
        if (cancelled) return;
        setError(humanizeError(e));
        setStatus("error");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const toggleSource = useCallback((label: string) => {
    setCollapsedSources((prev) => {
      const next = new Set(prev);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });
  }, []);

  const toggleFile = useCallback((path: string) => {
    setCollapsedFiles((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  // Apply the search filter. When the needle is non-empty, sources / files
  // with zero matching items are dropped entirely so the list shrinks.
  const filteredSources = useMemo<TocSource[]>(() => {
    if (!result) return [];
    if (!needle.trim()) return result.sources;
    const out: TocSource[] = [];
    for (const src of result.sources) {
      const files = src.files.filter((f) => matchesFilter(f, needle));
      if (files.length === 0) continue;
      out.push({ ...src, files });
    }
    return out;
  }, [result, needle]);

  // When filtering, force matching files open so the user sees the heading hit.
  const effectiveCollapsedFiles = useMemo(() => {
    if (!needle.trim()) return collapsedFiles;
    return new Set<string>();
  }, [needle, collapsedFiles]);

  const onCopyMarkdown = useCallback(async () => {
    if (!result) return;
    const md = formatTocAsMarkdown(result);
    try {
      await navigator.clipboard.writeText(md);
      pushToast({
        title: "TOC copied",
        body: `${result.file_count} files, ${result.heading_count} headings.`,
        kind: "success",
      });
    } catch (e) {
      pushToast({
        title: "Copy failed",
        body: humanizeError(e),
        kind: "error",
      });
    }
  }, [result]);

  return (
    <div className="brain-toc-backdrop" onMouseDown={onClose}>
      <div
        className="brain-toc-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="brain-toc-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="brain-toc-header">
          <div>
            <h2 id="brain-toc-title">Cortex Brain — table of contents</h2>
            {result ? (
              <p className="brain-toc-subtitle muted">
                {result.file_count} file{result.file_count === 1 ? "" : "s"} ·{" "}
                {result.heading_count} heading
                {result.heading_count === 1 ? "" : "s"}
                {result.truncated ? " · truncated at 500 files" : ""}
              </p>
            ) : null}
          </div>
          <div className="brain-toc-actions">
            <button
              className="brain-toc-copy"
              onClick={onCopyMarkdown}
              disabled={!result || result.file_count === 0}
              title="Copy the whole TOC as markdown to the clipboard"
            >
              Copy as markdown
            </button>
            <button
              className="brain-toc-close"
              onClick={onClose}
              aria-label="Close"
            >
              ×
            </button>
          </div>
        </header>

        <div className="brain-toc-search">
          <input
            type="search"
            placeholder="Filter files and headings…"
            value={needle}
            onChange={(e) => setNeedle(e.target.value)}
            autoFocus
          />
        </div>

        {status === "loading" && (
          <div className="brain-toc-status muted">scanning memory sources…</div>
        )}

        {status === "error" && (
          <div className="brain-toc-status brain-toc-error">
            failed to load: {error ?? "unknown error"}
          </div>
        )}

        {status === "ready" && filteredSources.length === 0 && (
          <div className="brain-toc-status muted">
            {needle.trim()
              ? "No files or headings match this filter."
              : "No markdown files found in any memory source."}
          </div>
        )}

        {status === "ready" && filteredSources.length > 0 && (
          <ul className="brain-toc-sources">
            {filteredSources.map((src) => {
              const srcCollapsed = collapsedSources.has(src.label);
              return (
                <li key={`${src.kind}::${src.label}`} className="brain-toc-source">
                  <button
                    type="button"
                    className="brain-toc-source-head"
                    onClick={() => toggleSource(src.label)}
                    aria-expanded={!srcCollapsed}
                  >
                    <span className="brain-toc-chev" aria-hidden="true">
                      <Chevron open={!srcCollapsed} size={12} />
                    </span>
                    <span className="brain-toc-source-kind">
                      {kindLabel(src.kind)}
                    </span>
                    <span className="brain-toc-source-label muted">
                      {src.label}
                    </span>
                    <span className="brain-toc-source-count muted">
                      {src.files.length} file{src.files.length === 1 ? "" : "s"}
                    </span>
                  </button>

                  {!srcCollapsed && (
                    <ul className="brain-toc-files">
                      {src.files.map((file) => {
                        const fileCollapsed = effectiveCollapsedFiles.has(
                          file.path,
                        );
                        const headings = filterHeadings(file, needle);
                        return (
                          <li key={file.path} className="brain-toc-file">
                            <div className="brain-toc-file-head">
                              <button
                                type="button"
                                className="brain-toc-file-toggle"
                                onClick={() => toggleFile(file.path)}
                                aria-expanded={!fileCollapsed}
                              >
                                <span className="brain-toc-chev" aria-hidden="true">
                                  <Chevron open={!fileCollapsed} size={12} />
                                </span>
                                <span className="brain-toc-file-title">
                                  {file.title}
                                </span>
                              </button>
                              <button
                                type="button"
                                className="brain-toc-file-open"
                                onClick={() => openInEditor(file.path)}
                                title={file.path}
                              >
                                Open
                              </button>
                            </div>
                            {!fileCollapsed && headings.length > 0 && (
                              <ul className="brain-toc-headings">
                                {headings.map((h, i) => (
                                  <li
                                    key={`${file.path}::${h.line}::${i}`}
                                    className={`brain-toc-heading brain-toc-h${h.level}`}
                                    style={{ paddingLeft: `${(h.level - 1) * 14}px` }}
                                  >
                                    <button
                                      type="button"
                                      className="brain-toc-heading-btn"
                                      onClick={() => openInEditor(file.path, h.line)}
                                      title={`line ${h.line}`}
                                    >
                                      {h.text}
                                    </button>
                                  </li>
                                ))}
                              </ul>
                            )}
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

/** Imperative summoner — same pattern as `openDedupePanel`. */
let activeRoot: Root | null = null;

export function openBrainTocModal(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "brain-toc";
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
  root.render(<BrainTocModal onClose={close} />);
}
