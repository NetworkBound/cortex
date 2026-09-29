import { useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { humanizeError } from "@/lib/errors";
import {
  tsEnable,
  tsDisable,
  tsStatus,
  tsSetAuthkey,
  tsGetExternalSocks,
  tsSetExternalSocks,
  tsWslSetup,
  tsWslStatus,
  tsWslStop,
  tsMobilePairing,
  type MobilePairing,
  type TsStatus,
  type WslTsStatus,
} from "@/lib/cortex-bridge";
import { SettingsSection, SettingsToggle, UnsavedBadge } from "./Section";

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
export function TailscaleSection() {
  const [status, setStatus] = useState<TsStatus>({ state: "disconnected" });
  const [authkey, setAuthkey] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [savedKey, setSavedKey] = useState(false);
  const [externalSocks, setExternalSocks] = useState("");
  // What the proxy field held when loaded / last saved — drives the unsaved pill.
  const [externalLoaded, setExternalLoaded] = useState("");
  const [savedExternal, setSavedExternal] = useState(false);
  const [wsl, setWsl] = useState<WslTsStatus | null>(null);
  const [wslBusy, setWslBusy] = useState(false);

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
      try {
        const ext = await tsGetExternalSocks();
        if (!cancelled) {
          setExternalSocks(ext);
          setExternalLoaded(ext);
        }
      } catch {
        /* leave blank */
      }
      try {
        const w = await tsWslStatus();
        if (!cancelled) setWsl(w);
      } catch {
        /* WSL status unavailable — leave null */
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
      setAuthkey("");
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

  const onSaveExternal = async () => {
    setErr(null);
    setBusy(true);
    try {
      const next = externalSocks.trim();
      await tsSetExternalSocks(next);
      setExternalSocks(next);
      setExternalLoaded(next);
      setSavedExternal(true);
      setTimeout(() => setSavedExternal(false), 2500);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const onWslSetup = async () => {
    setErr(null);
    setWslBusy(true);
    try {
      const w = await tsWslSetup();
      setWsl(w);
      if (w.proxy_addr) setExternalSocks(w.proxy_addr);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setWslBusy(false);
    }
  };

  const onWslStop = async () => {
    setErr(null);
    setWslBusy(true);
    try {
      await tsWslStop();
      const w = await tsWslStatus();
      setWsl(w);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setWslBusy(false);
    }
  };

  const openLogin = (url: string) => {
    void openExternal(url).catch((e) => setErr(humanizeError(e)));
  };

  const statusLabel: Record<TsStatus["state"], string> = {
    connected: "connected",
    needs_login: "needs login",
    error: "error",
    disconnected: "disconnected",
  };

  return (
    <SettingsSection
      title="Tailscale (embedded)"
      description={
        <>
          Reach your home Cortex gateway + local LLM from any network, no admin
          — a userspace Tailscale runs inside Cortex. Local traffic (
          <code>127.0.0.1</code>, LAN) stays local; only tailnet / home services
          route over the tunnel.
        </>
      }
    >
      <SettingsToggle
        checked={enabled}
        disabled={busy}
        onChange={(next) => void onToggle(next)}
        label="Enable embedded Tailscale"
        description="Starts the built-in userspace node. Uses the saved auth key when there is one, otherwise asks you to log in."
      />

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
          <UnsavedBadge show={authkey.trim().length > 0} />
          {savedKey && <span className="settings-success">Saved.</span>}
        </div>
        <small className="settings-muted">
          With an auth key the node joins headlessly. Without one, click
          <strong> Log in</strong> and open the login page below.
        </small>
      </div>

      <div className="settings-row wrap gap-top">
        <span
          className={`settings-pill ${status.state === "connected" ? "ok" : "warn"}`}
        >
          {statusLabel[status.state]}
        </span>
      </div>

      {status.state === "connected" && (
        <div className="settings-hint ok">
          On the tailnet as <code>{status.dnsname}</code> (
          <code>{status.ip}</code>).
        </div>
      )}

      {status.state === "connected" && <MobilePairingCard />}

      {status.state === "needs_login" && (
        <div className="settings-stack tight">
          <div className="settings-hint warn">
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

      <div className="settings-divider gap-top" />
      <h4>External SOCKS5 proxy (advanced)</h4>
      <div className="settings-hint spaced">
        For locked-down machines where the embedded sidecar can't run (no admin,
        or antivirus quarantines it): run Tailscale elsewhere — e.g. in WSL —
        exposing a SOCKS5 proxy, and point Cortex at it here. When set, Cortex
        routes home/tailnet traffic through this proxy and never starts the
        embedded sidecar. Leave blank to use the embedded node above.
      </div>

      {wsl?.wsl_available && (
        <div className="settings-card gap-top">
          <div className="settings-row wrap between">
            <strong>Set up automatically via WSL</strong>
            {wsl.connected && (
              <span className="settings-pill ok">connected</span>
            )}
            {!wsl.connected && wsl.daemon_running && (
              <span className="settings-pill warn">needs login</span>
            )}
          </div>
          <div className="settings-hint spaced">
            Cortex can install and run Tailscale inside your WSL distro, then
            point itself at it — no admin, nothing for antivirus to quarantine.
            Click once; if the node needs authorising, a login link appears.
          </div>
          <div className="settings-row wrap">
            <button
              type="button"
              disabled={wslBusy}
              onClick={() => void onWslSetup()}
            >
              {wslBusy
                ? "Working…"
                : wsl.daemon_running
                  ? "Re-run setup"
                  : "Set up Tailscale in WSL"}
            </button>
            {wsl.daemon_running && (
              <button
                type="button"
                disabled={wslBusy}
                onClick={() => void onWslStop()}
              >
                Stop
              </button>
            )}
          </div>
          {wsl.login_url && (
            <div className="settings-stack tight gap-top">
              <div className="settings-hint warn">
                Authorise this node on your tailnet:
              </div>
              <div className="settings-row wrap">
                <button type="button" onClick={() => openLogin(wsl.login_url!)}>
                  Open login page
                </button>
                <a
                  href={wsl.login_url}
                  target="_blank"
                  rel="noreferrer"
                  className="settings-link settings-mono"
                >
                  {wsl.login_url}
                </a>
              </div>
            </div>
          )}
          {wsl.proxy_addr && (
            <small className="settings-muted">
              Proxy: <code>{wsl.proxy_addr}</code>
              {wsl.tailnet_ip && (
                <>
                  {" "}
                  · tailnet IP <code>{wsl.tailnet_ip}</code>
                </>
              )}
            </small>
          )}
        </div>
      )}

      <div className="settings-hint spaced gap-top">
        Or point Cortex at a proxy you run yourself. In WSL:{" "}
        <code>
          tailscaled --tun=userspace-networking --socks5-server=0.0.0.0:1055
          &amp;
        </code>{" "}
        then <code>tailscale up</code>, and enter the WSL IP with{" "}
        <code>:1055</code> below (use the WSL IP, not <code>127.0.0.1</code>,
        unless WSL mirrored networking is on).
      </div>
      <div className="settings-stack tight">
        <label>
          Proxy address (<code>host:port</code>)
          <input
            type="text"
            value={externalSocks}
            autoComplete="off"
            placeholder="127.0.0.1:1055"
            onChange={(e) => setExternalSocks(e.target.value)}
          />
        </label>
        <div className="settings-row wrap">
          <button
            type="button"
            disabled={busy}
            onClick={() => void onSaveExternal()}
          >
            {externalSocks.trim().length > 0 ? "Save proxy" : "Clear proxy"}
          </button>
          <UnsavedBadge show={externalSocks.trim() !== externalLoaded} />
          {savedExternal && <span className="settings-success">Saved.</span>}
        </div>
        <small className="settings-muted">
          Applies to new requests immediately. A restart is safest so all
          clients pick it up.
        </small>
      </div>

      {err && <div className="settings-err">{err}</div>}
    </SettingsSection>
  );
}

/**
 * "Pair a device" — renders a QR of the tailnet mobile URL so a phone on the
 * same tailnet scans instead of typing. Only shown once the node is connected;
 * the backend derives the URL from live MagicDNS and returns the QR as SVG.
 */
function MobilePairingCard() {
  const [pairing, setPairing] = useState<MobilePairing | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  const load = async () => {
    setBusy(true);
    setErr(null);
    try {
      setPairing(await tsMobilePairing());
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setBusy(false);
    }
  };

  const copyUrl = async () => {
    if (!pairing) return;
    try {
      await navigator.clipboard.writeText(pairing.url);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      /* clipboard blocked — the URL is visible to select manually */
    }
  };

  return (
    <div className="settings-stack tight gap-top">
      {!pairing && (
        <div className="settings-row wrap">
          <button type="button" disabled={busy} onClick={() => void load()}>
            {busy ? "Preparing…" : "Pair a device"}
          </button>
          <small className="settings-muted">
            Scan a QR from your phone (must be on this tailnet) to open the
            mobile app — no URL to type.
          </small>
        </div>
      )}
      {pairing && (
        <div className="ts-pairing">
          <div
            className="ts-pairing-qr"
            // Backend-generated SVG of the tailnet URL; no external network.
            dangerouslySetInnerHTML={{ __html: pairing.qr_svg }}
          />
          <div className="settings-stack tight">
            <small className="settings-muted">
              Scan with your phone’s camera (on this tailnet):
            </small>
            <a
              href={pairing.url}
              target="_blank"
              rel="noreferrer"
              className="settings-link settings-mono"
            >
              {pairing.url}
            </a>
            <div className="settings-row wrap">
              <button type="button" onClick={() => void copyUrl()}>
                {copied ? "Copied." : "Copy URL"}
              </button>
              <button type="button" disabled={busy} onClick={() => void load()}>
                Refresh
              </button>
            </div>
          </div>
        </div>
      )}
      {err && <div className="settings-err">{err}</div>}
    </div>
  );
}
