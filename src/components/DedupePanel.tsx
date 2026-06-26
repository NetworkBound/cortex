import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { EDITOR_OPEN_EVENT } from "@/lib/editor";
import { findDuplicateMemory, type DuplicatePair } from "@/lib/memory-dedupe";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Memory-dedupe modal. Mirrors the `IDEExportModal` self-mounting portal so
 * the `/dedupe` slash command can summon it without App.tsx wiring. Lists
 * Jaccard-similar markdown pairs across every memory source — click "Open A"
 * / "Open B" to dispatch a `cortex:editor-open` event for either file.
 */

interface DedupePanelProps {
  onClose: () => void;
}

const MIN_THRESHOLD = 0.3;
const MAX_THRESHOLD = 0.9;
const DEFAULT_THRESHOLD = 0.4;

function basename(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

function openInEditor(path: string): void {
  try {
    window.dispatchEvent(
      new CustomEvent(EDITOR_OPEN_EVENT, { detail: { path } }),
    );
  } catch {
    /* Not in a browser-like env — best-effort */
  }
}

export function DedupePanel({ onClose }: DedupePanelProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [threshold, setThreshold] = useState(DEFAULT_THRESHOLD);
  const [busy, setBusy] = useState(false);
  const [pairs, setPairs] = useState<DuplicatePair[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onScan = useCallback(async () => {
    setBusy(true);
    setError(null);
    setPairs(null);
    try {
      const result = await findDuplicateMemory({
        threshold,
        activeProject: activeProject?.root ?? null,
      });
      setPairs(result);
      pushToast({
        title: "Memory scan complete",
        body: `${result.length} pair${result.length === 1 ? "" : "s"} at ≥ ${threshold.toFixed(2)}.`,
        kind: result.length === 0 ? "info" : "success",
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [threshold, activeProject]);

  return (
    <div className="dedupe-backdrop" onMouseDown={onClose}>
      <div
        className="dedupe-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="dedupe-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="dedupe-header">
          <h2 id="dedupe-title">Memory dedupe</h2>
          <button className="dedupe-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="dedupe-summary">
          Finds markdown files with similar content across every memory source
          (claude project memory, runbooks, global instructions, Obsidian).
          Jaccard similarity over normalized word sets.
        </p>

        <div className="dedupe-controls">
          <label className="dedupe-slider">
            <span>
              Threshold: <strong>{threshold.toFixed(2)}</strong>
            </span>
            <input
              type="range"
              min={MIN_THRESHOLD}
              max={MAX_THRESHOLD}
              step={0.05}
              value={threshold}
              onChange={(e) => setThreshold(parseFloat(e.target.value))}
              disabled={busy}
            />
          </label>
          <button
            className="dedupe-primary"
            onClick={onScan}
            disabled={busy}
          >
            {busy ? "Scanning…" : "Scan"}
          </button>
        </div>

        {error && <div className="dedupe-error">{error}</div>}

        {pairs && pairs.length === 0 && !error && (
          <div className="dedupe-empty">
            No duplicates found at this threshold. Try lowering it.
          </div>
        )}

        {pairs && pairs.length > 0 && (
          <ul className="dedupe-list">
            {pairs.map((pair, idx) => (
              <li key={`${pair.file_a}-${pair.file_b}-${idx}`} className="dedupe-pair">
                <div className="dedupe-pair-head">
                  <span className="dedupe-score">
                    {(pair.similarity * 100).toFixed(0)}%
                  </span>
                  <div className="dedupe-files">
                    <div className="dedupe-file">
                      <code title={pair.file_a}>{basename(pair.file_a)}</code>
                      <button
                        className="dedupe-open"
                        onClick={() => openInEditor(pair.file_a)}
                      >
                        Open A
                      </button>
                    </div>
                    <div className="dedupe-file">
                      <code title={pair.file_b}>{basename(pair.file_b)}</code>
                      <button
                        className="dedupe-open"
                        onClick={() => openInEditor(pair.file_b)}
                      >
                        Open B
                      </button>
                    </div>
                  </div>
                </div>
                {pair.shared_words.length > 0 && (
                  <div className="dedupe-chips">
                    {pair.shared_words.map((w) => (
                      <span key={w} className="dedupe-chip">
                        {w}
                      </span>
                    ))}
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}

        <footer className="dedupe-footer">
          <button className="dedupe-secondary" onClick={onClose} disabled={busy}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/** Imperative summoner — same pattern as `openIDEExportModal`. */
let activeRoot: Root | null = null;

export function openDedupePanel(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "dedupe";
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
  root.render(<DedupePanel onClose={close} />);
}
