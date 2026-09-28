import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import {
  DEFAULT_PUSH_CONFIG,
  PUSH_EVENTS,
  getPushConfig,
  setPushConfig,
  testPush,
  type PushConfig,
  type PushProvider,
} from "@/lib/push-notify";

/**
 * "Phone push" settings card: ntfy or Gotify server, topic/token, which
 * events to forward, and the LAN/tailnet opt-in. Mounted as one section of
 * SettingsModal (General tab). Owns its own load/save round-trip so the
 * modal only needs a single render line.
 *
 * The token is write-only: the backend reports `has_token` and we never
 * pull the secret back. "Test" sends with the *saved* config, so it saves
 * first when the form is dirty.
 */
export function PushNotifySettings() {
  const [cfg, setCfg] = useState<PushConfig>(DEFAULT_PUSH_CONFIG);
  const [hasToken, setHasToken] = useState(false);
  const [token, setToken] = useState("");
  const [clearToken, setClearToken] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<"save" | "test" | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    getPushConfig()
      .then((v) => {
        const { has_token, ...rest } = v;
        setCfg({ ...DEFAULT_PUSH_CONFIG, ...rest });
        setHasToken(has_token);
      })
      .catch(() => {
        /* defaults are fine on a fresh install */
      })
      .finally(() => setLoaded(true));
  }, []);

  const patch = (p: Partial<PushConfig>) => {
    setCfg((c) => ({ ...c, ...p }));
    setDirty(true);
  };

  const toggleEvent = (id: string, on: boolean) => {
    patch({
      events: on
        ? Array.from(new Set([...cfg.events, id]))
        : cfg.events.filter((e) => e !== id),
    });
  };

  async function save(): Promise<boolean> {
    setBusy("save");
    try {
      await setPushConfig(cfg, token, clearToken);
      if (clearToken) setHasToken(false);
      else if (token.trim()) setHasToken(true);
      setToken("");
      setClearToken(false);
      setDirty(false);
      return true;
    } catch (e) {
      pushToast({
        title: "Couldn't save push settings",
        body: humanizeError(e),
        kind: "error",
      });
      return false;
    } finally {
      setBusy(null);
    }
  }

  async function test() {
    if (dirty || token.trim() || clearToken) {
      if (!(await save())) return;
    }
    setBusy("test");
    try {
      const r = await testPush();
      pushToast({
        title: r.ok ? "Push sent" : "Push failed",
        body: r.ok
          ? `HTTP ${r.status ?? "?"} in ${r.latency_ms} ms — check your phone.`
          : (r.error ?? `HTTP ${r.status ?? "?"}`),
        kind: r.ok ? "success" : "error",
      });
    } catch (e) {
      pushToast({
        title: "Push failed",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setBusy(null);
    }
  }

  const isNtfy = cfg.provider === "ntfy";

  return (
    <div className="settings-section">
      <h3>Phone push</h3>
      <div className="settings-hint spaced">
        Send a notification to your phone through a self-hosted or public ntfy
        or Gotify server when a run needs approval, finishes, fails, or a quota
        window is nearly used. Approval pushes deep-link to the mobile inbox.
        Off by default; nothing leaves this machine until enabled.
      </div>

      <label className="settings-check">
        <input
          type="checkbox"
          checked={cfg.enabled}
          disabled={!loaded}
          onChange={(e) => patch({ enabled: e.target.checked })}
        />
        <span>Enable phone push notifications</span>
      </label>

      <label>
        Provider
        <select
          value={cfg.provider}
          onChange={(e) => patch({ provider: e.target.value as PushProvider })}
        >
          <option value="ntfy">ntfy</option>
          <option value="gotify">Gotify</option>
        </select>
      </label>

      <label>
        Server URL
        <input
          value={cfg.server_url}
          onChange={(e) => patch({ server_url: e.target.value })}
          placeholder={
            isNtfy
              ? "https://ntfy.sh or http://100.x.y.z:8080"
              : "http://gotify.lan"
          }
          spellCheck={false}
        />
      </label>

      {isNtfy && (
        <label>
          Topic
          <input
            value={cfg.topic}
            onChange={(e) => patch({ topic: e.target.value })}
            placeholder="cortex-alerts (letters, digits, - and _)"
            spellCheck={false}
          />
        </label>
      )}

      <label>
        {isNtfy ? "Access token (optional)" : "Application token"}{" "}
        {hasToken && !clearToken && (
          <small className="settings-success">
            (stored in the OS keychain — leave blank to keep)
          </small>
        )}
        <input
          type="password"
          value={token}
          onChange={(e) => {
            setToken(e.target.value);
            setClearToken(false);
          }}
          placeholder={
            hasToken ? "leave blank to keep current" : isNtfy ? "tk_…" : "A…"
          }
          autoComplete="off"
        />
      </label>
      {hasToken && (
        <label className="settings-check">
          <input
            type="checkbox"
            checked={clearToken}
            onChange={(e) => setClearToken(e.target.checked)}
          />
          <span>Remove the stored token on save</span>
        </label>
      )}

      <label>
        Mobile app URL (optional)
        <input
          value={cfg.mobile_url}
          onChange={(e) => patch({ mobile_url: e.target.value })}
          placeholder="https://<machine>.<tailnet>.ts.net — auto from Tailscale when blank"
          spellCheck={false}
        />
      </label>

      <div className="settings-subheading gap-top">Send for</div>
      {PUSH_EVENTS.map((ev) => (
        <label key={ev.id} className="settings-check">
          <input
            type="checkbox"
            checked={cfg.events.includes(ev.id)}
            onChange={(e) => toggleEvent(ev.id, e.target.checked)}
          />
          <span>
            {ev.label}
            <small>{ev.hint}</small>
          </span>
        </label>
      ))}

      <label className="settings-check gap-top">
        <input
          type="checkbox"
          checked={cfg.allow_private_host}
          onChange={(e) => patch({ allow_private_host: e.target.checked })}
        />
        <span>
          Allow a private-network server (LAN / tailnet)
          <small>
            Admits 10.x, 172.16–31.x, 192.168.x and Tailscale 100.64–127.x
            addresses. Loopback and link-local stay blocked. Only outbound
            webhooks keep the strict default.
          </small>
        </span>
      </label>

      <div className="settings-row gap-top">
        <button
          className="btn-primary"
          disabled={busy !== null || (!dirty && !token.trim() && !clearToken)}
          onClick={() => void save()}
        >
          {busy === "save" ? "Saving…" : "Save"}
        </button>
        <button
          disabled={busy !== null || !cfg.server_url.trim()}
          onClick={() => void test()}
          title="Saves pending changes, then sends a test notification"
        >
          {busy === "test" ? "Sending…" : "Send test"}
        </button>
      </div>
    </div>
  );
}
