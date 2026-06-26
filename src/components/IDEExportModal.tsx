import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  IDE_FORMATS,
  exportIDEConfigs,
  type ExportResult,
  type IDEFormatId,
} from "@/lib/ide-export";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Modal for the multi-IDE config export feature. Renders as a self-mounting
 * portal so the slash command can summon it without App.tsx wiring.
 *
 * Format checkboxes are sticky-per-session — the same set the user picked
 * last time is pre-checked the next time they `/export-ide`. We don't persist
 * across reloads because the right answer usually depends on which project
 * is active.
 */

interface IDEExportModalProps {
  onClose: () => void;
}

const STORAGE_KEY = "cortex.ideExport.selected";

function loadSelection(): IDEFormatId[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return ["cursor", "windsurf"];
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return ["cursor", "windsurf"];
    const known = new Set(IDE_FORMATS.map((f) => f.id));
    return arr.filter((x): x is IDEFormatId => typeof x === "string" && known.has(x as IDEFormatId));
  } catch {
    return ["cursor", "windsurf"];
  }
}

function saveSelection(sel: IDEFormatId[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(sel));
  } catch {
    /* private mode / quota — best-effort */
  }
}

export function IDEExportModal({ onClose }: IDEExportModalProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [selected, setSelected] = useState<IDEFormatId[]>(() => loadSelection());
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ExportResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  // ESC closes the modal — the standard expectation for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const toggle = useCallback((id: IDEFormatId) => {
    setSelected((prev) => {
      const next = prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id];
      saveSelection(next);
      return next;
    });
  }, []);

  const onExport = useCallback(async () => {
    if (!activeProject) {
      setError("No active project — pick one from the sidebar first.");
      return;
    }
    if (selected.length === 0) {
      setError("Select at least one IDE format.");
      return;
    }
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const res = await exportIDEConfigs(activeProject.root, selected);
      setResult(res);
      pushToast({
        title: "IDE configs exported",
        body: `${res.written.length} written, ${res.skipped.length} skipped.`,
        kind: res.skipped.length === 0 ? "success" : "warning",
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [activeProject, selected]);

  return (
    <div className="ide-export-backdrop" onMouseDown={onClose}>
      <div
        className="ide-export-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="ide-export-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="ide-export-header">
          <h2 id="ide-export-title">Export IDE Configs</h2>
          <button className="ide-export-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>
        <p className="ide-export-summary">
          {activeProject ? (
            <>
              Generates rule files from <code>CLAUDE.md</code> + <code>AGENTS.md</code> +{" "}
              <code>.cortex/rules/*.md</code> for{" "}
              <strong>{activeProject.name}</strong>.
            </>
          ) : (
            <em>No active project — pick one from the sidebar first.</em>
          )}
        </p>

        <ul className="ide-export-list">
          {IDE_FORMATS.map((fmt) => (
            <li key={fmt.id}>
              <label className="ide-export-row">
                <input
                  type="checkbox"
                  checked={selected.includes(fmt.id)}
                  onChange={() => toggle(fmt.id)}
                />
                <span className="ide-export-label">{fmt.label}</span>
                <code className="ide-export-target">{fmt.target}</code>
              </label>
            </li>
          ))}
        </ul>

        {error && <div className="ide-export-error">{error}</div>}

        {result && (
          <div className="ide-export-result">
            {result.written.length > 0 && (
              <div>
                <strong>Written ({result.written.length}):</strong>
                <ul>
                  {result.written.map((p) => (
                    <li key={p}>
                      <code>{p}</code>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {result.skipped.length > 0 && (
              <div>
                <strong>Skipped ({result.skipped.length}):</strong>
                <ul>
                  {result.skipped.map((s, i) => (
                    <li key={`${s.path}-${i}`}>
                      <code>{s.path}</code> — {s.reason}
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}

        <footer className="ide-export-footer">
          <button className="ide-export-secondary" onClick={onClose} disabled={busy}>
            Close
          </button>
          <button
            className="ide-export-primary"
            onClick={onExport}
            disabled={busy || !activeProject || selected.length === 0}
          >
            {busy ? "Exporting…" : "Export"}
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the slash command. Creates a detached root
 * mounted on `document.body` and tears it down on close — no state lives in
 * App.tsx so we don't have to touch it.
 */
let activeRoot: Root | null = null;

export function openIDEExportModal(): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "ide-export";
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
  root.render(<IDEExportModal onClose={close} />);
}
