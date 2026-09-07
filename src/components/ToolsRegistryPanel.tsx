import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { PanelLoading } from "./Skeleton";
import {
  INPUT_KINDS,
  TOOL_METHODS,
  coerceArg,
  deleteTool,
  invokeTool,
  isValidToolName,
  listTools,
  makeEmptyTool,
  saveTool,
  testTool,
  type ResponseFormat,
  type ToolDef,
  type ToolInput,
  type ToolInvocationResult,
  type ToolMethod,
} from "@/lib/tools";
import { confirmDialog } from "@/lib/dialogs";
import { pushToast } from "@/lib/toast";

/**
 * REST→MCP tool virtualizer registry UI (ContextForge #12).
 *
 * Three modes share one component:
 *  - list    → roster of saved tools with quick-action buttons
 *  - editor  → form for create / edit, with a "Test" button
 *  - invoke  → minimal arg-input view fired from the "Invoke" button
 *
 * Kept inside a single component so the panel can swap modes without
 * unmounting (which would lose unsaved form state on a misclick).
 */

type Mode =
  | { kind: "list" }
  | { kind: "editor"; draft: ToolDef; original: string | null }
  | { kind: "invoke"; tool: ToolDef };

export function ToolsRegistryPanel() {
  const [tools, setTools] = useState<ToolDef[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>({ kind: "list" });

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setTools(await listTools());
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const onNew = () =>
    setMode({ kind: "editor", draft: makeEmptyTool(), original: null });

  const onEdit = (tool: ToolDef) =>
    setMode({ kind: "editor", draft: { ...tool }, original: tool.name });

  const onInvoke = (tool: ToolDef) => setMode({ kind: "invoke", tool });

  const onDelete = async (tool: ToolDef) => {
    if (!(await confirmDialog({
      title: "Delete tool?",
      message: `Delete tool "${tool.name}"?`,
      confirmLabel: "Delete",
      danger: true,
    }))) return;
    try {
      await deleteTool(tool.name);
      pushToast({ title: "Tool deleted", body: tool.name, kind: "success" });
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    }
  };

  const onSaved = async () => {
    await refresh();
    setMode({ kind: "list" });
  };

  return (
    <div className="tools-panel">
      <header className="tools-panel-head">
        <div>
          <strong>Tools registry</strong>
          <span className="muted" style={{ marginLeft: 8 }}>
            REST endpoints exposed to agents
          </span>
        </div>
        {mode.kind === "list" ? (
          <button className="tools-primary" onClick={onNew}>
            + New tool
          </button>
        ) : (
          <button className="link-btn" onClick={() => setMode({ kind: "list" })}>
            ← back
          </button>
        )}
      </header>

      {error && <div className="tools-error">{error}</div>}

      {mode.kind === "list" && (
        <ToolsList
          tools={tools}
          loading={loading}
          hasError={!!error}
          onInvoke={onInvoke}
          onEdit={onEdit}
          onDelete={onDelete}
        />
      )}
      {mode.kind === "editor" && (
        <ToolEditor
          draft={mode.draft}
          original={mode.original}
          onChange={(d) => setMode({ kind: "editor", draft: d, original: mode.original })}
          onSaved={onSaved}
          onError={setError}
        />
      )}
      {mode.kind === "invoke" && (
        <ToolInvokeForm
          tool={mode.tool}
          onClose={() => setMode({ kind: "list" })}
          onError={setError}
        />
      )}
    </div>
  );
}

// ── List ────────────────────────────────────────────────────────────────────

function ToolsList(props: {
  tools: ToolDef[];
  loading: boolean;
  hasError: boolean;
  onInvoke: (t: ToolDef) => void;
  onEdit: (t: ToolDef) => void;
  onDelete: (t: ToolDef) => void;
}) {
  if (props.loading && props.tools.length === 0) {
    return <PanelLoading label="Loading tools" />;
  }
  if (props.tools.length === 0) {
    // A failed load already shows the error box above — don't also invite the
    // user to create a tool against a backend that isn't responding.
    if (props.hasError) return null;
    return (
      <div className="tools-empty">
        No tools yet. Click <strong>+ New tool</strong> to define one.
      </div>
    );
  }
  return (
    <ul className="tools-list">
      {props.tools.map((t) => (
        <li key={t.name} className="tools-row">
          <div className="tools-row-main">
            <div className="tools-row-head">
              <strong>{t.name}</strong>
              <span className={`tools-method tools-method-${t.method.toLowerCase()}`}>
                {t.method}
              </span>
            </div>
            <code className="tools-row-url">{t.url_template}</code>
            {t.description && <div className="tools-row-desc">{t.description}</div>}
          </div>
          <div className="tools-row-actions">
            <button onClick={() => props.onInvoke(t)}>Invoke</button>
            <button onClick={() => props.onEdit(t)}>Edit</button>
            <button className="tools-danger" onClick={() => props.onDelete(t)}>
              Delete
            </button>
          </div>
        </li>
      ))}
    </ul>
  );
}

// ── Editor ──────────────────────────────────────────────────────────────────

function ToolEditor(props: {
  draft: ToolDef;
  original: string | null;
  onChange: (d: ToolDef) => void;
  onSaved: () => void | Promise<void>;
  onError: (e: string | null) => void;
}) {
  const { draft, onChange } = props;
  const [busy, setBusy] = useState(false);
  const [testArgs, setTestArgs] = useState<Record<string, string>>({});
  const [testResult, setTestResult] = useState<ToolInvocationResult | null>(null);

  const set = <K extends keyof ToolDef>(key: K, value: ToolDef[K]) =>
    onChange({ ...draft, [key]: value });

  const onSave = async () => {
    if (!isValidToolName(draft.name)) {
      props.onError("Name must be 1-64 chars: letters, digits, '-', '_', '.'");
      return;
    }
    setBusy(true);
    props.onError(null);
    try {
      await saveTool(draft);
      pushToast({ title: "Tool saved", body: draft.name, kind: "success" });
      await props.onSaved();
    } catch (e) {
      props.onError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const onTest = async () => {
    if (!isValidToolName(draft.name)) {
      props.onError("Save the tool first (need a valid name).");
      return;
    }
    setBusy(true);
    props.onError(null);
    try {
      // Persist before testing so the backend always reads the latest def
      // from disk. Saves us having to mirror substitute_template in TS.
      await saveTool(draft);
      const coerced: Record<string, unknown> = {};
      for (const input of draft.inputs) {
        const raw = testArgs[input.name];
        if (raw !== undefined && raw !== "") {
          coerced[input.name] = coerceArg(input, raw);
        }
      }
      const result = await testTool(draft.name, coerced);
      setTestResult(result);
    } catch (e) {
      props.onError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="tools-editor">
      <div className="tools-field-row">
        <label className="tools-field">
          Name
          <input
            value={draft.name}
            onChange={(e) => set("name", e.target.value)}
            placeholder="get-weather"
            disabled={busy || props.original !== null}
          />
        </label>
        <label className="tools-field">
          Method
          <select
            value={draft.method}
            onChange={(e) => set("method", e.target.value as ToolMethod)}
            disabled={busy}
          >
            {TOOL_METHODS.map((m) => (
              <option key={m} value={m}>{m}</option>
            ))}
          </select>
        </label>
        <label className="tools-field">
          Response
          <select
            value={draft.response_format}
            onChange={(e) =>
              set("response_format", e.target.value as ResponseFormat)
            }
            disabled={busy}
          >
            <option value="json">json</option>
            <option value="text">text</option>
          </select>
        </label>
      </div>

      <label className="tools-field">
        Description
        <input
          value={draft.description}
          onChange={(e) => set("description", e.target.value)}
          placeholder="What does this tool do? (shown to the LLM)"
          maxLength={200}
          disabled={busy}
        />
      </label>

      <label className="tools-field">
        URL template
        <input
          value={draft.url_template}
          onChange={(e) => set("url_template", e.target.value)}
          placeholder="https://api.example.com/users/{id}"
          disabled={busy}
        />
      </label>

      <InputsEditor
        inputs={draft.inputs}
        onChange={(inputs) => set("inputs", inputs)}
        disabled={busy}
      />

      <HeadersEditor
        headers={draft.headers}
        onChange={(headers) => set("headers", headers)}
        disabled={busy}
      />

      <details className="tools-test">
        <summary>Test invocation</summary>
        <div className="tools-test-body">
          {draft.inputs.length === 0 ? (
            <div className="muted" style={{ fontSize: 11.5 }}>
              No inputs declared — Test will fire with no args.
            </div>
          ) : (
            draft.inputs.map((input) => (
              <label key={input.name} className="tools-field">
                {input.name}{" "}
                <span className="muted">
                  ({input.kind}
                  {input.required ? ", required" : ""})
                </span>
                <input
                  value={testArgs[input.name] ?? ""}
                  onChange={(e) =>
                    setTestArgs((s) => ({ ...s, [input.name]: e.target.value }))
                  }
                  placeholder={input.description}
                  disabled={busy}
                />
              </label>
            ))
          )}
          <button onClick={onTest} disabled={busy}>
            {busy ? "Testing…" : "Test"}
          </button>
          {testResult && <InvocationResultView result={testResult} />}
        </div>
      </details>

      <div className="tools-editor-actions">
        <button className="tools-primary" onClick={onSave} disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </button>
      </div>
    </div>
  );
}

// ── Inputs sub-editor ───────────────────────────────────────────────────────

function InputsEditor(props: {
  inputs: ToolInput[];
  onChange: (inputs: ToolInput[]) => void;
  disabled: boolean;
}) {
  const update = (idx: number, patch: Partial<ToolInput>) => {
    props.onChange(
      props.inputs.map((i, k) => (k === idx ? { ...i, ...patch } : i)),
    );
  };
  const add = () =>
    props.onChange([
      ...props.inputs,
      { name: "", kind: "string", required: false, description: "" },
    ]);
  const remove = (idx: number) =>
    props.onChange(props.inputs.filter((_, k) => k !== idx));

  return (
    <fieldset className="tools-fieldset">
      <legend>
        Inputs
        <button
          className="link-btn tools-add"
          onClick={add}
          disabled={props.disabled}
        >
          + add
        </button>
      </legend>
      {props.inputs.length === 0 && (
        <div className="muted" style={{ fontSize: 11.5 }}>
          No inputs — the URL template won't have any <code>{"{param}"}</code> placeholders.
        </div>
      )}
      {props.inputs.map((input, idx) => (
        <div key={idx} className="tools-input-row">
          <input
            placeholder="name"
            value={input.name}
            onChange={(e) => update(idx, { name: e.target.value })}
            disabled={props.disabled}
          />
          <select
            value={input.kind}
            onChange={(e) =>
              update(idx, { kind: e.target.value as ToolInput["kind"] })
            }
            disabled={props.disabled}
          >
            {INPUT_KINDS.map((k) => (
              <option key={k} value={k}>{k}</option>
            ))}
          </select>
          <label className="tools-required">
            <input
              type="checkbox"
              checked={input.required}
              onChange={(e) => update(idx, { required: e.target.checked })}
              disabled={props.disabled}
            />
            required
          </label>
          <input
            placeholder="description"
            value={input.description}
            onChange={(e) => update(idx, { description: e.target.value })}
            disabled={props.disabled}
          />
          <button
            className="tools-danger"
            onClick={() => remove(idx)}
            disabled={props.disabled}
          >
            ×
          </button>
        </div>
      ))}
    </fieldset>
  );
}

// ── Headers sub-editor ──────────────────────────────────────────────────────

function HeadersEditor(props: {
  headers: Record<string, string>;
  onChange: (headers: Record<string, string>) => void;
  disabled: boolean;
}) {
  const entries = Object.entries(props.headers);
  const update = (oldKey: string, key: string, value: string) => {
    const next: Record<string, string> = {};
    for (const [k, v] of entries) {
      if (k === oldKey) {
        if (key.trim() !== "") next[key] = value;
      } else {
        next[k] = v;
      }
    }
    props.onChange(next);
  };
  const add = () => {
    // Find a non-clashing placeholder name so the row gains a stable key.
    let i = 0;
    let newKey = "X-Header";
    while (newKey in props.headers) {
      i += 1;
      newKey = `X-Header-${i}`;
    }
    props.onChange({ ...props.headers, [newKey]: "" });
  };
  const remove = (key: string) => {
    const next = { ...props.headers };
    delete next[key];
    props.onChange(next);
  };

  return (
    <fieldset className="tools-fieldset">
      <legend>
        Headers
        <button
          className="link-btn tools-add"
          onClick={add}
          disabled={props.disabled}
        >
          + add
        </button>
        <span className="muted" style={{ marginLeft: 8, fontSize: 11 }}>
          values may include <code>{"{secret:provider/label}"}</code>
        </span>
      </legend>
      {entries.length === 0 && (
        <div className="muted" style={{ fontSize: 11.5 }}>
          No headers.
        </div>
      )}
      {entries.map(([k, v]) => (
        <div key={k} className="tools-header-row">
          <input
            placeholder="Header-Name"
            value={k}
            onChange={(e) => update(k, e.target.value, v)}
            disabled={props.disabled}
          />
          <input
            placeholder="value"
            value={v}
            onChange={(e) => update(k, k, e.target.value)}
            disabled={props.disabled}
          />
          <button
            className="tools-danger"
            onClick={() => remove(k)}
            disabled={props.disabled}
          >
            ×
          </button>
        </div>
      ))}
    </fieldset>
  );
}

// ── Invoke modal ────────────────────────────────────────────────────────────

function ToolInvokeForm(props: {
  tool: ToolDef;
  onClose: () => void;
  onError: (e: string | null) => void;
}) {
  const [args, setArgs] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ToolInvocationResult | null>(null);

  const fire = async () => {
    setBusy(true);
    props.onError(null);
    try {
      const coerced: Record<string, unknown> = {};
      for (const input of props.tool.inputs) {
        const raw = args[input.name];
        if (raw !== undefined && raw !== "") {
          coerced[input.name] = coerceArg(input, raw);
        }
      }
      const r = await invokeTool(props.tool.name, coerced);
      setResult(r);
    } catch (e) {
      props.onError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="tools-invoke">
      <div className="tools-invoke-head">
        <strong>{props.tool.name}</strong>
        <span className={`tools-method tools-method-${props.tool.method.toLowerCase()}`}>
          {props.tool.method}
        </span>
        <code className="tools-row-url">{props.tool.url_template}</code>
      </div>
      {props.tool.description && (
        <div className="tools-row-desc">{props.tool.description}</div>
      )}
      <div className="tools-invoke-body">
        {props.tool.inputs.length === 0 && (
          <div className="muted" style={{ fontSize: 11.5 }}>
            No inputs declared — fire as-is.
          </div>
        )}
        {props.tool.inputs.map((input) => (
          <label key={input.name} className="tools-field">
            {input.name}{" "}
            <span className="muted">
              ({input.kind}
              {input.required ? ", required" : ""})
            </span>
            <input
              value={args[input.name] ?? ""}
              onChange={(e) =>
                setArgs((s) => ({ ...s, [input.name]: e.target.value }))
              }
              placeholder={input.description}
              disabled={busy}
            />
          </label>
        ))}
        <div className="tools-editor-actions">
          <button onClick={props.onClose} disabled={busy}>
            Close
          </button>
          <button className="tools-primary" onClick={fire} disabled={busy}>
            {busy ? "Calling…" : "Invoke"}
          </button>
        </div>
        {result && <InvocationResultView result={result} />}
      </div>
    </div>
  );
}

// ── Result view ─────────────────────────────────────────────────────────────

function InvocationResultView({ result }: { result: ToolInvocationResult }) {
  const statusClass = result.ok ? "tools-ok" : "tools-fail";
  return (
    <div className={`tools-result ${statusClass}`}>
      <div className="tools-result-head">
        <span>
          {result.ok ? "OK" : "FAIL"}
          {result.status !== null && ` · http ${result.status}`}
          {` · ${result.latency_ms}ms`}
          {result.truncated && " · truncated"}
        </span>
        {result.error && <span className="tools-result-err">{result.error}</span>}
      </div>
      <pre className="tools-result-body">{result.body || "(empty response)"}</pre>
    </div>
  );
}
