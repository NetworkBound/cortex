import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  applyPresetState,
  deleteWorkspacePreset,
  listWorkspacePresets,
  savePresetFromCurrentState,
  type WorkspacePreset,
} from "@/lib/workspace-presets";
import { confirmDialog, promptDialog } from "@/lib/dialogs";
import { timeAgo } from "@/lib/time";
import { pushToast } from "@/lib/toast";

/**
 * Workspace presets modal. Same self-mounting portal pattern as
 * `DedupePanel` / `IDEExportModal` — App.tsx is intentionally untouched so
 * the `/preset` and `/layout` slash commands can summon it without wiring.
 *
 * Lists every saved preset with its name, description, and a compact state
 * badge strip; supports `Apply`, `Delete`, and "Save current as preset" via a
 * pair of in-app prompt dialogs for the name + description.
 */

interface WorkspacePresetsModalProps {
  onClose: () => void;
}

interface BadgeProps {
  label: string;
  value: string | null | undefined;
}

function Badge({ label, value }: BadgeProps) {
  if (!value) return null;
  return (
    <span className="wsp-badge" title={`${label}: ${value}`}>
      <span className="wsp-badge-label">{label}</span>
      <span className="wsp-badge-value">{value}</span>
    </span>
  );
}

export function WorkspacePresetsModal({ onClose }: WorkspacePresetsModalProps) {
  const [presets, setPresets] = useState<WorkspacePreset[] | null>(null);
  const [busyName, setBusyName] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const items = await listWorkspacePresets();
      setPresets(items);
    } catch (e) {
      setError(humanizeError(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onSaveCurrent = useCallback(async () => {
    const name = await promptDialog({
      title: "Save preset",
      message: "Preset name (letters, digits, _ - . space)",
      placeholder: "e.g. review-layout",
    });
    if (!name) return;
    const description =
      (await promptDialog({
        title: "Save preset",
        message: "Description (optional)",
      })) ?? "";
    setBusyName(name);
    setError(null);
    try {
      const saved = await savePresetFromCurrentState(name, description);
      if (!saved) {
        setError("Save failed — see console for details.");
        return;
      }
      pushToast({
        title: "Preset saved",
        body: `'${saved.name}' captured.`,
        kind: "success",
      });
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyName(null);
    }
  }, [refresh]);

  const onApply = useCallback(async (preset: WorkspacePreset) => {
    setBusyName(preset.name);
    setError(null);
    try {
      const report = await applyPresetState(preset);
      const appliedSummary =
        report.applied.length > 0
          ? `applied: ${report.applied.join(", ")}`
          : "nothing applied";
      const skippedSummary =
        report.skipped.length > 0 ? ` · skipped: ${report.skipped.join(", ")}` : "";
      pushToast({
        title: `Preset '${preset.name}' applied`,
        body: `${appliedSummary}${skippedSummary}`,
        kind: report.applied.length > 0 ? "success" : "warning",
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyName(null);
    }
  }, []);

  const onDelete = useCallback(async (preset: WorkspacePreset) => {
    if (
      !(await confirmDialog({
        title: "Delete preset?",
        message: `'${preset.name}' will be deleted.`,
        confirmLabel: "Delete",
        danger: true,
      }))
    )
      return;
    setBusyName(preset.name);
    setError(null);
    try {
      const ok = await deleteWorkspacePreset(preset.name);
      if (!ok) {
        setError(`Delete of '${preset.name}' failed.`);
        return;
      }
      pushToast({ title: "Preset deleted", body: preset.name, kind: "info" });
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyName(null);
    }
  }, [refresh]);

  const empty = useMemo(() => presets !== null && presets.length === 0, [presets]);

  return (
    <div className="wsp-backdrop" onMouseDown={onClose}>
      <div
        className="wsp-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="wsp-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="wsp-header">
          <h2 id="wsp-title">Workspace presets</h2>
          <button className="wsp-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="wsp-summary">
          Saved layouts capture the active panel tab, mode, sandbox tier, theme,
          gateway model, and right-column tab. Restore any preset to flip the
          whole workspace in one shot.
        </p>

        <div className="wsp-controls">
          <button
            className="wsp-primary"
            onClick={onSaveCurrent}
            disabled={busyName !== null}
          >
            Save current as preset
          </button>
        </div>

        {error && <div className="wsp-error">{error}</div>}

        {presets === null && <div className="wsp-empty">Loading…</div>}

        {empty && (
          <div className="wsp-empty">
            No presets yet. Click <strong>Save current as preset</strong> to capture
            the current layout.
          </div>
        )}

        {presets && presets.length > 0 && (
          <ul className="wsp-list">
            {presets.map((p) => {
              const busy = busyName === p.name;
              return (
                <li key={p.name} className="wsp-row">
                  <div className="wsp-row-head">
                    <div className="wsp-row-title">
                      <strong>{p.name}</strong>
                      <span className="wsp-row-age">{timeAgo(p.created_unix_ms, { coarse: true })}</span>
                    </div>
                    <div className="wsp-row-actions">
                      <button
                        className="wsp-secondary"
                        onClick={() => onApply(p)}
                        disabled={busy}
                      >
                        {busy ? "Applying…" : "Apply"}
                      </button>
                      <button
                        className="wsp-danger"
                        onClick={() => onDelete(p)}
                        disabled={busy}
                      >
                        Delete
                      </button>
                    </div>
                  </div>
                  {p.description && (
                    <p className="wsp-row-desc">{p.description}</p>
                  )}
                  <div className="wsp-badges">
                    <Badge label="tab" value={p.state.activity_tab} />
                    <Badge label="mode" value={p.state.mode} />
                    <Badge label="sandbox" value={p.state.sandbox_tier} />
                    <Badge label="theme" value={p.state.theme} />
                    <Badge label="model" value={p.state.gateway_model ?? p.state.hermes_model} />
                    <Badge label="right" value={p.state.right_tab} />
                  </div>
                </li>
              );
            })}
          </ul>
        )}

        <footer className="wsp-footer">
          <button className="wsp-secondary" onClick={onClose} disabled={busyName !== null}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/** Imperative summoner — same pattern as `openDedupePanel`. */
let activeRoot: Root | null = null;

export function openWorkspacePresetsModal(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "workspace-presets";
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
  root.render(<WorkspacePresetsModal onClose={close} />);
}
