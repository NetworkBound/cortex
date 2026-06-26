//! Embedded Tailscale integration.
//!
//! Cortex ships a userspace Tailscale node as a Go sidecar
//! (`sidecar/cortex-tsnet`, built on `tailscale.com/tsnet`). It joins the
//! tailnet with no root/daemon/admin and exposes a local SOCKS5 proxy whose
//! dialer routes over the tailnet. This module:
//!
//! - spawns/stops that sidecar ([`manager`]),
//! - parses its stdout status protocol into [`TsStatus`],
//! - tracks the live status + socks address in process-global state
//!   ([`shared`]), and
//! - offers [`maybe_tailscale_proxy`] so the gateway + Ollama reqwest clients
//!   tunnel home-service traffic through the proxy when enabled + connected.
//!
//! The auth key is never logged; it is stored in the OS keychain and passed to
//! the sidecar via the `TS_AUTHKEY` env var only.

pub mod manager;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Default local SOCKS5 listen address the sidecar binds and clients dial.
pub const DEFAULT_SOCKS_ADDR: &str = "127.0.0.1:1055";

const KEYRING_SERVICE: &str = "dev.connor.cortex";
const KEYRING_USER_AUTHKEY: &str = "tailscale_authkey";

/// Live state of the embedded Tailscale node, mirrored from the sidecar's
/// stdout status protocol. Serialized to the frontend by the `ts_*` commands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TsStatus {
    /// Sidecar not running (or never started).
    Disconnected,
    /// Sidecar is up but needs interactive auth; `url` is the login link.
    NeedsLogin { url: String },
    /// Node is on the tailnet. `ip` is the tailnet IPv4, `dnsname` the MagicDNS name.
    Connected { ip: String, dnsname: String },
    /// Fatal error from the sidecar.
    Error { msg: String },
}

impl Default for TsStatus {
    fn default() -> Self {
        TsStatus::Disconnected
    }
}

/// Process-global Tailscale state. Lives in a module static so the deeply-nested
/// reqwest client builders (gateway, Ollama) can consult it without threading
/// `AppState` through every call site.
#[derive(Debug)]
pub struct Shared {
    pub status: RwLock<TsStatus>,
    /// Whether the user enabled embedded Tailscale. Routing only applies when
    /// `enabled && status == Connected`.
    pub enabled: RwLock<bool>,
    /// Local SOCKS5 address (`host:port`) the sidecar listens on.
    pub socks_addr: RwLock<String>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            status: RwLock::new(TsStatus::Disconnected),
            enabled: RwLock::new(false),
            socks_addr: RwLock::new(DEFAULT_SOCKS_ADDR.to_string()),
        }
    }
}

/// Accessor for the process-global Tailscale state.
pub fn shared() -> &'static Arc<Shared> {
    use once_cell::sync::Lazy;
    static SHARED: Lazy<Arc<Shared>> = Lazy::new(|| Arc::new(Shared::default()));
    &SHARED
}

/// True when embedded Tailscale is enabled AND the node is connected — i.e. it
/// is safe to route home-service traffic through the proxy.
pub fn is_active() -> bool {
    let s = shared();
    *s.enabled.read() && matches!(*s.status.read(), TsStatus::Connected { .. })
}

/// Current local SOCKS5 address (`host:port`).
pub fn socks_addr() -> String {
    shared().socks_addr.read().clone()
}

/// Snapshot the live status.
pub fn current_status() -> TsStatus {
    shared().status.read().clone()
}

/// Hosts/CIDRs that must NEVER be routed through the embedded-Tailscale proxy:
/// loopback plus the RFC1918 private ranges. This keeps *local* services — a
/// local Ollama on `127.0.0.1:11434`, a LAN box — talking directly, while a
/// *home/tailnet* gateway or Ollama (a `100.x` CGNAT addr or MagicDNS name) is
/// still tunneled. Comma-separated, in the format `reqwest::NoProxy` parses.
const PROXY_NO_PROXY: &str = "localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";

/// Apply the embedded-Tailscale SOCKS5 proxy to a reqwest client builder **only
/// when** Tailscale is enabled + connected. Used by both the gateway client and
/// the Ollama client so home-service traffic tunnels over the tailnet.
///
/// Uses `socks5h://` (resolve-at-proxy) so MagicDNS names resolve tailnet-side
/// rather than on the local box. A no-proxy list ([`PROXY_NO_PROXY`]) carves out
/// loopback + RFC1918 so *local* traffic (e.g. a local Ollama on
/// `127.0.0.1:11434`, or a LAN host) bypasses the tunnel and stays direct, while
/// tailnet/home hosts (`100.x`, MagicDNS) still route over the proxy.
///
/// When inactive, the builder is returned unchanged, so non-Tailscale behavior
/// is byte-for-byte identical to before.
///
/// Proxy construction failures degrade gracefully (returns the unmodified
/// builder) rather than panicking — a malformed addr must never brick the app.
pub fn maybe_tailscale_proxy(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    if !is_active() {
        return builder;
    }
    let addr = socks_addr();
    match reqwest::Proxy::all(format!("socks5h://{addr}")) {
        Ok(proxy) => builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(PROXY_NO_PROXY))),
        Err(e) => {
            tracing::warn!("tailscale: bad socks proxy addr {addr}: {e}; not proxying");
            builder
        }
    }
}

// ---------- authkey storage (OS keychain) ----------

/// Store the tailnet auth key in the OS keychain. Never logged.
pub fn set_authkey(key: &str) -> anyhow::Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER_AUTHKEY)?.set_password(key)?;
    Ok(())
}

/// Read the tailnet auth key from the OS keychain, if present.
pub fn get_authkey() -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER_AUTHKEY)
        .ok()
        .and_then(|e| e.get_password().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------- enabled-state + socks addr persistence (~/.cortex/tailscale.json) ----------

fn config_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".cortex").join("tailscale.json"))
}

/// Persisted Tailscale settings (NOT the auth key — that lives in the keychain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_socks")]
    pub socks_addr: String,
}

fn default_socks() -> String {
    DEFAULT_SOCKS_ADDR.to_string()
}

impl Default for TsConfig {
    fn default() -> Self {
        Self { enabled: false, socks_addr: default_socks() }
    }
}

/// Load persisted settings, ignoring a missing/malformed file.
pub fn load_config() -> TsConfig {
    let Some(path) = config_path() else { return TsConfig::default() };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Persist settings to `~/.cortex/tailscale.json`.
pub fn save_config(cfg: &TsConfig) -> anyhow::Result<()> {
    let path = config_path().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(cfg)?)?;
    Ok(())
}

/// Hydrate the process-global state from disk on startup. Does NOT auto-spawn
/// the sidecar; the manager / `ts_enable` does that.
pub fn init_from_disk() {
    let cfg = load_config();
    let s = shared();
    *s.enabled.write() = cfg.enabled;
    *s.socks_addr.write() = cfg.socks_addr;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_with_state_tag() {
        let v = serde_json::to_value(TsStatus::NeedsLogin { url: "https://x".into() }).unwrap();
        assert_eq!(v.get("state").and_then(|s| s.as_str()), Some("needs_login"));
        assert_eq!(v.get("url").and_then(|s| s.as_str()), Some("https://x"));

        let v = serde_json::to_value(TsStatus::Connected {
            ip: "100.1.2.3".into(),
            dnsname: "cortex.tail.ts.net".into(),
        })
        .unwrap();
        assert_eq!(v.get("state").and_then(|s| s.as_str()), Some("connected"));
        assert_eq!(v.get("ip").and_then(|s| s.as_str()), Some("100.1.2.3"));
    }

    #[test]
    fn proxy_passthrough_when_inactive() {
        // Default state is disabled+disconnected → builder must be unchanged
        // (we can't easily assert builder equality, but the call must not panic
        // and must return a usable builder).
        let b = maybe_tailscale_proxy(reqwest::Client::builder());
        assert!(b.build().is_ok());
    }
}
