import { useCallback, useEffect, useState } from "react";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  KNOWN_PROVIDERS,
  formatAddedAt,
  vaultGet,
  vaultList,
  vaultRemove,
  vaultSet,
  type KeyMetadata,
} from "@/lib/keyvault";
import { confirmDialog } from "@/lib/dialogs";
import { pushToast } from "@/lib/toast";

/**
 * UI for the encrypted provider key vault.
 *
 * The "Panel" name reflects the original spec (an ActivityPanel tab), but
 * because ActivityPanel.tsx is off-limits in this change set, we render the
 * same component as a self-mounting modal. `/vault` summons it via
 * `openKeyVaultPanel()`; the inner panel layout is reusable as a tab later
 * with no logic changes.
 */

interface KeyVaultPanelProps {
  onClose: () => void;
}

interface FormState {
  provider: string;
  label: string;
  key: string;
}

const EMPTY_FORM: FormState = { provider: "anthropic", label: "personal", key: "" };

function maskedPreview(s: string): string {
  if (s.length <= 8) return "•".repeat(s.length);
  return `${s.slice(0, 4)}…${s.slice(-4)}`;
}

export function KeyVaultPanel({ onClose }: KeyVaultPanelProps) {
  const [items, setItems] = useState<KeyMetadata[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [revealed, setRevealed] = useState<Record<string, string>>({});
  const [busyRow, setBusyRow] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setItems(await vaultList());
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
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const rowKey = (m: KeyMetadata) => `${m.provider}::${m.label}`;

  const onSave = useCallback(async () => {
    const provider = form.provider.trim();
    const label = form.label.trim();
    const key = form.key.trim();
    if (!provider || !label || !key) {
      setError("provider, label, and key are required");
      return;
    }
    setBusyRow("__form");
    setError(null);
    try {
      await vaultSet(provider, label, key);
      pushToast({ title: "Key saved", body: `${provider}/${label}`, kind: "success" });
      setForm({ ...EMPTY_FORM, provider });
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyRow(null);
    }
  }, [form, refresh]);

  const onReveal = useCallback(async (m: KeyMetadata) => {
    const k = rowKey(m);
    if (revealed[k]) {
      // Toggle off — clear from memory so it doesn't linger on screen.
      setRevealed((prev) => {
        const next = { ...prev };
        delete next[k];
        return next;
      });
      return;
    }
    setBusyRow(k);
    try {
      const value = await vaultGet(m.provider, m.label);
      setRevealed((prev) => ({ ...prev, [k]: value }));
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyRow(null);
    }
  }, [revealed]);

  const onCopy = useCallback(async (m: KeyMetadata) => {
    setBusyRow(rowKey(m));
    try {
      const value = await vaultGet(m.provider, m.label);
      await navigator.clipboard.writeText(value);
      pushToast({ title: "Copied", body: `${m.provider}/${m.label} key`, kind: "success" });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyRow(null);
    }
  }, []);

  const onRemove = useCallback(async (m: KeyMetadata) => {
    if (!(await confirmDialog({
      title: "Remove key?",
      message: `Remove ${m.provider}/${m.label}?`,
      confirmLabel: "Remove",
      danger: true,
    }))) return;
    setBusyRow(rowKey(m));
    try {
      await vaultRemove(m.provider, m.label);
      pushToast({ title: "Key removed", body: `${m.provider}/${m.label}`, kind: "success" });
      await refresh();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusyRow(null);
    }
  }, [refresh]);

  return (
    <div className="keyvault-backdrop" onMouseDown={onClose}>
      <div
        className="keyvault-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="keyvault-title"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="keyvault-header">
          <h2 id="keyvault-title">Provider Key Vault</h2>
          <button className="keyvault-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <p className="keyvault-summary">
          AES-256-GCM, master key in OS keychain. Stored at <code>~/.cortex/keys.enc</code>.
        </p>

        <section className="keyvault-add">
          <h3>Add or update</h3>
          <div className="keyvault-form-row">
            <label>
              Provider
              <input
                list="keyvault-provider-list"
                value={form.provider}
                onChange={(e) => setForm((f) => ({ ...f, provider: e.target.value }))}
                disabled={busyRow !== null}
              />
              <datalist id="keyvault-provider-list">
                {KNOWN_PROVIDERS.map((p) => (
                  <option key={p} value={p} />
                ))}
              </datalist>
            </label>
            <label>
              Label
              <input
                value={form.label}
                onChange={(e) => setForm((f) => ({ ...f, label: e.target.value }))}
                placeholder="personal"
                disabled={busyRow !== null}
              />
            </label>
            <label className="keyvault-key-input">
              Key
              <input
                type="password"
                value={form.key}
                onChange={(e) => setForm((f) => ({ ...f, key: e.target.value }))}
                placeholder="sk-…"
                disabled={busyRow !== null}
              />
            </label>
            <button
              className="keyvault-primary"
              onClick={onSave}
              disabled={busyRow !== null}
            >
              {busyRow === "__form" ? "Saving…" : "Save"}
            </button>
          </div>
        </section>

        <section className="keyvault-list-section">
          <h3>Stored keys ({items.length})</h3>
          {loading && items.length === 0 && <PanelLoading label="Loading keys" />}
          {!loading && !error && items.length === 0 && (
            <div className="keyvault-empty">No keys stored yet.</div>
          )}
          <ul className="keyvault-list">
            {items.map((m) => {
              const k = rowKey(m);
              const shown = revealed[k];
              const busy = busyRow === k;
              return (
                <li key={k} className="keyvault-item">
                  <div className="keyvault-item-meta">
                    <strong>{m.provider}</strong>
                    <span className="keyvault-item-label">{m.label}</span>
                    <span className="keyvault-item-ts">{formatAddedAt(m.added_unix_ms)}</span>
                  </div>
                  <code className="keyvault-item-key">
                    {shown ? shown : maskedPreview(`${m.provider}-${m.label}`)}
                  </code>
                  <div className="keyvault-item-actions">
                    <button onClick={() => onReveal(m)} disabled={busy}>
                      {shown ? "Hide" : "Reveal"}
                    </button>
                    <button onClick={() => onCopy(m)} disabled={busy}>
                      Copy
                    </button>
                    <button
                      onClick={() => onRemove(m)}
                      disabled={busy}
                      className="keyvault-danger"
                    >
                      Remove
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        </section>

        {error && <div className="keyvault-error">{error}</div>}
      </div>
    </div>
  );
}

/**
 * Imperative summoner. Mirrors `openIDEExportModal` — a detached React root
 * on document.body, torn down when closed. Keeps App.tsx untouched.
 */
let activeRoot: Root | null = null;

export function openKeyVaultPanel(): void {
  if (activeRoot) return;
  const container = document.createElement("div");
  container.dataset.cortexMount = "keyvault";
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
  root.render(<KeyVaultPanel onClose={close} />);
}
