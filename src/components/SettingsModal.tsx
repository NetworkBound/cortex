import { useEffect, useMemo, useState, type ReactNode } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { humanizeError } from "@/lib/errors";
import {
  getGatewayConfig,
  getProviderConfig,
  importChatFile,
  importChatPull,
  listLocalCliProviders,
  listRules,
  setGatewayApiKey,
  setProviderDefaultModel,
  setProviderKey,
  setRuntimeMode,
  updateGatewayConfig,
  validateProviderKey,
  tsEnable,
  tsDisable,
  tsStatus,
  tsSetAuthkey,
  historySyncStatus,
  historySyncSetEnabled,
  historySyncNow,
  historySyncConnect,
  type ImportProvider,
  type ImportResult,
  type LocalCliProvider,
  type ProviderConfig,
  type ProviderSyncStatus,
  type RuleActivation,
  type RuleSummary,
  type TsStatus,
} from "@/lib/cortex-bridge";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { CliLoginModal } from "./CliLoginModal";
import { pushToast } from "@/lib/toast";
import { notifyGatewayConfigChanged } from "@/lib/gateway";
import { setObsidianVault } from "@/lib/brain";
import {
  listAutoApprove,
  removeAutoApprove,
  type AutoApproveEntry,
} from "@/lib/approvals";
import { checkUpdates, configuredManifestUrl, type UpdateInfo } from "@/lib/updater";
import { SettingsThemeTab } from "./SettingsThemeTab";
import { playSound } from "@/lib/sounds";
import { useCortexStore } from "@/state/store";
import { applyProfile, listProfiles, type Profile } from "@/lib/profiles";
import {
  DEFAULT_SANDBOX_TIER,
  SANDBOX_TIERS,
  SANDBOX_TIER_META,
  getSandboxTier,
  setSandboxTier,
  type SandboxTier,
} from "@/lib/sandbox";
import {
  listMonitors,
  startMonitors,
  stopMonitors,
  type MonitorSpec,
} from "@/lib/monitors";
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
import "@/styles/settings.css";

// Compact pill that visualises a rule's activation mode. Colours are
// indicative-only and inherit from the theme tokens so they stay readable in
// both light and dark builds.
const ACTIVATION_LABELS: Record<RuleActivation, string> = {
  alwaysApply: "always",
  globs: "globs",
  description: "desc",
  manual: "manual",
};

function ActivationBadge({ activation }: { activation: RuleActivation }) {
  const label = ACTIVATION_LABELS[activation];
  return (
    <span title={`activation: ${activation}`} className="settings-badge">
      {label}
    </span>
  );
}

// ── Chat-history import ──────────────────────────────────────────────────────
// Self-contained so its local state doesn't bloat the big SettingsModal render.
// File import goes through the native dialog (the repo already uses
// @tauri-apps/plugin-dialog); the experimental pull mirrors the mobile UX.

type ImportStatus =
  | { kind: "idle" }
  | { kind: "busy"; what: string }
  | { kind: "ok"; result: ImportResult }
  | { kind: "err"; message: string };

function ImportSettings() {
  const [status, setStatus] = useState<ImportStatus>({ kind: "idle" });
  const [provider, setProvider] = useState<ImportProvider>("claude");
  const [token, setToken] = useState("");

  const busy = status.kind === "busy";

  const run = async (what: string, fn: () => Promise<ImportResult>) => {
    setStatus({ kind: "busy", what });
    try {
      setStatus({ kind: "ok", result: await fn() });
    } catch (e) {
      setStatus({ kind: "err", message: humanizeError(e) });
    }
  };

  const pickFile = async () => {
    if (busy) return;
    const selected = await openDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Chat export", extensions: ["json"] }],
    });
    if (typeof selected !== "string") return; // cancelled
    void run("Importing file…", () => importChatFile(selected));
  };

  const pull = () => {
    const t = token.trim();
    if (!t || busy) return;
    void run(`Pulling from ${provider}…`, () => importChatPull(provider, t));
  };

  return (
    <div className="settings-section">
      <h3>Import chat history</h3>
      <div className="settings-hint spaced">
        Bring your Claude.ai or ChatGPT history into Cortex. Imported chats
        become resumable, searchable sessions in the sidebar.
      </div>

      {status.kind === "busy" && (
        <div className="settings-hint">{status.what}</div>
      )}
      {status.kind === "ok" && (
        <div className="settings-hint">
          ✓ Imported {status.result.imported}{" "}
          {status.result.imported === 1 ? "conversation" : "conversations"}
          {status.result.skipped > 0 && `, skipped ${status.result.skipped}`}.
        </div>
      )}
      {status.kind === "err" && (
        <div className="settings-err">{status.message}</div>
      )}

      <div className="settings-row spaced">
        <button type="button" onClick={() => void pickFile()} disabled={busy}>
          Choose export file…
        </button>
        <span className="settings-hint">
          A chat-export <code>.json</code> from Claude.ai, ChatGPT, or generic.
        </span>
      </div>

      <div className="settings-hint spaced">
        <strong>⚠️ Experimental / unofficial.</strong> Pull directly from your
        account with a session token. Uses fragile, unofficial endpoints that
        may break. The token is sent once to import and never stored or logged.
      </div>

      <label>
        Provider
        <select
          value={provider}
          onChange={(e) => setProvider(e.target.value as ImportProvider)}
          disabled={busy}
        >
          <option value="claude">Claude.ai</option>
          <option value="chatgpt">ChatGPT</option>
        </select>
      </label>

      <label>
        Session token
        <input
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder={provider === "claude" ? "sessionKey value" : "accessToken value"}
          disabled={busy}
        />
      </label>
      <div className="settings-hint">
        {provider === "claude" ? (
          <>
            Claude.ai: copy the <code>sessionKey</code> cookie value (DevTools →
            Application → Cookies → claude.ai).
          </>
        ) : (
          <>
            ChatGPT: open <code>chatgpt.com/api/auth/session</code> while signed
            in and copy the <code>accessToken</code> value.
          </>
        )}
      </div>

      <div className="settings-row spaced">
        <button type="button" onClick={pull} disabled={busy || !token.trim()}>
          Pull from {provider === "claude" ? "Claude.ai" : "ChatGPT"}
        </button>
      </div>
    </div>
  );
}

// Format an epoch-millis timestamp as a short relative "x ago" string, or
// "never" when absent. Keeps the History Sync rows compact.
function relTime(ms: number | null): string {
  if (!ms) return "never";
  const diff = Date.now() - ms;
  if (diff < 0) return "just now";
  const s = Math.floor(diff / 1000);
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}

/**
 * "History Sync" — per-provider auto-sync of web chat history into Cortex.
 *
 * Unlike the manual {@link ImportSettings} pull (which needs a pasted token),
 * this captures the provider's *web* session automatically: browser cookie
 * auto-detect first, with a one-time in-app login fallback (the "Connect"
 * button → `historySyncConnect` opens a sign-in webview). Once enabled, a
 * background loop keeps history fresh on a schedule. We poll status every few
 * seconds while any action is in flight so counts/last-synced refresh live.
 */
function HistorySyncSection() {
  const [rows, setRows] = useState<ProviderSyncStatus[]>([]);
  const [busy, setBusy] = useState<string | null>(null); // provider key in flight
  const [msg, setMsg] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const refresh = async () => {
    try {
      setRows(await historySyncStatus());
    } catch (e) {
      setErr(humanizeError(e));
    }
  };

  // Initial load + light polling so background syncs surface without a reopen.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const s = await historySyncStatus();
        if (!cancelled) setRows(s);
      } catch {
        /* leave empty */
      }
    })();
    const id = setInterval(() => {
      void (async () => {
        try {
          const s = await historySyncStatus();
          if (!cancelled) setRows(s);
        } catch {
          /* transient */
        }
      })();
    }, 5000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  const onToggle = async (provider: string, next: boolean) => {
    setErr(null);
    setMsg(null);
    setBusy(provider);
    try {
      await historySyncSetEnabled(provider, next);
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(null);
    }
  };

  const onSyncNow = async (provider: string) => {
    setErr(null);
    setMsg(null);
    setBusy(provider);
    try {
      setMsg(await historySyncNow(provider));
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(null);
    }
  };

  const onConnect = async (provider: string) => {
    setErr(null);
    setMsg(null);
    setBusy(provider);
    try {
      setMsg(await historySyncConnect(provider));
      await refresh();
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="settings-section">
      <h3>History Sync</h3>
      <div className="settings-hint spaced">
        Automatically pull your web chat history (Claude.ai, ChatGPT) into
        Cortex and keep it updated on a schedule — no token to paste. The web
        session is detected from your browser; if that fails, use{" "}
        <strong>Connect</strong> to sign in once. Sessions are never logged.
      </div>

      {msg && <div className="settings-hint">{msg}</div>}
      {err && <div className="settings-err">{err}</div>}

      {rows.map((r) => {
        const inFlight = busy === r.provider;
        return (
          <div key={r.provider} className="settings-section">
            <label className="settings-check">
              <input
                type="checkbox"
                checked={r.enabled}
                disabled={inFlight}
                onChange={(e) => void onToggle(r.provider, e.target.checked)}
              />
              <span>Sync history — {r.label}</span>
            </label>
            <div className="settings-hint">
              {r.conversation_count}{" "}
              {r.conversation_count === 1 ? "conversation" : "conversations"} ·
              last synced {relTime(r.last_sync)}
              {r.session_source ? ` · via ${r.session_source}` : ""}
            </div>
            <div className="settings-row spaced">
              <button
                type="button"
                onClick={() => void onSyncNow(r.provider)}
                disabled={inFlight}
              >
                Sync now
              </button>
              {r.needs_login && (
                <button
                  type="button"
                  onClick={() => void onConnect(r.provider)}
                  disabled={inFlight}
                >
                  Connect / Sign in
                </button>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

type TabId =
  | "general"
  | "connections"
  | "providers"
  | "workspace"
  | "theme"
  | "updates"
  | "advanced";

const TABS: { id: TabId; label: string }[] = [
  { id: "general", label: "General" },
  { id: "connections", label: "Connections" },
  { id: "providers", label: "Providers" },
  { id: "workspace", label: "Workspace" },
  { id: "theme", label: "Theme" },
  { id: "updates", label: "Updates" },
  { id: "advanced", label: "Advanced" },
];

const TAB_STORAGE_KEY = "cortex.settingsTab";

function loadTab(): TabId {
  try {
    const raw = localStorage.getItem(TAB_STORAGE_KEY);
    if (raw && TABS.some((t) => t.id === raw)) return raw as TabId;
  } catch {
    /* ignore */
  }
  return "general";
}

function persistTab(id: TabId) {
  try {
    localStorage.setItem(TAB_STORAGE_KEY, id);
  } catch {
    /* ignore */
  }
}

// One section per "card" inside a tab. `text` is concatenated heading + body
// text used for the substring search filter at the top of the nav.
type Section = { tab: TabId; heading: string; text: string; render: () => ReactNode };

/**
 * Brain auto-context toggle. A standalone component (rather than inline render)
 * so it *subscribes* to the store via the hook and re-renders when the value
 * flips — reading `getState()` once would leave the checkbox stale.
 */
function BrainSettingsSection() {
  const enabled = useCortexStore((s) => s.brainAutoEnabled);
  const setEnabled = useCortexStore((s) => s.setBrainAutoEnabled);
  return (
    <div className="settings-section">
      <h3>Brain</h3>
      <div className="settings-hint spaced">
        The local brain greps memory + recent edits + project files when
        you pause typing. Suggested @-tokens appear above the composer
        so you can click to attach. Disable below if you prefer to
        trigger brain context manually via the 🧠 button, slash
        commands, or <code>Alt+B</code>. <strong>Implicit path
        mentions</strong> (typing <code>src/auth.rs</code> directly into
        the draft) auto-attach up to 3 files regardless of this setting.
      </div>
      <label className="settings-check">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
        />
        <span>Auto-fire brain on typing pause (≥25 chars + 800ms)</span>
      </label>
    </div>
  );
}

/**
 * "Always-allow grants" — the revoke list for the global auto-approve allowlist
 * (`~/.cortex/auto-approve.json`). Every "Always allow" the user clicks in an
 * approval prompt lands here as a persistent, global-scope grant; this section
 * lets them review and revoke them. Loaded on open and after each revoke so the
 * list stays in sync with the on-disk file.
 */
function AutoApproveSection() {
  const [rows, setRows] = useState<AutoApproveEntry[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState<number | null>(null);

  async function refresh() {
    try {
      setRows(await listAutoApprove());
      setErr(null);
    } catch (e) {
      setErr(humanizeError(e));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function revoke(index: number) {
    setBusy(index);
    setErr(null);
    try {
      await removeAutoApprove(index);
      await refresh();
      pushToast({ title: "Grant revoked", kind: "success" });
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="settings-section">
      <h3>Always-allow grants</h3>
      <div className="settings-hint spaced">
        Tools you've granted a permanent, global "Always allow" via an approval
        prompt. These live in <code>~/.cortex/auto-approve.json</code> and skip
        the approval step on every project. Revoke any you no longer trust.
      </div>
      {err && <div className="settings-err">{err}</div>}
      {rows && rows.length === 0 && (
        <div className="settings-hint">
          No always-allow grants. You haven't permanently auto-approved any
          tools.
        </div>
      )}
      {rows && rows.length > 0 && (
        <ul className="settings-list">
          {rows.map((r, i) => (
            <li key={`${r.tool}|${r.pattern}|${i}`} className="settings-list-row">
              <code>{r.tool.trim() === "" ? "(any tool)" : r.tool}</code>
              <span className="settings-muted settings-mono">{r.pattern}</span>
              {r.profile && (
                <span className="settings-microlabel">{r.profile}</span>
              )}
              <button
                type="button"
                className="settings-label-sm danger"
                onClick={() => void revoke(i)}
                disabled={busy !== null}
              >
                {busy === i ? "Revoking…" : "Revoke"}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// Status pill for a provider credential / login state. Green when ready,
// amber otherwise — semantic tokens only so it tracks the active theme.
function StatusPill({ ok, okLabel, offLabel }: { ok: boolean; okLabel: string; offLabel: string }) {
  return (
    <span className={`settings-pill ${ok ? "ok" : "warn"}`}>
      {ok ? okLabel : offLabel}
    </span>
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
function LocalCliProvidersSection() {
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
    <div className="settings-section">
      <h3>Local AI providers</h3>
      <div className="settings-hint spaced">
        Every major AI maker's CLI, driven locally — no gateway, no keys to paste.
        Each row spawns that CLI's own binary; auth is the CLI's own login. Install
        the ones you want, then click <strong>Sign in</strong> to complete the
        provider's login flow inside Cortex.
      </div>
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
    </div>
  );
}

/**
 * "Tailscale (embedded)" — toggles the userspace Tailscale node baked into
 * Cortex. When enabled with no stored key, we poll `ts_status` every ~2s; if
 * the node reports `needs_login` we surface the login URL with an "Open login
 * page" button (opens in the system browser via the shell plugin). Polling
 * stops once the node is `connected` or hits an `error`.
 *
 * The auth key is write-only: it goes straight into the OS keychain via
 * `ts_set_authkey`/`ts_enable` and is never read back or logged.
 */
function TailscaleSection() {
  const [status, setStatus] = useState<TsStatus>({ state: "disconnected" });
  const [authkey, setAuthkey] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [savedKey, setSavedKey] = useState(false);

  const enabled =
    status.state === "connected" ||
    status.state === "needs_login" ||
    status.state === "error";

  // Pull the current status once on mount so the UI reflects a node that was
  // already enabled (e.g. from a previous session / disk hydration).
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const s = await tsStatus();
        if (!cancelled) setStatus(s);
      } catch {
        /* leave at disconnected */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Poll while the node is mid-flight (disconnected→needs_login→connected).
  // Stop once connected or errored — those are terminal for this view.
  useEffect(() => {
    if (status.state === "connected" || status.state === "error") return;
    if (!enabled && status.state === "disconnected") return;
    let cancelled = false;
    const id = setInterval(() => {
      void (async () => {
        try {
          const s = await tsStatus();
          if (!cancelled) setStatus(s);
        } catch {
          /* transient — keep polling */
        }
      })();
    }, 2000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [status.state, enabled]);

  const onToggle = async (next: boolean) => {
    setErr(null);
    setBusy(true);
    try {
      if (next) {
        // Pass the in-field key if the user typed one but hasn't hit Save;
        // otherwise enable with whatever (if anything) is in the keychain.
        const key = authkey.trim();
        const s = await tsEnable(key.length > 0 ? key : undefined);
        setStatus(s);
      } else {
        await tsDisable();
        setStatus({ state: "disconnected" });
      }
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const onSaveKey = async () => {
    const key = authkey.trim();
    if (!key) return;
    setErr(null);
    setBusy(true);
    try {
      await tsSetAuthkey(key);
      setSavedKey(true);
      setTimeout(() => setSavedKey(false), 2500);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  // "Log in" re-runs enable so the sidecar (re)starts and produces a fresh
  // login URL when the node still needs interactive auth.
  const onLogin = async () => {
    setErr(null);
    setBusy(true);
    try {
      const key = authkey.trim();
      const s = await tsEnable(key.length > 0 ? key : undefined);
      setStatus(s);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const openLogin = (url: string) => {
    void openExternal(url).catch((e) => setErr(humanizeError(e)));
  };

  return (
    <div className="settings-section">
      <h3>Tailscale (embedded)</h3>
      <div className="settings-hint spaced">
        Reach your home Cortex gateway + local LLM from any network, no admin —
        a userspace Tailscale runs inside Cortex. Local traffic
        (<code>127.0.0.1</code>, LAN) stays local; only tailnet / home services
        route over the tunnel.
      </div>

      <label className="settings-check">
        <input
          type="checkbox"
          checked={enabled}
          disabled={busy}
          onChange={(e) => void onToggle(e.target.checked)}
        />
        <span>Enable embedded Tailscale</span>
      </label>

      <div className="settings-stack tight gap-top">
        <label>
          Auth key (optional)
          <input
            type="password"
            value={authkey}
            autoComplete="off"
            placeholder="tskey-auth-… (stored in OS keychain)"
            onChange={(e) => setAuthkey(e.target.value)}
          />
        </label>
        <div className="settings-row wrap">
          <button
            type="button"
            disabled={busy || authkey.trim().length === 0}
            onClick={() => void onSaveKey()}
          >
            Save key
          </button>
          <button type="button" disabled={busy} onClick={() => void onLogin()}>
            Log in
          </button>
          {savedKey && <span className="settings-success">Saved.</span>}
        </div>
        <small className="settings-muted">
          With an auth key the node joins headlessly. Without one, click
          <strong> Log in</strong> and open the login page below.
        </small>
      </div>

      <div className="settings-row wrap gap-top">
        {status.state === "connected" && (
          <span className="settings-pill ok">connected</span>
        )}
        {status.state === "needs_login" && (
          <span className="settings-pill warn">needs login</span>
        )}
        {status.state === "error" && (
          <span className="settings-pill warn">error</span>
        )}
        {status.state === "disconnected" && (
          <span className="settings-pill warn">disconnected</span>
        )}
      </div>

      {status.state === "connected" && (
        <div className="settings-hint" style={{ color: "var(--success)" }}>
          On the tailnet as <code>{status.dnsname}</code> (
          <code>{status.ip}</code>).
        </div>
      )}

      {status.state === "needs_login" && (
        <div className="settings-stack tight">
          <div className="settings-hint" style={{ color: "var(--warning)" }}>
            This node needs to be authorised. Open the login page to add it to
            your tailnet:
          </div>
          <div className="settings-row wrap">
            <button type="button" onClick={() => openLogin(status.url)}>
              Open login page
            </button>
            <a
              href={status.url}
              target="_blank"
              rel="noreferrer"
              className="settings-link settings-mono"
            >
              {status.url}
            </a>
          </div>
        </div>
      )}

      {status.state === "error" && (
        <div className="settings-err">{status.msg}</div>
      )}

      {err && <div className="settings-err">{err}</div>}
    </div>
  );
}

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
    staticModels: ["gpt-4o", "gpt-4o-mini", "gpt-4.1", "gpt-4.1-mini", "o3", "o4-mini"],
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
  const [validation, setValidation] = useState<ValidationState>({ phase: "idle" });
  const [liveModels, setLiveModels] = useState<string[]>([]);
  const [err, setErr] = useState<string | null>(null);

  const keySet =
    meta.id === "anthropic" ? !!cfg?.anthropic_key_set : !!cfg?.openai_key_set;
  const defaultModel =
    (meta.id === "anthropic" ? cfg?.anthropic_default_model : cfg?.openai_default_model) ??
    "";

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
    if (defaultModel && !merged.includes(defaultModel)) merged.unshift(defaultModel);
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
          placeholder={keySet ? "leave blank to keep current" : meta.keyPlaceholder}
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
            validation.phase === "busy" || (!keySet && keyDraft.trim().length === 0)
          }
        >
          {validation.phase === "busy" ? "Validating…" : "Validate"}
        </button>
        {validation.phase === "done" && (
          <small className={`settings-validation ${validation.ok ? "ok" : "err"}`}>
            {validation.message}
          </small>
        )}
      </div>
      <label className="settings-field-row">
        <span className="settings-field-label">Default model</span>
        <select value={defaultModel} onChange={(e) => void changeModel(e.target.value)}>
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
          Validate the key to merge {meta.label}'s live model list into this picker.
          Model changes apply on the next message — no restart needed.
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
function ProviderSettingsSection() {
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
    <div className="settings-section">
      <h3>Providers</h3>
      <div className="settings-hint spaced">
        Sign in to model providers directly, bypassing the Cortex Gateway. Keys
        are stored encrypted in the OS key vault and never leave this machine.
        {cfg && !cfg.standalone_build && (
          <>
            {" "}
            <strong>Note:</strong> this build does not include the standalone
            (cloud) adapters — keys are saved and validated, but the direct
            providers only activate in a build compiled with the{" "}
            <code>standalone</code> feature.
          </>
        )}
      </div>

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
            onChange={(e) => void changeMode(e.target.value as "homelab" | "cloud")}
          >
            <option value="homelab">Gateway — route through the Cortex Gateway</option>
            <option value="cloud">Cloud — direct provider APIs (keys below)</option>
          </select>
        </label>
        <small className="settings-muted">
          {cfg?.standalone_build
            ? "Saved in-app; takes effect on the next restart. The CORTEX_RUNTIME_MODE env var still works as a fallback when no in-app choice has been saved."
            : "Mode switching needs the standalone build — this build always routes through the gateway."}
        </small>
      </div>

      {PROVIDER_META.map((meta) => (
        <ProviderRow key={meta.id} meta={meta} cfg={cfg} onConfigChange={refresh} />
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
    </div>
  );
}

export function SettingsModal() {
  const show = useCortexStore((s) => s.showSettings);
  const setShow = useCortexStore((s) => s.setShowSettings);
  const setHasApiKey = useCortexStore((s) => s.setHasApiKey);
  const soundsEnabled = useCortexStore((s) => s.soundsEnabled);
  const setSoundsEnabled = useCortexStore((s) => s.setSoundsEnabled);

  const activeProject = useCortexStore((s) => s.activeProject);
  const currentProfile = useCortexStore((s) => s.currentProfile);
  const setCurrentProfile = useCortexStore((s) => s.setCurrentProfile);

  // Advanced tab — wired to existing store toggles (both have complete
  // setter actions in store.ts; we do NOT add new store fields here).
  const statusBarCompact = useCortexStore((s) => s.statusBarCompact);
  const setStatusBarCompact = useCortexStore((s) => s.setStatusBarCompact);
  const architectMode = useCortexStore((s) => s.architectMode);
  const setArchitectMode = useCortexStore((s) => s.setArchitectMode);
  const autoCondenseEnabled = useCortexStore((s) => s.autoCondenseEnabled);
  const setAutoCondenseEnabled = useCortexStore((s) => s.setAutoCondenseEnabled);
  const autoCondenseThreshold = useCortexStore((s) => s.autoCondenseThreshold);
  const setAutoCondenseThreshold = useCortexStore((s) => s.setAutoCondenseThreshold);

  const [activeTab, setActiveTab] = useState<TabId>(() => loadTab());
  const [query, setQuery] = useState("");
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [profileErr, setProfileErr] = useState<string | null>(null);
  const [rules, setRules] = useState<RuleSummary[]>([]);
  const [rulesErr, setRulesErr] = useState<string | null>(null);

  // Connection fields start empty ("not configured") and are hydrated from
  // the backend config when the modal opens — no baked-in addresses.
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("gateway-agent");
  const [apiKey, setApiKey] = useState("");
  const [ollamaUrl, setOllamaUrl] = useState("");
  const [ollamaModel, setOllamaModel] = useState("qwen2.5:14b");
  const [obsidian, setObsidian] = useState("");
  const [hasKey, setHasKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [sandboxTier, setSandboxTierState] =
    useState<SandboxTier>(DEFAULT_SANDBOX_TIER);
  const [sandboxErr, setSandboxErr] = useState<string | null>(null);
  const [monitors, setMonitors] = useState<MonitorSpec[]>([]);
  const [monitorsErr, setMonitorsErr] = useState<string | null>(null);
  const [monitorsActive, setMonitorsActive] = useState(false);
  const [monitorsBusy, setMonitorsBusy] = useState(false);

  // Advanced tab — Continue.dev-style per-project model roles (default model per
  // logical role), persisted at `.cortex/model-roles.toml`. `modelList` populates
  // the per-role `<select>`s; an empty selection clears that role.
  const [modelRoles, setModelRolesState] = useState<ModelRoles>({});
  const [modelList, setModelList] = useState<ModelEntry[]>([]);
  const [modelRolesErr, setModelRolesErr] = useState<string | null>(null);

  // Updates tab — local-only state for the manual "check for updates" flow.
  // We never download/apply here; we only surface current vs latest.
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateChecking, setUpdateChecking] = useState(false);
  const [updateErr, setUpdateErr] = useState<string | null>(null);
  // Running build version, surfaced unconditionally (no manifest needed) so the
  // user can always see what they're on — read from the Tauri app metadata,
  // which is sourced from tauri.conf.json / Cargo.toml at build time.
  const [appVersion, setAppVersion] = useState<string | null>(null);

  // Advanced tab — diagnostics export. Local-only: the backend writes a
  // redacted bundle under ~/.cortex and we surface the resulting path.
  const [diagBusy, setDiagBusy] = useState(false);
  const [diagResult, setDiagResult] = useState<DiagnosticsExport | null>(null);
  const [diagErr, setDiagErr] = useState<string | null>(null);

  async function runDiagnosticsExport() {
    setDiagBusy(true);
    setDiagErr(null);
    try {
      const res = await exportDiagnostics();
      setDiagResult(res);
      pushToast({
        title: "Diagnostics exported",
        body: res.path,
        kind: "success",
      });
    } catch (e) {
      setDiagResult(null);
      setDiagErr(humanizeError(e));
    } finally {
      setDiagBusy(false);
    }
  }

  async function copyDiagnosticsPath() {
    if (!diagResult) return;
    try {
      await navigator.clipboard.writeText(diagResult.path);
      pushToast({ title: "Path copied", kind: "success" });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }

  async function runUpdateCheck() {
    setUpdateChecking(true);
    setUpdateErr(null);
    try {
      const manifestUrl = configuredManifestUrl();
      if (!manifestUrl) {
        // No baked-in manifest URL ships with the app — humanize instead of
        // dialing anything.
        setUpdateErr(
          "No update manifest configured. Set an https:// manifest URL in localStorage under cortex.updateUrl to enable update checks.",
        );
        return;
      }
      const info = await checkUpdates(manifestUrl);
      setUpdateInfo(info);
    } catch (e) {
      // Fetch can fail (offline, manifest unreachable). Show inline, never throw.
      setUpdateErr(humanizeError(e));
    } finally {
      setUpdateChecking(false);
    }
  }

  useEffect(() => {
    if (!show || appVersion) return;
    getVersion()
      .then(setAppVersion)
      .catch(() => {
        /* non-Tauri/web preview — leave version unknown rather than throw */
      });
  }, [show, appVersion]);

  useEffect(() => {
    if (!show) return;
    getGatewayConfig()
      .then((cfg) => {
        setBaseUrl(cfg.base_url);
        setModel(cfg.model);
        setOllamaUrl(cfg.ollama_base_url);
        setOllamaModel(cfg.ollama_model);
        setHasKey(cfg.has_api_key);
        setObsidian(cfg.obsidian_vault ?? "");
      })
      .catch((e) => setErr(humanizeError(e)));
  }, [show]);

  useEffect(() => {
    persistTab(activeTab);
  }, [activeTab]);

  // Refresh the profile list whenever the modal opens against a project.
  // Cheap (single fs scan) and avoids a stale list after the user edits a
  // TOML on disk between visits.
  useEffect(() => {
    if (!show) return;
    const root = activeProject?.root;
    if (!root) { setProfiles([]); return; }
    let cancelled = false;
    listProfiles(root)
      .then((list) => { if (!cancelled) setProfiles(list); })
      .catch((e) => { if (!cancelled) { setProfiles([]); setProfileErr(humanizeError(e)); } });
    return () => { cancelled = true; };
  }, [show, activeProject?.root]);

  // Refresh `.cortex/rules/*.md` summaries on the same trigger as profiles.
  // No-op when there's no active project — the UI shows a friendly hint.
  useEffect(() => {
    if (!show) return;
    const root = activeProject?.root;
    if (!root) { setRules([]); setRulesErr(null); return; }
    let cancelled = false;
    listRules(root)
      .then((list) => { if (!cancelled) { setRules(list); setRulesErr(null); } })
      .catch((e) => { if (!cancelled) { setRules([]); setRulesErr(humanizeError(e)); } });
    return () => { cancelled = true; };
  }, [show, activeProject?.root]);

  // Load the per-project sandbox tier on open so the picker reflects the
  // value the chat.rs gate is currently enforcing.
  useEffect(() => {
    if (!show) return;
    const root = activeProject?.root;
    if (!root) { setSandboxTierState(DEFAULT_SANDBOX_TIER); return; }
    let cancelled = false;
    getSandboxTier(root)
      .then((t) => { if (!cancelled) { setSandboxTierState(t); setSandboxErr(null); } })
      .catch((e) => { if (!cancelled) setSandboxErr(humanizeError(e)); });
    return () => { cancelled = true; };
  }, [show, activeProject?.root]);

  // Load the per-project model-role map + the available model list on open, so
  // the Advanced tab's per-role pickers reflect what chat.rs will resolve.
  useEffect(() => {
    if (!show) return;
    let cancelled = false;
    listModels()
      .then((list) => { if (!cancelled) setModelList(list); })
      .catch(() => { if (!cancelled) setModelList([]); });
    const root = activeProject?.root;
    if (!root) { setModelRolesState({}); setModelRolesErr(null); return; }
    getModelRoles(root)
      .then((r) => { if (!cancelled) { setModelRolesState(r); setModelRolesErr(null); } })
      .catch((e) => { if (!cancelled) { setModelRolesState({}); setModelRolesErr(humanizeError(e)); } });
    return () => { cancelled = true; };
  }, [show, activeProject?.root]);

  // Persist a single role assignment (blank clears it). Optimistic local update,
  // then write to disk; on failure surface inline and reload the stored map.
  async function updateModelRole(key: ModelRoleKey, value: string) {
    const root = activeProject?.root;
    if (!root) return;
    const next: ModelRoles = { ...modelRoles, [key]: value || null };
    setModelRolesState(next);
    setModelRolesErr(null);
    try {
      const stored = await setModelRoles(root, next);
      setModelRolesState(stored);
    } catch (e) {
      setModelRolesErr(humanizeError(e));
      try { setModelRolesState(await getModelRoles(root)); } catch { /* keep optimistic */ }
    }
  }

  // Load monitor specs for the active project so the Workspace tab can show
  // a checklist. We don't probe whether the backend is currently running them;
  // the toggle state is local to this modal session.
  useEffect(() => {
    if (!show) return;
    const root = activeProject?.root;
    if (!root) {
      setMonitors([]);
      setMonitorsErr(null);
      return;
    }
    let cancelled = false;
    listMonitors(root)
      .then((list) => {
        if (!cancelled) {
          setMonitors(list);
          setMonitorsErr(null);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setMonitors([]);
          setMonitorsErr(humanizeError(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [show, activeProject?.root]);

  async function toggleMonitors(next: boolean) {
    const root = activeProject?.root;
    if (!root) return;
    setMonitorsErr(null);
    setMonitorsBusy(true);
    try {
      if (next) {
        await startMonitors(root);
        setMonitorsActive(true);
      } else {
        await stopMonitors();
        setMonitorsActive(false);
      }
    } catch (e) {
      setMonitorsErr(humanizeError(e));
    } finally {
      setMonitorsBusy(false);
    }
  }

  async function pickSandboxTier(next: SandboxTier) {
    const root = activeProject?.root;
    if (!root || next === sandboxTier) return;
    setSandboxErr(null);
    try {
      await setSandboxTier(root, next);
      setSandboxTierState(next);
    } catch (e) {
      setSandboxErr(humanizeError(e));
    }
  }

  async function switchProfile(name: string) {
    const root = activeProject?.root;
    if (!root) return;
    setProfileErr(null);
    try {
      const applied = await applyProfile(root, name);
      setCurrentProfile(applied);
    } catch (e) {
      setProfileErr(humanizeError(e));
    }
  }

  async function save() {
    setSaving(true);
    setErr(null);
    try {
      await updateGatewayConfig({
        gateway_base_url: baseUrl,
        gateway_model: model,
        ollama_base_url: ollamaUrl,
        ollama_model: ollamaModel,
      });
      if (apiKey.trim().length > 0) {
        await setGatewayApiKey(apiKey.trim());
        setHasKey(true);
        setHasApiKey(true);
        setApiKey("");
      }
      await setObsidianVault(obsidian.trim() || null);
      // Let gateway-gated surfaces (deep research, …) re-check without a reload.
      notifyGatewayConfigChanged();
      setShow(false);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setSaving(false);
    }
  }

  // Sections, grouped by tab. Bodies preserve the original behavior — they
  // are just wrapped so they can render under the correct tab and respond to
  // the search filter.
  const sections: Section[] = useMemo(
    () => [
      {
        tab: "general",
        heading: "Welcome",
        text: "welcome cortex desktop gateway obsidian onboarding intro theme",
        render: () => (
          <div className="settings-section">
            <h3>Welcome</h3>
            <div className="settings-hint">
              Cortex is the desktop client to your Cortex Gateway. Use the tabs
              on the left to configure connections, your Obsidian workspace,
              and other options.
            </div>
          </div>
        ),
      },
      {
        tab: "general",
        heading: "Sounds",
        text: "sounds audio feedback chime done approve error tick mute beep",
        render: () => (
          <div className="settings-section">
            <h3>Sounds</h3>
            <label className="settings-check">
              <input
                type="checkbox"
                checked={soundsEnabled}
                onChange={(e) => {
                  const next = e.target.checked;
                  setSoundsEnabled(next);
                  // Preview the tone immediately when toggling on so the user
                  // knows what they just signed up for. Toggle-off stays silent.
                  if (next) playSound("done");
                }}
              />
              <span>
                Enable subtle audio feedback
                <small>
                  Short tones on completion, approval, errors, and copy/pin. Off by default.
                </small>
              </span>
            </label>
          </div>
        ),
      },
      {
        tab: "connections",
        heading: "Gateway backend",
        text: "gateway backend url model id api key bearer v1",
        render: () => (
          <div className="settings-section">
            <h3>Gateway backend</h3>
            <label>
              Gateway backend URL
              <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
            </label>
            <label>
              Model id
              <input value={model} onChange={(e) => setModel(e.target.value)} />
            </label>
            <label>
              Gateway API key{" "}
              {hasKey && (
                <small className="settings-success">
                  (configured — leave blank to keep)
                </small>
              )}
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={hasKey ? "leave blank to keep current" : "Bearer key for /v1/* access"}
              />
            </label>
          </div>
        ),
      },
      {
        tab: "connections",
        heading: "Sandbox tier",
        text: "sandbox tier read-only workspace-write danger full access codex three permission gate",
        render: () => (
          <div className="settings-section">
            <h3>Sandbox tier</h3>
            {!activeProject ? (
              <div className="settings-hint">
                Pick a project to configure <code>.cortex/sandbox.toml</code>.
              </div>
            ) : (
              <div className="settings-stack">
                {SANDBOX_TIERS.map((t) => {
                  const m = SANDBOX_TIER_META[t];
                  return (
                    <label
                      key={t}
                      className={`sandbox-radio${t === sandboxTier ? " selected" : ""}`}
                      style={t === sandboxTier ? { borderColor: m.color } : undefined}
                    >
                      <input
                        type="radio"
                        name="settings-sandbox-tier"
                        checked={t === sandboxTier}
                        onChange={() => void pickSandboxTier(t)}
                      />
                      <span className="sandbox-radio-body">
                        <span
                          className="sandbox-radio-label"
                          style={{ color: m.color }}
                        >
                          {m.label}
                        </span>
                        <small className="sandbox-radio-desc">{m.description}</small>
                      </span>
                    </label>
                  );
                })}
                {sandboxErr && <div className="settings-err">{sandboxErr}</div>}
                <div className="settings-hint">
                  Tier rejections are deny-bias and override approval rules.
                  High-risk guardrails still apply on top.
                </div>
              </div>
            )}
          </div>
        ),
      },
      {
        tab: "connections",
        heading: "Always-allow grants",
        text: "always allow grants auto approve allowlist revoke remove permanent global scope auto-approve.json shell file destructive permission tool pattern trust",
        render: () => <AutoApproveSection />,
      },
      {
        tab: "connections",
        heading: "Profile",
        text: "profile bundle model sandbox reasoning allowed tools toml cortex switch active",
        render: () => (
          <div className="settings-section">
            <h3>Profile</h3>
            {!activeProject && (
              <div className="settings-hint">
                Pick a project to load its <code>.cortex/profiles/*.toml</code> bundles.
              </div>
            )}
            {activeProject && (
              <div className="settings-stack">
                <div className="settings-hint">
                  Active: <strong className="settings-emph">{currentProfile?.name ?? "none"}</strong>
                </div>
                {currentProfile && (
                  <div className="settings-hint">
                    {currentProfile.model && <>model: <code>{currentProfile.model}</code><br /></>}
                    {currentProfile.sandbox_tier && <>sandbox: <code>{currentProfile.sandbox_tier}</code><br /></>}
                    {currentProfile.reasoning_effort && <>reasoning: <code>{currentProfile.reasoning_effort}</code><br /></>}
                    {currentProfile.allowed_tools && currentProfile.allowed_tools.length > 0 && (
                      <>tools: <code>{currentProfile.allowed_tools.join(", ")}</code><br /></>
                    )}
                    {currentProfile.system_prompt && (
                      <>prompt: <code>{currentProfile.system_prompt.slice(0, 80)}{currentProfile.system_prompt.length > 80 ? "…" : ""}</code></>
                    )}
                  </div>
                )}
                {profiles.length === 0 ? (
                  <div className="settings-hint">
                    No profiles in <code>{activeProject.root}/.cortex/profiles/</code>. Drop a <code>&lt;name&gt;.toml</code> there to enable switching.
                  </div>
                ) : (
                  <div className="settings-row wrap">
                    {profiles.map((p) => (
                      <button
                        key={p.name}
                        type="button"
                        onClick={() => void switchProfile(p.name)}
                        disabled={currentProfile?.name === p.name}
                        title={p.system_prompt ?? p.model ?? p.name}
                      >
                        {p.name}
                      </button>
                    ))}
                  </div>
                )}
                {profileErr && <div className="settings-err">{profileErr}</div>}
              </div>
            )}
          </div>
        ),
      },
      {
        tab: "connections",
        heading: "Ollama",
        text: "ollama base url model local llm",
        render: () => (
          <div className="settings-section">
            <h3>Ollama</h3>
            <label>
              Ollama base URL
              <input value={ollamaUrl} onChange={(e) => setOllamaUrl(e.target.value)} />
            </label>
            <label>
              Ollama model
              <input value={ollamaModel} onChange={(e) => setOllamaModel(e.target.value)} />
            </label>
          </div>
        ),
      },
      {
        tab: "connections",
        heading: "Tailscale (embedded)",
        text: "tailscale embedded userspace tsnet socks5 proxy tailnet magicdns vpn mesh remote home gateway login authkey auth key connect network connectivity no admin",
        render: () => <TailscaleSection />,
      },
      {
        tab: "providers",
        heading: "Providers",
        text: "providers anthropic openai api key validate model picker default model mode switch homelab claude cli login direct cloud standalone gateway bypass sign in",
        render: () => <ProviderSettingsSection />,
      },
      {
        tab: "providers",
        heading: "Local AI providers",
        text: "local cli providers claude codex openai gemini google qwen grok xai aider mistral vibe sign in login install headless detect installed authenticated terminal",
        render: () => <LocalCliProvidersSection />,
      },
      {
        tab: "workspace",
        heading: "Obsidian vault",
        text: "obsidian vault path workspace notes brain cortex per-project",
        render: () => (
          <div className="settings-section">
            <h3>Obsidian vault</h3>
            <label>
              Obsidian vault path
              <input
                value={obsidian}
                onChange={(e) => setObsidian(e.target.value)}
                placeholder="auto-detected: ~/Documents/Cortex Brain"
              />
            </label>
            <div className="settings-hint">
              Cortex auto-detects <code>~/Documents/Cortex Brain</code> on
              first run. Per-project config lives under <code>.cortex/*</code>
              inside each workspace directory.
            </div>
          </div>
        ),
      },
      {
        tab: "workspace",
        heading: "Import chat history",
        text: "import chat history claude chatgpt openai export json session token pull migrate conversations recent sessions experimental",
        render: () => <ImportSettings />,
      },
      {
        tab: "workspace",
        heading: "History Sync",
        text: "history sync auto automatic chat claude chatgpt browser cookie session login connect sign in schedule background pull web app conversations keep updated toggle",
        render: () => <HistorySyncSection />,
      },
      {
        tab: "theme",
        heading: "Appearance",
        text: "theme appearance dark light zinc amber carbon solarized accent palette background image wallpaper customize colors",
        render: () => <SettingsThemeTab />,
      },
      {
        tab: "workspace",
        heading: "Project rules",
        text: "rules cortex mdc cursor activation globs description manual always apply frontmatter",
        render: () => (
          <div className="settings-section">
            <h3>Project rules</h3>
            <div className="settings-hint spaced">
              Markdown files in <code>.cortex/rules/</code> are pulled into
              every new chat for this project. Add a YAML frontmatter
              <code>activation</code> field to scope when each rule fires.
            </div>
            {!activeProject && (
              <div className="settings-hint">
                Pick a project from the sidebar to see its rules.
              </div>
            )}
            {activeProject && rulesErr && (
              <div className="settings-err">{rulesErr}</div>
            )}
            {activeProject && !rulesErr && rules.length === 0 && (
              <div className="settings-hint">
                No rules found in <code>{activeProject.root}/.cortex/rules/</code>.
                Drop a <code>&lt;name&gt;.md</code> there to add one.
              </div>
            )}
            {activeProject && rules.length > 0 && (
              <ul className="settings-list">
                {rules.map((r) => (
                  <li key={r.name} className="settings-list-row">
                    <code>{r.name}</code>
                    <ActivationBadge activation={r.activation} />
                    {r.activation === "globs" && r.globs.length > 0 && (
                      <small className="settings-muted settings-mono">
                        {r.globs.join(", ")}
                      </small>
                    )}
                    {r.activation === "description" && r.description && (
                      <small className="settings-muted">{r.description}</small>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </div>
        ),
      },
      {
        tab: "workspace",
        heading: "Brain",
        text: "brain auto context massive memory grep recent fragments @-tokens disable",
        render: () => <BrainSettingsSection />,
      },
      {
        tab: "workspace",
        heading: "Monitors",
        text: "monitors background tail watch tests logs commands processes spawn child npm error info warn",
        render: () => (
          <div className="settings-section">
            <h3>Monitors</h3>
            <div className="settings-hint spaced">
              Background commands defined in
              <code> .cortex/monitors/monitors.json </code>
              are tailed and their output is surfaced as synthetic chat
              messages. Toggle below to start/stop the whole set.
            </div>
            {!activeProject && (
              <div className="settings-hint">
                Pick a project to configure monitors.
              </div>
            )}
            {activeProject && (
              <div className="settings-stack">
                <label className="settings-check">
                  <input
                    type="checkbox"
                    checked={monitorsActive}
                    disabled={monitorsBusy || monitors.length === 0}
                    onChange={(e) => void toggleMonitors(e.target.checked)}
                  />
                  <span>
                    {monitorsActive ? "Monitors running" : "Monitors stopped"}
                    {monitorsBusy && (
                      <small className="settings-muted gap-left">
                        (working…)
                      </small>
                    )}
                  </span>
                </label>
                {monitorsErr && <div className="settings-err">{monitorsErr}</div>}
                {monitors.length === 0 ? (
                  <div className="settings-hint">
                    No monitors in
                    <code> {activeProject.root}/.cortex/monitors/monitors.json</code>.
                    Drop a JSON array there to enable.
                  </div>
                ) : (
                  <ul className="settings-list">
                    {monitors.map((m) => (
                      <li key={m.name} className="settings-list-row mono">
                        <strong>{m.name}</strong>
                        <span className="settings-muted">
                          {m.command} {m.args.join(" ")}
                        </span>
                        <span className="settings-microlabel">{m.level}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}
          </div>
        ),
      },
      {
        tab: "updates",
        heading: "Updates",
        text: "updates version cortex tauri release auto-update check manifest latest current offline",
        render: () => (
          <div className="settings-section">
            <h3>Updates</h3>
            <div className="settings-row spaced">
              <span className="settings-microlabel">Installed version</span>
              <code className="settings-emph">
                {appVersion ? `v${appVersion}` : "—"}
              </code>
            </div>
            <div className="settings-hint spaced">
              Cortex ships auto-updates through Tauri. Check below to compare
              your running build against the latest published manifest.
            </div>
            <div className="settings-row spaced">
              <button
                type="button"
                onClick={() => void runUpdateCheck()}
                disabled={updateChecking}
              >
                {updateChecking ? "checking…" : "Check for updates"}
              </button>
            </div>
            {updateErr && (
              <div className="settings-err spaced">
                Couldn't check for updates: {updateErr}
              </div>
            )}
            {updateInfo && (
              <div className="settings-stack">
                <div className="settings-hint">
                  Current:{" "}
                  <code className="settings-emph">{updateInfo.current}</code>
                  {"  "}·{"  "}Latest:{" "}
                  <code className="settings-emph">{updateInfo.latest}</code>
                </div>
                <div
                  className={`settings-update-status ${updateInfo.available ? "available" : "ok"}`}
                >
                  {updateInfo.available
                    ? "↑ Update available"
                    : "✓ Up to date"}
                </div>
                {updateInfo.notes && (
                  <div className="settings-note">{updateInfo.notes}</div>
                )}
                {updateInfo.url && (
                  <a
                    href={updateInfo.url}
                    target="_blank"
                    rel="noreferrer"
                    className="settings-label-sm"
                  >
                    Open release →
                  </a>
                )}
              </div>
            )}
          </div>
        ),
      },
      {
        tab: "advanced",
        heading: "Advanced",
        text: "advanced power user developer experimental flags status bar compact architect mode planner editor split toggle auto condense overflow context window summary cline threshold default model per role chat continue.dev model roles assignment export diagnostics bug report crash log bundle redacted support troubleshoot",
        render: () => (
          <div className="settings-section">
            <h3>Advanced</h3>
            <div className="settings-hint spaced">
              Power-user toggles. Each persists locally and applies immediately.
            </div>
            <label className="settings-check">
              <input
                type="checkbox"
                checked={statusBarCompact}
                onChange={(e) => setStatusBarCompact(e.target.checked)}
              />
              <span>
                Compact status bar
                <small>
                  Hide secondary chips (gateway, project, RepoWatch, msgs,
                  session-id). Also toggled with <code>Ctrl+.</code>.
                </small>
              </span>
            </label>
            <label className="settings-check gap-top">
              <input
                type="checkbox"
                checked={architectMode}
                onChange={(e) => setArchitectMode(e.target.checked)}
              />
              <span>
                Architect mode
                <small>
                  Aider-style split: plan with one model, edit with another.
                </small>
              </span>
            </label>

            <div className="settings-group">
              <div className="settings-subheading">
                Default model per role
              </div>
              <div className="settings-hint">
                Continue.dev-style. Pin a default model per role for this project.
                An explicit composer pick (chat) or <code>/architect</code> override
                always wins; <em>Auto</em> leaves the role unset.
              </div>
              {!activeProject?.root ? (
                <div className="settings-hint gap-top">
                  Open a project to assign per-role models.
                </div>
              ) : (
                <div className="settings-stack gap-top">
                  {MODEL_ROLE_KEYS.map((key) => {
                    const value = modelRoles[key] ?? "";
                    // The stored value may be a model not in the live list
                    // (offline gateway, a typed alias) — keep it selectable.
                    const known = modelList.some((m) => m.id === value);
                    return (
                      <label key={key} className="settings-field-row">
                        <span className="settings-field-label">{MODEL_ROLE_META[key].label}</span>
                        <select
                          value={value}
                          title={MODEL_ROLE_META[key].help}
                          onChange={(e) => updateModelRole(key, e.target.value)}
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
              {modelRolesErr && (
                <div className="settings-err gap-top">
                  {modelRolesErr}
                </div>
              )}
            </div>
            <label className="settings-check gap-top">
              <input
                type="checkbox"
                checked={autoCondenseEnabled}
                onChange={(e) => setAutoCondenseEnabled(e.target.checked)}
              />
              <span>
                Auto-condense on overflow
                <small>
                  Fold older turns into an LLM summary automatically once the
                  conversation fills the model's context window (Cline-style).
                </small>
              </span>
            </label>
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

            <div className="settings-group">
              <div className="settings-subheading">Diagnostics</div>
              <div className="settings-hint">
                Bundle app version, OS info, the crash log, recent session
                metadata (never message contents) and a redacted config
                snapshot into a single archive you can attach to a bug
                report. Keys, tokens, private IPs and home paths are
                scrubbed before anything touches disk.
              </div>
              <div className="settings-row gap-top">
                <button
                  type="button"
                  onClick={() => void runDiagnosticsExport()}
                  disabled={diagBusy}
                >
                  {diagBusy ? "Exporting…" : "Export diagnostics"}
                </button>
              </div>
              {diagResult && (
                <div className="settings-row wrap gap-top">
                  <code className="settings-mono settings-label-sm">
                    {diagResult.path}
                  </code>
                  <button
                    type="button"
                    className="settings-label-sm"
                    onClick={() => void copyDiagnosticsPath()}
                  >
                    Copy path
                  </button>
                  <small className="settings-muted">
                    {diagResult.files.length} files inside
                  </small>
                </div>
              )}
              {diagErr && <div className="settings-err gap-top">{diagErr}</div>}
            </div>
          </div>
        ),
      },
    ],
    [baseUrl, model, apiKey, hasKey, ollamaUrl, ollamaModel, obsidian, soundsEnabled, setSoundsEnabled, activeProject, currentProfile, profiles, profileErr, rules, rulesErr, sandboxTier, sandboxErr, monitors, monitorsActive, monitorsBusy, monitorsErr, updateInfo, updateChecking, updateErr, statusBarCompact, setStatusBarCompact, architectMode, setArchitectMode, autoCondenseEnabled, setAutoCondenseEnabled, autoCondenseThreshold, setAutoCondenseThreshold, modelRoles, modelList, modelRolesErr, diagBusy, diagResult, diagErr],
  );

  const q = query.trim().toLowerCase();
  const matches = (s: Section) =>
    q.length === 0 ||
    s.heading.toLowerCase().includes(q) ||
    s.text.toLowerCase().includes(q);

  // Which tabs have any matching section under the current search? When a
  // query is active we hide tabs with zero hits and auto-pick the first
  // matching tab if the current one is empty.
  const tabHasHits = useMemo(() => {
    const map: Record<TabId, boolean> = {
      general: false,
      connections: false,
      providers: false,
      workspace: false,
      theme: false,
      updates: false,
      advanced: false,
    };
    for (const s of sections) if (matches(s)) map[s.tab] = true;
    return map;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sections, q]);

  const visibleTabs = q.length > 0 ? TABS.filter((t) => tabHasHits[t.id]) : TABS;

  useEffect(() => {
    if (q.length === 0) return;
    if (!tabHasHits[activeTab] && visibleTabs.length > 0) {
      setActiveTab(visibleTabs[0].id);
    }
  }, [q, tabHasHits, activeTab, visibleTabs]);

  if (!show) return null;

  const visibleSections = sections.filter((s) => s.tab === activeTab && matches(s));

  return (
    <div className="modal-backdrop" onClick={() => setShow(false)}>
      <div className="modal modal-settings" onClick={(e) => e.stopPropagation()}>
        <div className="settings-header">
          <h2>Settings</h2>
        </div>
        <div className="settings-body">
          <nav className="settings-nav" aria-label="Settings categories">
            <div className="settings-search">
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search settings…"
                aria-label="Search settings"
              />
            </div>
            <div className="settings-nav-list">
              {visibleTabs.length === 0 && (
                <div className="settings-nav-empty">No matches</div>
              )}
              {visibleTabs.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className={`settings-nav-btn${t.id === activeTab ? " active" : ""}`}
                  onClick={() => setActiveTab(t.id)}
                >
                  {t.label}
                </button>
              ))}
            </div>
          </nav>
          <div className="settings-content">
            {visibleSections.length === 0 ? (
              <div className="settings-hint">
                {q.length > 0
                  ? "No settings match your search in this tab."
                  : "Nothing to configure in this tab yet."}
              </div>
            ) : (
              visibleSections.map((s, i) => <div key={`${s.tab}-${i}`}>{s.render()}</div>)
            )}
          </div>
        </div>
        <div className="settings-footer">
          {err && <div className="settings-err">{err}</div>}
          <div className="modal-actions">
            <button onClick={() => setShow(false)} disabled={saving}>Cancel</button>
            <button className="btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
