import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import {
  getModelRoles,
  setModelRoles,
  MODEL_ROLE_KEYS,
  MODEL_ROLE_META,
  type ModelRoleKey,
  type ModelRoles,
} from "@/lib/model-roles";
import { listModels, type ModelEntry } from "@/lib/models";
import { exportDiagnostics, type DiagnosticsExport } from "@/lib/diagnostics";
import { SettingsSection, SettingsToggle } from "./Section";
import type { SectionDef } from "./types";

/**
 * Continue.dev-style per-project model roles (default model per logical
 * role), persisted at `.cortex/model-roles.toml`. `modelList` populates the
 * per-role `<select>`s; an empty selection clears that role.
 */
function ModelRolesGroup() {
  const root = useCortexStore((s) => s.activeProject?.root ?? null);
  const [roles, setRoles] = useState<ModelRoles>({});
  const [modelList, setModelList] = useState<ModelEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listModels()
      .then((list) => {
        if (!cancelled) setModelList(list);
      })
      .catch(() => {
        if (!cancelled) setModelList([]);
      });
    if (!root) {
      setRoles({});
      setErr(null);
      return () => {
        cancelled = true;
      };
    }
    getModelRoles(root)
      .then((r) => {
        if (!cancelled) {
          setRoles(r);
          setErr(null);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setRoles({});
          setErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  // Persist a single role assignment (blank clears it). Optimistic local
  // update, then write to disk; on failure surface inline and reload the
  // stored map.
  async function update(key: ModelRoleKey, value: string) {
    if (!root) return;
    const next: ModelRoles = { ...roles, [key]: value || null };
    setRoles(next);
    setErr(null);
    try {
      setRoles(await setModelRoles(root, next));
    } catch (e) {
      setErr(humanizeError(e));
      try {
        setRoles(await getModelRoles(root));
      } catch {
        /* keep optimistic */
      }
    }
  }

  return (
    <div className="settings-group">
      <div className="settings-subheading">Default model per role</div>
      <div className="settings-hint">
        Continue.dev-style. Pin a default model per role for this project. An
        explicit composer pick (chat) or <code>/architect</code> override always
        wins; <em>Auto</em> leaves the role unset.
      </div>
      {!root ? (
        <div className="settings-hint gap-top">
          Open a project to assign per-role models.
        </div>
      ) : (
        <div className="settings-stack gap-top">
          {MODEL_ROLE_KEYS.map((key) => {
            const value = roles[key] ?? "";
            // The stored value may be a model not in the live list (offline
            // gateway, a typed alias) — keep it selectable.
            const known = modelList.some((m) => m.id === value);
            return (
              <label key={key} className="settings-field-row">
                <span className="settings-field-label">
                  {MODEL_ROLE_META[key].label}
                </span>
                <select
                  value={value}
                  title={MODEL_ROLE_META[key].help}
                  onChange={(e) => void update(key, e.target.value)}
                >
                  <option value="">Auto / default</option>
                  {!known && value && <option value={value}>{value}</option>}
                  {modelList.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.label} ({m.source})
                    </option>
                  ))}
                </select>
              </label>
            );
          })}
        </div>
      )}
      {err && <div className="settings-err gap-top">{err}</div>}
    </div>
  );
}

/** Diagnostics export. The backend writes a redacted bundle under ~/.cortex. */
function DiagnosticsGroup() {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<DiagnosticsExport | null>(null);
  const [err, setErr] = useState<string | null>(null);

  async function run() {
    setBusy(true);
    setErr(null);
    try {
      const res = await exportDiagnostics();
      setResult(res);
      pushToast({
        title: "Diagnostics exported",
        body: res.path,
        kind: "success",
      });
    } catch (e) {
      setResult(null);
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function copyPath() {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.path);
      pushToast({ title: "Path copied", kind: "success" });
    } catch (e) {
      pushToast({
        title: "Copy failed",
        body: humanizeError(e),
        kind: "error",
      });
    }
  }

  return (
    <div className="settings-group">
      <div className="settings-subheading">Diagnostics</div>
      <div className="settings-hint">
        Bundle app version, OS info, the crash log, recent session metadata
        (never message contents) and a redacted config snapshot into a single
        archive you can attach to a bug report. Keys, tokens, private IPs and
        home paths are scrubbed before anything touches disk.
      </div>
      <div className="settings-row gap-top">
        <button type="button" onClick={() => void run()} disabled={busy}>
          {busy ? "Exporting…" : "Export diagnostics"}
        </button>
      </div>
      {result && (
        <div className="settings-row wrap gap-top">
          <code className="settings-mono settings-label-sm">{result.path}</code>
          <button
            type="button"
            className="settings-label-sm"
            onClick={() => void copyPath()}
          >
            Copy path
          </button>
          <small className="settings-muted">
            {result.files.length} files inside
          </small>
        </div>
      )}
      {err && <div className="settings-err gap-top">{err}</div>}
    </div>
  );
}

/** Power-user toggles wired to existing store actions (no new store fields). */
function AdvancedSection() {
  const architectMode = useCortexStore((s) => s.architectMode);
  const setArchitectMode = useCortexStore((s) => s.setArchitectMode);
  const autoCondenseEnabled = useCortexStore((s) => s.autoCondenseEnabled);
  const setAutoCondenseEnabled = useCortexStore(
    (s) => s.setAutoCondenseEnabled,
  );
  const autoCondenseThreshold = useCortexStore((s) => s.autoCondenseThreshold);
  const setAutoCondenseThreshold = useCortexStore(
    (s) => s.setAutoCondenseThreshold,
  );

  return (
    <SettingsSection
      title="Advanced"
      description="Power-user toggles. Each persists locally and applies immediately."
    >
      <SettingsToggle
        checked={architectMode}
        onChange={setArchitectMode}
        label="Architect mode"
        description="Aider-style split: plan with one model, edit with another."
      />

      <ModelRolesGroup />

      <div className="gap-top">
        <SettingsToggle
          checked={autoCondenseEnabled}
          onChange={setAutoCondenseEnabled}
          label="Auto-condense on overflow"
          description="Fold older turns into an LLM summary automatically once the conversation fills the model's context window (Cline-style)."
        />
      </div>
      {autoCondenseEnabled && (
        <label className="settings-field-row settings-subrow">
          <span>Condense at</span>
          <input
            type="number"
            min={50}
            max={95}
            step={5}
            value={autoCondenseThreshold}
            onChange={(e) => setAutoCondenseThreshold(Number(e.target.value))}
          />
          <span className="settings-muted">% of the context window</span>
        </label>
      )}

      <DiagnosticsGroup />
    </SettingsSection>
  );
}

export const ADVANCED_SECTIONS: SectionDef[] = [
  {
    tab: "advanced",
    heading: "Advanced",
    text: "advanced power user developer experimental flags architect mode planner editor split toggle auto condense overflow context window summary cline threshold default model per role chat continue.dev model roles assignment export diagnostics bug report crash log bundle redacted support troubleshoot",
    render: () => <AdvancedSection />,
  },
];
