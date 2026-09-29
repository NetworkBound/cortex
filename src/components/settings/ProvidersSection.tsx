import { useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { vaultSet, vaultRemove } from "@/lib/keyvault";
import {
  getProviderConfig,
  listLocalCliProviders,
  listOpenAiCompatProviders,
  setProviderDefaultModel,
  setProviderKey,
  setRuntimeMode,
  validateProviderKey,
  type LocalCliProvider,
  type OpenAiCompatProvider,
  type ProviderConfig,
} from "@/lib/cortex-bridge";
import { CliLoginModal } from "../CliLoginModal";
import { SettingsSection, StatusPill, UnsavedBadge } from "./Section";

/** Per-provider metadata for the Providers tab. `staticModels` is the
 *  in-code current-model list feeding the default-model picker; a successful
 *  key validation merges the provider's live `GET /v1/models` result in.
 *  `builtinDefault` mirrors `DEFAULT_MODEL` in the matching direct adapter. */
const PROVIDER_META: {
  id: "anthropic" | "openai";
  label: string;
  keyPlaceholder: string;
  builtinDefault: string;
  staticModels: string[];
}[] = [
  {
    id: "anthropic",
    label: "Anthropic",
    keyPlaceholder: "sk-ant-…",
    builtinDefault: "claude-opus-4-8",
    staticModels: [
      "claude-opus-4-8",
      "claude-opus-4-7",
      "claude-opus-4-6",
      "claude-sonnet-4-6",
      "claude-haiku-4-5",
    ],
  },
  {
    id: "openai",
    label: "OpenAI",
    keyPlaceholder: "sk-…",
    builtinDefault: "gpt-4o",
    staticModels: [
      "gpt-4o",
      "gpt-4o-mini",
      "gpt-4.1",
      "gpt-4.1-mini",
      "o3",
      "o4-mini",
    ],
  },
];

type ValidationState =
  | { phase: "idle" }
  | { phase: "busy" }
  | { phase: "done"; ok: boolean; message: string };

/**
 * One provider's card: key entry + Validate (cheap live GET /v1/models
 * round-trip, inline outcome) + default-model picker. The picker persists to
 * the vault entry `(provider, "default-model")`, which the direct adapters
 * re-resolve on every run — so model changes apply immediately, no restart.
 */
function ProviderRow({
  meta,
  cfg,
  onConfigChange,
}: {
  meta: (typeof PROVIDER_META)[number];
  cfg: ProviderConfig | null;
  onConfigChange: () => Promise<void>;
}) {
  const [keyDraft, setKeyDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [validation, setValidation] = useState<ValidationState>({
    phase: "idle",
  });
  const [liveModels, setLiveModels] = useState<string[]>([]);
  const [err, setErr] = useState<string | null>(null);

  const keySet =
    meta.id === "anthropic" ? !!cfg?.anthropic_key_set : !!cfg?.openai_key_set;
  const defaultModel =
    (meta.id === "anthropic"
      ? cfg?.anthropic_default_model
      : cfg?.openai_default_model) ?? "";

  // Vault-write the typed key (if any) so Validate always checks what the
  // adapters will actually use. Shared by Save and Validate.
  async function saveDraftIfAny() {
    if (keyDraft.trim().length === 0) return;
    await setProviderKey(meta.id, keyDraft.trim());
    setKeyDraft("");
    await onConfigChange();
  }

  async function saveKey() {
    setSaving(true);
    setErr(null);
    try {
      await saveDraftIfAny();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setSaving(false);
    }
  }

  async function validate() {
    setValidation({ phase: "busy" });
    setErr(null);
    try {
      await saveDraftIfAny();
      const res = await validateProviderKey(meta.id);
      setValidation({ phase: "done", ok: res.ok, message: res.message });
      if (res.ok && res.models.length > 0) setLiveModels(res.models);
    } catch (e) {
      setValidation({ phase: "done", ok: false, message: humanizeError(e) });
    }
  }

  async function changeModel(model: string) {
    setErr(null);
    try {
      await setProviderDefaultModel(meta.id, model);
      await onConfigChange();
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  // Static list first, live list appended, and whatever is currently saved
  // kept selectable even if it appears in neither.
  const modelOptions = useMemo(() => {
    const merged = [...meta.staticModels];
    for (const m of liveModels) if (!merged.includes(m)) merged.push(m);
    if (defaultModel && !merged.includes(defaultModel))
      merged.unshift(defaultModel);
    return merged;
  }, [meta.staticModels, liveModels, defaultModel]);

  return (
    <div className="settings-stack tight spaced">
      <label>
        <span className="settings-row">
          {meta.label} API key{" "}
          {cfg && <StatusPill ok={keySet} okLabel="set" offLabel="not set" />}
        </span>
        <input
          type="password"
          value={keyDraft}
          onChange={(e) => setKeyDraft(e.target.value)}
          placeholder={
            keySet ? "leave blank to keep current" : meta.keyPlaceholder
          }
        />
      </label>
      <div className="settings-row wrap">
        <button
          type="button"
          onClick={() => void saveKey()}
          disabled={saving || keyDraft.trim().length === 0}
        >
          {saving ? "Saving…" : "Save key"}
        </button>
        <button
          type="button"
          onClick={() => void validate()}
          disabled={
            validation.phase === "busy" ||
            (!keySet && keyDraft.trim().length === 0)
          }
        >
          {validation.phase === "busy" ? "Validating…" : "Validate"}
        </button>
        <UnsavedBadge show={keyDraft.trim().length > 0} />
        {validation.phase === "done" && (
          <small
            className={`settings-validation ${validation.ok ? "ok" : "err"}`}
          >
            {validation.message}
          </small>
        )}
      </div>
      <label className="settings-field-row">
        <span className="settings-field-label">Default model</span>
        <select
          value={defaultModel}
          onChange={(e) => void changeModel(e.target.value)}
        >
          <option value="">Adapter default ({meta.builtinDefault})</option>
          {modelOptions.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </label>
      {liveModels.length === 0 && (
        <small className="settings-muted">
          Validate the key to merge {meta.label}'s live model list into this
          picker. Model changes apply on the next message — no restart needed.
        </small>
      )}
      {err && <div className="settings-err">{err}</div>}
    </div>
  );
}

/**
 * Settings → Providers. Lets the user store direct provider API keys
 * (Anthropic, OpenAI) in the OS key vault, validate them live, pick default
 * models, and switch the runtime mode (homelab gateway vs cloud direct) —
 * all in-app. Keys never round-trip back across the bridge — only their
 * presence does (`ProviderConfig`). The mode switch persists to
 * `~/.cortex/runtime-mode.json` and applies at the next launch (adapters
 * register once at startup); in the default (homelab) build the direct
 * adapters aren't compiled, so the toggle is disabled with a note.
 */
export function ProviderSettingsSection() {
  const [cfg, setCfg] = useState<ProviderConfig | null>(null);
  const [err, setErr] = useState<string | null>(null);

  async function refresh() {
    try {
      const c = await getProviderConfig();
      setCfg(c);
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function changeMode(mode: "homelab" | "cloud") {
    setErr(null);
    try {
      await setRuntimeMode(mode);
      await refresh();
      pushToast({
        title: "Provider mode saved",
        body: "Restart Cortex to apply the new mode — adapters are registered at startup.",
        kind: "info",
      });
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  return (
    <SettingsSection
      title="Providers"
      description={
        <>
          Sign in to model providers directly, bypassing the Cortex Gateway.
          Keys are stored encrypted in the OS key vault and never leave this
          machine.
          {cfg && !cfg.standalone_build && (
            <>
              {" "}
              <strong>Note:</strong> this build does not include the standalone
              (cloud) adapters — keys are saved and validated, but the direct
              providers only activate in a build compiled with the{" "}
              <code>standalone</code> feature.
            </>
          )}
        </>
      }
    >
      <div className="settings-stack tight spaced">
        <div className="settings-row">
          <span className="settings-label-sm">Build / mode</span>
          {cfg && (
            <>
              <StatusPill
                ok={cfg.standalone_build}
                okLabel="standalone"
                offLabel="gateway build"
              />
              <StatusPill
                ok={cfg.runtime_mode === "cloud"}
                okLabel="cloud mode"
                offLabel="gateway mode"
              />
            </>
          )}
        </div>
        <label className="settings-field-row">
          <span className="settings-field-label">Provider mode</span>
          <select
            value={cfg?.runtime_mode ?? "homelab"}
            disabled={!cfg?.standalone_build}
            onChange={(e) =>
              void changeMode(e.target.value as "homelab" | "cloud")
            }
          >
            <option value="homelab">
              Gateway — route through the Cortex Gateway
            </option>
            <option value="cloud">
              Cloud — direct provider APIs (keys below)
            </option>
          </select>
        </label>
        <small className="settings-muted">
          {cfg?.standalone_build
            ? "Saved in-app; takes effect on the next restart. The CORTEX_RUNTIME_MODE env var still works as a fallback when no in-app choice has been saved."
            : "Mode switching needs the standalone build — this build always routes through the gateway."}
        </small>
      </div>

      {PROVIDER_META.map((meta) => (
        <ProviderRow
          key={meta.id}
          meta={meta}
          cfg={cfg}
          onConfigChange={refresh}
        />
      ))}

      <div className="settings-row gap-top">
        <span className="settings-label-sm">Claude CLI login</span>
        {cfg && (
          <StatusPill
            ok={cfg.claude_cli_available}
            okLabel="available"
            offLabel="not installed"
          />
        )}
        <small className="settings-muted">
          {cfg?.claude_cli_available
            ? "The claude binary is on PATH and usable for the Claude CLI adapter."
            : "Install Claude Code and run `claude login` to enable the CLI adapter."}
        </small>
      </div>
      {err && <div className="settings-err">{err}</div>}
    </SettingsSection>
  );
}

/**
 * "Local AI providers" — every AI-maker CLI Cortex can drive locally (Claude,
 * OpenAI Codex, Gemini, Qwen, Grok, aider, Mistral Vibe). For each we show
 * install/sign-in status and a **Sign in** button that launches the CLI's own
 * login flow inside Cortex (a PTY terminal running e.g. `codex login`). Auth is
 * each CLI's own login — there is no key entry here. CLIs that authenticate via
 * env API keys (aider) show the key hint instead of a sign-in button.
 */
export function LocalCliProvidersSection() {
  const [rows, setRows] = useState<LocalCliProvider[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loginFor, setLoginFor] = useState<LocalCliProvider | null>(null);

  async function refresh() {
    try {
      setRows(await listLocalCliProviders());
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  return (
    <SettingsSection
      title="Local AI providers"
      description={
        <>
          Every major AI maker's CLI, driven locally — no gateway, no keys to
          paste. Each row spawns that CLI's own binary; auth is the CLI's own
          login. Install the ones you want, then click <strong>Sign in</strong>{" "}
          to complete the provider's login flow inside Cortex.
        </>
      }
    >
      <div className="settings-row spaced">
        <button type="button" onClick={() => void refresh()}>
          Refresh
        </button>
      </div>

      {rows?.map((p) => {
        const authKnown = p.authenticated !== null;
        return (
          <div key={p.id} className="settings-stack tight spaced">
            <div className="settings-row wrap">
              <span className="settings-row">
                {p.label}{" "}
                <StatusPill
                  ok={p.installed}
                  okLabel="installed"
                  offLabel="not found"
                />
                {p.installed && authKnown && (
                  <StatusPill
                    ok={!!p.authenticated}
                    okLabel="signed in"
                    offLabel="sign-in required"
                  />
                )}
              </span>
              {p.installed && p.has_login && (
                <button type="button" onClick={() => setLoginFor(p)}>
                  Sign in
                </button>
              )}
              {!p.installed && (
                <a
                  href={p.install_url}
                  target="_blank"
                  rel="noreferrer"
                  className="settings-link"
                >
                  Install
                </a>
              )}
            </div>
            <small className="settings-muted">
              {!p.installed
                ? p.install_hint
                : p.has_login
                  ? `Sign in runs: ${p.login_cmd}`
                  : "Authenticates via your provider API key env var — no in-app login."}
            </small>
          </div>
        );
      })}

      {err && <div className="settings-err">{err}</div>}

      {loginFor && (
        <CliLoginModal
          providerId={loginFor.id}
          providerLabel={loginFor.label}
          loginCmd={loginFor.login_cmd}
          onClose={() => {
            setLoginFor(null);
            void refresh();
          }}
        />
      )}
    </SettingsSection>
  );
}

/**
 * "API providers" — the 13 OpenAI-compatible, per-token providers
 * (`agents::openai_compat::PROVIDERS`). Unlike the CLI section above, these
 * authenticate with a pasted API key, not an in-app login flow. Each row
 * writes straight to the same encrypted vault
 * (`~/.cortex/keys.enc`, `<id>/api-key`) the adapter itself reads from —
 * this is a guided, per-provider-labeled alternative to typing the exact
 * provider id into the generic Provider Key Vault panel (`/vault`), not a
 * separate store.
 *
 * No live-validation round trip here (unlike the Anthropic/OpenAI direct
 * rows above, which do a cheap `GET /v1/models` check) — 13 different API
 * shapes each needing their own validation call is real, scoped follow-up
 * work, not bundled into this pass. A saved key's correctness surfaces the
 * first time it's actually used, same as it did before this section existed.
 */
export function OpenAiCompatProvidersSection() {
  const [rows, setRows] = useState<OpenAiCompatProvider[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [keyDrafts, setKeyDrafts] = useState<Record<string, string>>({});
  const [busyRow, setBusyRow] = useState<string | null>(null);

  async function refresh() {
    try {
      setRows(await listOpenAiCompatProviders());
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function saveKey(id: string) {
    const key = (keyDrafts[id] ?? "").trim();
    if (!key) return;
    setBusyRow(id);
    setErr(null);
    try {
      await vaultSet(id, "api-key", key);
      setKeyDrafts((d) => ({ ...d, [id]: "" }));
      pushToast({ title: "Key saved", body: id, kind: "success" });
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusyRow(null);
    }
  }

  async function removeKey(id: string) {
    setBusyRow(id);
    setErr(null);
    try {
      await vaultRemove(id, "api-key");
      pushToast({ title: "Key removed", body: id, kind: "success" });
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusyRow(null);
    }
  }

  return (
    <SettingsSection
      title="API providers"
      description="Per-token API providers — paste a key to enable each one. Every adapter also falls back to its documented env var (shown below each row) if the vault has nothing, so a row can read as configured even with no key saved here."
    >
      <div className="settings-row spaced">
        <button type="button" onClick={() => void refresh()}>
          Refresh
        </button>
      </div>

      {rows?.map((p) => {
        const busy = busyRow === p.id;
        return (
          <div key={p.id} className="settings-stack tight spaced">
            <div className="settings-row wrap">
              <span className="settings-row">
                {p.label}{" "}
                <StatusPill
                  ok={p.key_set}
                  okLabel="key saved"
                  offLabel="not configured"
                />
              </span>
              <input
                type="password"
                placeholder={p.key_set ? "Replace key…" : "Paste API key…"}
                aria-label={`${p.label} API key`}
                value={keyDrafts[p.id] ?? ""}
                onChange={(e) =>
                  setKeyDrafts((d) => ({ ...d, [p.id]: e.target.value }))
                }
                disabled={busy}
              />
              <button
                type="button"
                onClick={() => void saveKey(p.id)}
                disabled={busy || !(keyDrafts[p.id] ?? "").trim()}
              >
                {busy ? "Saving…" : "Save"}
              </button>
              {p.key_set && (
                <button
                  type="button"
                  onClick={() => void removeKey(p.id)}
                  disabled={busy}
                >
                  Remove
                </button>
              )}
            </div>
            <small className="settings-muted">
              {p.base_url} · falls back to env <code>{p.api_key_env}</code>
            </small>
          </div>
        );
      })}

      {err && <div className="settings-err">{err}</div>}
    </SettingsSection>
  );
}
