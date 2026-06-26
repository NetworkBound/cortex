/**
 * Trust matrix — Cline-style granular auto-approve toggles.
 *
 * Eight policy switches + a `max_requests_per_task` cap, persisted at
 * `~/.cortex/trust-matrix.json` via the `get_trust_matrix` / `set_trust_matrix`
 * Tauri commands. Other modules (approval pipeline) read the file at decision
 * time — this panel just owns the editing UI.
 *
 * All edits write through immediately; there's no save button. Failures are
 * logged but don't block the UI so the toggles still reflect what the user
 * sees on screen.
 */

import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { PanelLoading } from "./Skeleton";
import { invoke } from "@tauri-apps/api/core";

interface TrustMatrixData {
  read_in_workspace: boolean;
  read_outside: boolean;
  edit_in_workspace: boolean;
  edit_outside: boolean;
  safe_commands: boolean;
  all_commands: boolean;
  browser: boolean;
  mcp: boolean;
  max_requests_per_task: number;
}

const DEFAULT_MATRIX: TrustMatrixData = {
  read_in_workspace: false,
  read_outside: false,
  edit_in_workspace: false,
  edit_outside: false,
  safe_commands: false,
  all_commands: false,
  browser: false,
  mcp: false,
  max_requests_per_task: 20,
};

interface ToggleDef {
  key: keyof Omit<TrustMatrixData, "max_requests_per_task">;
  label: string;
  hint: string;
  risk: "low" | "med" | "high";
}

const TOGGLES: ToggleDef[] = [
  { key: "read_in_workspace", label: "Read in workspace", hint: "Auto-approve reads of files in the project root.", risk: "low" },
  { key: "read_outside",      label: "Read outside",      hint: "Auto-approve reads of files anywhere on disk.",      risk: "med" },
  { key: "edit_in_workspace", label: "Edit in workspace", hint: "Auto-approve edits to files in the project root.", risk: "med" },
  { key: "edit_outside",      label: "Edit outside",      hint: "Auto-approve edits to files anywhere on disk.",     risk: "high" },
  { key: "safe_commands",     label: "Safe commands",     hint: "Auto-approve allow-listed read-only shell commands.", risk: "low" },
  { key: "all_commands",      label: "All commands",      hint: "Auto-approve every shell command. Use with care.",   risk: "high" },
  { key: "browser",           label: "Browser",           hint: "Auto-approve browser/playwright tool calls.",         risk: "med" },
  { key: "mcp",               label: "MCP",               hint: "Auto-approve MCP server tool invocations.",           risk: "med" },
];

export function TrustMatrix() {
  const [matrix, setMatrix] = useState<TrustMatrixData>(DEFAULT_MATRIX);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<TrustMatrixData>("get_trust_matrix")
      .then((m) => {
        if (!cancelled) {
          setMatrix({ ...DEFAULT_MATRIX, ...m });
          setLoaded(true);
        }
      })
      .catch((err) => {
        console.warn("get_trust_matrix failed", err);
        if (!cancelled) setLoaded(true);
      });
    return () => { cancelled = true; };
  }, []);

  // Fire-and-forget persist on every edit. We compute the next state from the
  // latest prior state via a functional update so rapid concurrent edits (e.g.
  // two toggles within one render frame) compose instead of clobbering each
  // other, then optimistically reflect it and log if the write fails.
  function update(mutate: (prev: TrustMatrixData) => TrustMatrixData) {
    setMatrix((prev) => {
      const next = mutate(prev);
      invoke("set_trust_matrix", { matrix: next }).catch((err) => {
        console.warn("set_trust_matrix failed", err);
        setError(humanizeError(err));
      });
      return next;
    });
  }

  function toggle(key: ToggleDef["key"]) {
    update((prev) => ({ ...prev, [key]: !prev[key] }));
  }

  function setMax(value: number) {
    // Clamp 1..1000 — the agent loop uses this as a hard ceiling so we
    // don't want pathological zero/negative values silently disabling work.
    const clamped = Math.max(1, Math.min(1000, Math.floor(value || 0) || 1));
    update((prev) => ({ ...prev, max_requests_per_task: clamped }));
  }

  if (!loaded) {
    return <PanelLoading label="Loading trust matrix" />;
  }

  return (
    <div className="trust-matrix">
      <div className="trust-matrix-intro muted">
        Granular auto-approve. Anything left off keeps the standard approval
        prompt; anything switched on runs without asking, up to the cap below.
      </div>

      <ul className="trust-matrix-list">
        {TOGGLES.map((t) => (
          <li key={t.key} className="trust-matrix-row">
            <button
              type="button"
              className={`trust-toggle ${matrix[t.key] ? "on" : ""} risk-${t.risk}`}
              onClick={() => toggle(t.key)}
              aria-pressed={matrix[t.key]}
            >
              <span className="trust-toggle-dot" />
              <span className="trust-toggle-label">{t.label}</span>
            </button>
            <span className="trust-toggle-hint muted">{t.hint}</span>
          </li>
        ))}
      </ul>

      <div className="trust-matrix-cap">
        <label htmlFor="trust-max-req">
          Max requests per task
          <input
            id="trust-max-req"
            type="number"
            min={1}
            max={1000}
            value={matrix.max_requests_per_task}
            onChange={(e) => setMax(Number(e.target.value))}
          />
        </label>
        <div className="muted trust-matrix-cap-hint">
          Hard ceiling on agent tool calls inside a single task. The agent
          will stop and ask once it hits this number.
        </div>
      </div>

      {error && (
        <div className="trust-matrix-error">
          Save failed: {error}
        </div>
      )}
    </div>
  );
}
