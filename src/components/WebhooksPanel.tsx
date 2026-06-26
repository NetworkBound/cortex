import { useCallback, useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  KNOWN_EVENTS,
  addWebhook,
  deleteWebhook,
  listWebhooks,
  testWebhook,
  updateWebhook,
  type TestResult,
  type Webhook,
  type WebhookInput,
} from "@/lib/webhooks";
import { confirmDialog } from "@/lib/dialogs";
import { pushToast } from "@/lib/toast";

/**
 * Manage outbound webhooks (ContextForge #14). Self-mounting modal, same
 * portal pattern as IDEExportModal / KeyVaultPanel — no App.tsx wiring.
 *
 * Editing UX: clicking a row opens an inline drawer pre-filled with that
 * webhook. Saving an existing row calls `update_webhook`; saving with
 * `editingId === null` calls `add_webhook`. Header KV pairs are stored as a
 * newline-separated `Header-Name: value` textarea — we don't need a full grid
 * editor for the typical "one or two tokens" case.
 */

interface WebhooksPanelProps {
  onClose: () => void;
}

interface FormState {
  label: string;
  url: string;
  events: string[];
  headersText: string;
  enabled: boolean;
}

const EMPTY_FORM: FormState = {
  label: "",
  url: "",
  events: [],
  headersText: "",
  enabled: true,
};

function headersToText(headers: Record<string, string>): string {
  return Object.entries(headers)
    .map(([k, v]) => `${k}: ${v}`)
    .join("\n");
}

/** Parse a `Header-Name: value` textarea. Silently drops lines that don't
 * match — the user can see what stuck in the resulting webhook row. */
function parseHeaders(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const idx = trimmed.indexOf(":");
    if (idx <= 0) continue;
    const name = trimmed.slice(0, idx).trim();
    const value = trimmed.slice(idx + 1).trim();
    if (name) out[name] = value;
  }
  return out;
}

function describeTest(r: TestResult): string {
  const lat = `${r.latency_ms}ms`;
  if (r.ok) return `OK (HTTP ${r.status ?? "?"}) · ${lat}`;
  if (r.status != null) return `HTTP ${r.status} · ${lat}${r.error ? ` · ${r.error}` : ""}`;
  return `failed · ${lat}${r.error ? ` · ${r.error}` : ""}`;
}

export function WebhooksPanel({ onClose }: WebhooksPanelProps) {
  const [items, setItems] = useState<Webhook[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null | "__new">(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [testResults, setTestResults] = useState<Record<string, TestResult>>({});

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setItems(await listWebhooks());
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (editingId !== null) setEditingId(null);
        else onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editingId, onClose]);

  const startNew = useCallback(() => {
    setForm(EMPTY_FORM);
    setEditingId("__new");
  }, []);

  const startEdit = useCallback((w: Webhook) => {
    setForm({
      label: w.label,
      url: w.url,
      events: [...w.events],
      headersText: headersToText(w.headers),
      enabled: w.enabled,
    });
    setEditingId(w.id);
  }, []);

  const onSave = useCallback(async () => {
    const label = form.label.trim();
    const url = form.url.trim();
    if (!label || !url) {
      setError("label and url are required");
      return;
    }
    const payload: WebhookInput = {
      label,
      url,
      events: form.events,
      headers: parseHeaders(form.headersText),
      enabled: form.enabled,
    };
    setBusyId(editingId ?? "__new");
    setError(null);
    try {
      if (editingId && editingId !== "__new") {
        await updateWebhook({ ...payload, id: editingId });
        pushToast({ title: "Webhook updated", body: label, kind: "success" });
      } else {
        await addWebhook(payload);
        pushToast({ title: "Webhook added", body: label, kind: "success" });
      }
      setEditingId(null);
      setForm(EMPTY_FORM);
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyId(null);
    }
  }, [editingId, form, refresh]);

  const onDelete = useCallback(
    async (w: Webhook) => {
      if (!(await confirmDialog({
        title: "Delete webhook?",
        message: `Delete webhook "${w.label}"?`,
        confirmLabel: "Delete",
        danger: true,
      }))) return;
      setBusyId(w.id);
      try {
        await deleteWebhook(w.id);
        pushToast({ title: "Webhook deleted", body: w.label, kind: "success" });
        if (editingId === w.id) setEditingId(null);
        await refresh();
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setBusyId(null);
      }
    },
    [editingId, refresh],
  );

  const onTest = useCallback(async (w: Webhook) => {
    setBusyId(w.id);
    setError(null);
    try {
      const res = await testWebhook(w.id);
      setTestResults((prev) => ({ ...prev, [w.id]: res }));
      pushToast({
        title: res.ok ? "Webhook OK" : "Webhook failed",
        body: `${w.label}: ${describeTest(res)}`,
        kind: res.ok ? "success" : "error",
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyId(null);
    }
  }, []);

  const onToggleEnabled = useCallback(
    async (w: Webhook) => {
      setBusyId(w.id);
      try {
        await updateWebhook({ ...w, enabled: !w.enabled });
        await refresh();
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setBusyId(null);
      }
    },
    [refresh],
  );

  const toggleEvent = useCallback((event: string) => {
    setForm((f) =>
      f.events.includes(event)
        ? { ...f, events: f.events.filter((e) => e !== event) }
        : { ...f, events: [...f.events, event] },
    );
  }, []);

  /** Custom event names the user typed that aren't in KNOWN_EVENTS — surfaced
   * as additional checkboxes so they stay editable. */
  const customEvents = useMemo(() => {
    const known = new Set(KNOWN_EVENTS);
    return form.events.filter((e) => !known.has(e));
  }, [form.events]);

  return (
    <div className="webhooks-backdrop" onMouseDown={onClose}>
      <div
        className="webhooks-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="webhooks-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="webhooks-header">
          <h2 id="webhooks-title">Webhooks</h2>
          <button className="webhooks-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="webhooks-summary">
          Outbound POSTs on selected events. Stored at <code>~/.cortex/webhooks.json</code>. Errors
          are logged but never block the agent.
        </p>

        <section className="webhooks-toolbar">
          <button className="webhooks-primary" onClick={startNew} disabled={editingId !== null}>
            + Add webhook
          </button>
          <span className="webhooks-count">{items.length} configured</span>
        </section>

        {loading && <div className="webhooks-loading">Loading…</div>}

        {!loading && items.length === 0 && editingId === null && (
          <div className="webhooks-empty">No webhooks yet.</div>
        )}

        <ul className="webhooks-list">
          {items.map((w) => {
            const busy = busyId === w.id;
            const isEditing = editingId === w.id;
            const last = testResults[w.id];
            return (
              <li key={w.id} className={`webhooks-item ${isEditing ? "webhooks-item-editing" : ""}`}>
                <div className="webhooks-item-row">
                  <label className="webhooks-toggle" title={w.enabled ? "Disable" : "Enable"}>
                    <input
                      type="checkbox"
                      checked={w.enabled}
                      disabled={busy}
                      onChange={() => onToggleEnabled(w)}
                    />
                  </label>
                  <div className="webhooks-item-meta">
                    <strong>{w.label}</strong>
                    <code className="webhooks-item-url">{w.url}</code>
                    <span className="webhooks-item-events">
                      {w.events.length === 0 ? (
                        <em>no events</em>
                      ) : (
                        w.events.join(", ")
                      )}
                    </span>
                    {last && (
                      <span
                        className={`webhooks-item-test ${last.ok ? "webhooks-test-ok" : "webhooks-test-fail"}`}
                      >
                        {describeTest(last)}
                      </span>
                    )}
                  </div>
                  <div className="webhooks-item-actions">
                    <button onClick={() => onTest(w)} disabled={busy}>
                      Test
                    </button>
                    <button onClick={() => (isEditing ? setEditingId(null) : startEdit(w))} disabled={busy}>
                      {isEditing ? "Cancel" : "Edit"}
                    </button>
                    <button
                      onClick={() => onDelete(w)}
                      disabled={busy}
                      className="webhooks-danger"
                    >
                      Delete
                    </button>
                  </div>
                </div>
              </li>
            );
          })}
        </ul>

        {editingId !== null && (
          <section className="webhooks-edit">
            <h3>{editingId === "__new" ? "Add webhook" : "Edit webhook"}</h3>
            <label className="webhooks-field">
              Label
              <input
                value={form.label}
                onChange={(e) => setForm((f) => ({ ...f, label: e.target.value }))}
                placeholder="CI on memory snapshot"
                disabled={busyId !== null}
              />
            </label>
            <label className="webhooks-field">
              URL
              <input
                value={form.url}
                onChange={(e) => setForm((f) => ({ ...f, url: e.target.value }))}
                placeholder="https://hooks.example.com/..."
                disabled={busyId !== null}
              />
            </label>
            <fieldset className="webhooks-events">
              <legend>Events</legend>
              {KNOWN_EVENTS.map((ev) => (
                <label key={ev} className="webhooks-event-row">
                  <input
                    type="checkbox"
                    checked={form.events.includes(ev)}
                    onChange={() => toggleEvent(ev)}
                    disabled={busyId !== null}
                  />
                  <code>{ev}</code>
                </label>
              ))}
              {customEvents.map((ev) => (
                <label key={ev} className="webhooks-event-row">
                  <input
                    type="checkbox"
                    checked
                    onChange={() => toggleEvent(ev)}
                    disabled={busyId !== null}
                  />
                  <code>{ev}</code>
                  <span className="webhooks-custom-tag">custom</span>
                </label>
              ))}
              <input
                className="webhooks-custom-input"
                type="text"
                placeholder="Add custom event…  e.g. my.custom.event"
                disabled={busyId !== null}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    const v = (e.target as HTMLInputElement).value.trim();
                    if (v && !form.events.includes(v)) {
                      setForm((f) => ({ ...f, events: [...f.events, v] }));
                    }
                    (e.target as HTMLInputElement).value = "";
                    e.preventDefault();
                  }
                }}
              />
            </fieldset>
            <label className="webhooks-field">
              Headers (one per line — <code>Name: value</code>)
              <textarea
                value={form.headersText}
                rows={3}
                onChange={(e) => setForm((f) => ({ ...f, headersText: e.target.value }))}
                placeholder={"X-Token: abc\nX-Source: cortex"}
                disabled={busyId !== null}
              />
            </label>
            <label className="webhooks-field webhooks-field-inline">
              <input
                type="checkbox"
                checked={form.enabled}
                onChange={(e) => setForm((f) => ({ ...f, enabled: e.target.checked }))}
                disabled={busyId !== null}
              />
              Enabled
            </label>
            <div className="webhooks-edit-actions">
              <button onClick={() => setEditingId(null)} disabled={busyId !== null}>
                Cancel
              </button>
              <button className="webhooks-primary" onClick={onSave} disabled={busyId !== null}>
                {busyId !== null ? "Saving…" : "Save"}
              </button>
            </div>
          </section>
        )}

        {error && <div className="webhooks-error">{error}</div>}
      </div>
    </div>
  );
}

/**
 * Imperative summoner — same portal pattern as IDEExportModal /
 * KeyVaultPanel. Lets the `/webhook` slash command open the panel without
 * App.tsx wiring.
 */
let activeRoot: Root | null = null;

export function openWebhooksPanel(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "webhooks";
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
  root.render(<WebhooksPanel onClose={close} />);
}
