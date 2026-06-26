import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  CONFIG_PRESETS,
  findPreset,
  readConfigFile,
  writeConfigFile,
  type ConfigPreset,
} from "@/lib/config-files";
import { lineCount, parseJSON, prettify, type ParseError } from "@/lib/schema-editor";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Schema-locked settings editor. Renders a portal modal — same self-mount
 * pattern as IDEExportModal/KeyVaultPanel — and reads/writes a small,
 * curated set of Cortex config files via the `config_files` backend.
 *
 * The "schema" is intentionally lightweight: per-file static hint strings on
 * the right, plus an inline JSON syntax check (with line:col) at the bottom.
 * Save is disabled while the body is invalid JSON; Reload re-reads from disk.
 */

interface SchemaEditorProps {
  onClose: () => void;
  initialPresetId?: string;
}

interface LoadedState {
  preset: ConfigPreset;
  /** Body as currently typed in the textarea. */
  body: string;
  /** Body as last read from / written to disk — used by the dirty check. */
  pristine: string;
  /** Resolved absolute path (returned by the backend). */
  path: string;
  exists: boolean;
}

export function SchemaEditor({ onClose, initialPresetId }: SchemaEditorProps) {
  const activeProject = useCortexStore((s) => s.activeProject);
  const initial = useMemo(
    () => findPreset(initialPresetId ?? "") ?? CONFIG_PRESETS[0],
    [initialPresetId],
  );
  const [selectedId, setSelectedId] = useState<string>(initial.id);
  const [state, setState] = useState<LoadedState | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  // Inline validation error returned by the backend on save (e.g. a TOML
  // parse failure). Mirrors how JSON errors are surfaced, but for formats we
  // can't validate client-side it only populates after a save attempt.
  const [saveError, setSaveError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  const selected = useMemo(
    () => findPreset(selectedId) ?? CONFIG_PRESETS[0],
    [selectedId],
  );

  // Project-scoped configs need an active project; surface this in the UI
  // rather than letting the backend reject the call.
  const needsProject = selected.target.scope === "project" && !activeProject;

  const load = useCallback(
    async (preset: ConfigPreset) => {
      setLoading(true);
      setLoadError(null);
      setSaveError(null);
      try {
        const projectRoot =
          preset.target.scope === "project" ? activeProject?.root ?? null : null;
        if (preset.target.scope === "project" && !projectRoot) {
          setState(null);
          setLoadError("No active project — pick one from the sidebar first.");
          return;
        }
        const res = await readConfigFile(preset.target, projectRoot);
        setState({
          preset,
          body: res.body,
          pristine: res.body,
          path: res.path,
          exists: res.exists,
        });
      } catch (e) {
        setState(null);
        setLoadError(humanizeError(e));
      } finally {
        setLoading(false);
      }
    },
    [activeProject],
  );

  // Reload whenever the dropdown selection changes.
  useEffect(() => {
    void load(selected);
  }, [load, selected]);

  // ESC closes — match the rest of the modal family.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const parse = useMemo(() => {
    if (!state) return null;
    // Hint files (TOML etc) skip the JSON validator entirely.
    if (!state.preset.target.rel_path.endsWith(".json")) return null;
    return parseJSON(state.body);
  }, [state]);

  const dirty = state ? state.body !== state.pristine : false;
  const readOnly = selected.readOnly === true || state?.preset.readOnly === true;
  const canSave =
    !!state &&
    !readOnly &&
    !saving &&
    dirty &&
    (parse === null || parse.ok);

  const onSave = useCallback(async () => {
    if (!state) return;
    if (readOnly) return;
    setSaving(true);
    setSaveError(null);
    try {
      const projectRoot =
        state.preset.target.scope === "project" ? activeProject?.root ?? null : null;
      const path = await writeConfigFile(state.preset.target, state.body, projectRoot);
      setState({ ...state, pristine: state.body, path, exists: true });
      pushToast({
        title: "Config saved",
        body: `${state.preset.label} → ${path}`,
        kind: "success",
      });
    } catch (e) {
      // Surface backend validation errors (TOML parse failures etc.) inline so
      // the user sees the precise reason rather than only a transient toast.
      setSaveError(humanizeError(e));
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setSaving(false);
    }
  }, [activeProject, readOnly, state]);

  const onReload = useCallback(() => {
    void load(selected);
  }, [load, selected]);

  const onPrettify = useCallback(() => {
    if (!state || readOnly) return;
    if (!state.preset.target.rel_path.endsWith(".json")) return;
    const next = prettify(state.body);
    if (next !== state.body) setState({ ...state, body: next });
  }, [readOnly, state]);

  const lines = state ? lineCount(state.body) : 1;
  const gutter = useMemo(() => {
    const out: string[] = [];
    for (let i = 1; i <= lines; i++) out.push(String(i));
    return out.join("\n");
  }, [lines]);

  return (
    <div className="schema-editor-backdrop" onMouseDown={onClose}>
      <div
        className="schema-editor-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="schema-editor-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="schema-editor-header">
          <h2 id="schema-editor-title">Edit Config</h2>
          <button
            className="schema-editor-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        <div className="schema-editor-toolbar">
          <label className="schema-editor-select">
            File
            <select
              value={selectedId}
              onChange={(e) => setSelectedId(e.target.value)}
              disabled={loading || saving}
            >
              {CONFIG_PRESETS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label} — {p.display}
                </option>
              ))}
            </select>
          </label>
          <div className="schema-editor-toolbar-spacer" />
          <button
            className="schema-editor-secondary"
            onClick={onPrettify}
            disabled={!state || readOnly || loading || saving}
            title="Reformat JSON with 2-space indent"
          >
            Prettify
          </button>
          <button
            className="schema-editor-secondary"
            onClick={onReload}
            disabled={loading || saving}
          >
            {loading ? "Loading…" : "Reload"}
          </button>
          <button
            className="schema-editor-primary"
            onClick={onSave}
            disabled={!canSave}
            title={
              readOnly
                ? "This file is read-only in this build"
                : !dirty
                  ? "No changes to save"
                  : parse && !parse.ok
                    ? "Fix the JSON syntax error first"
                    : "Save changes"
            }
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>

        <p className="schema-editor-path">
          <code>{state?.path ?? selected.display}</code>{" "}
          {state && !state.exists && <em>(will be created on save)</em>}
          {needsProject && <em> — needs an active project</em>}
        </p>

        <div className="schema-editor-body">
          <div className="schema-editor-editor">
            {loadError && (
              <div className="schema-editor-error">Load error: {loadError}</div>
            )}
            {!loadError && (
              <div className="schema-editor-textwrap">
                <pre className="schema-editor-gutter" aria-hidden="true">
                  {gutter}
                </pre>
                <textarea
                  ref={textareaRef}
                  className="schema-editor-textarea"
                  spellCheck={false}
                  value={state?.body ?? ""}
                  readOnly={readOnly || loading}
                  onChange={(e) => {
                    if (saveError) setSaveError(null);
                    setState((prev) =>
                      prev ? { ...prev, body: e.target.value } : prev,
                    );
                  }}
                  placeholder={
                    loading ? "Loading…" : readOnly ? "(read-only)" : "{}"
                  }
                />
              </div>
            )}
            <ParseStatus
              parse={parse}
              dirty={dirty}
              readOnly={readOnly}
              saveError={saveError}
            />
          </div>
          <aside className="schema-editor-hint">
            <h3>Expected shape</h3>
            <pre>{selected.hint}</pre>
            {readOnly && (
              <p className="schema-editor-readonly-note">
                This file is shown for reference — it's read-only in this
                build, so saves are disabled.
              </p>
            )}
          </aside>
        </div>
      </div>
    </div>
  );
}

function ParseStatus({
  parse,
  dirty,
  readOnly,
  saveError,
}: {
  parse: ReturnType<typeof parseJSON> | null;
  dirty: boolean;
  readOnly: boolean;
  saveError: string | null;
}) {
  // A backend validation failure (e.g. invalid TOML) takes precedence — show
  // the precise reason inline using the same "bad" styling as JSON errors.
  if (saveError) {
    return (
      <div className="schema-editor-status schema-editor-status-bad">
        {saveError}
      </div>
    );
  }
  if (parse === null) {
    return (
      <div className="schema-editor-status schema-editor-status-info">
        {readOnly
          ? "Read-only"
          : "Non-JSON file — validated on save"}
      </div>
    );
  }
  if (parse.ok) {
    return (
      <div className="schema-editor-status schema-editor-status-ok">
        Valid JSON{dirty ? " — unsaved changes" : ""}
      </div>
    );
  }
  const err = parse.error as ParseError;
  return (
    <div className="schema-editor-status schema-editor-status-bad">
      Line {err.line}, col {err.column}: {err.message}
    </div>
  );
}

let activeRoot: Root | null = null;

/**
 * Imperative summoner for the schema editor modal. `presetId` matches a
 * `ConfigPreset.id` (e.g. `"snippets"`) — pass it through from the slash
 * command so `/edit-config snippets` opens straight to that file. Unknown
 * ids fall back to the first preset.
 */
export function openSchemaEditor(presetId?: string): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "schema-editor";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) activeRoot = null;
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<SchemaEditor onClose={close} initialPresetId={presetId} />);
}
