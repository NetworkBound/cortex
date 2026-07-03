//! One-click Tailscale-in-WSL setup for locked-down Windows machines.
//!
//! On a machine with no admin rights (can't install the system Tailscale) and
//! no antivirus-exclusion rights (the embedded Go sidecar gets quarantined),
//! the remaining path to the tailnet is Tailscale running *inside WSL*, exposing
//! a SOCKS5 proxy that Cortex dials via [`super::external_socks_addr`].
//!
//! This module automates that: it installs a rootless Tailscale static build in
//! the user's WSL distro, holds a `tailscaled` child (userspace-networking +
//! SOCKS5) alive for the app's lifetime — exactly like [`super::manager`] holds
//! the embedded sidecar — runs `tailscale up` (surfacing the login URL), reads
//! back the WSL IP, and points Cortex's external SOCKS5 setting at it.
//!
//! Persistence note: a bare `setsid`/`nohup` daemon does NOT survive in WSL2 —
//! the distro's lightweight VM tears down once the last session ends. Holding a
//! `wsl.exe` child process open (its lifetime == `tailscaled`'s) is what keeps
//! it running, so we manage it as a child just like the native sidecar.

use parking_lot::Mutex;
use serde::Serialize;
use std::process::{Child, Stdio};
use std::sync::Arc;

/// SOCKS5 port opened inside WSL (bound to 0.0.0.0 so Windows can reach it via
/// the WSL IP even in NAT mode).
const WSL_SOCKS_PORT: u16 = 1055;
/// Per-user working dir inside the WSL distro (binaries + tailscaled state).
const WSL_DIR: &str = "$HOME/.cortex-ts";

/// The held `wsl.exe` child running `tailscaled`, if any.
static CHILD: once_cell::sync::Lazy<Arc<Mutex<Option<Child>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

/// Result of a setup / status query, surfaced to the Settings UI.
#[derive(Debug, Clone, Serialize, Default)]
pub struct WslTsStatus {
    /// WSL is installed and a default distro responds.
    pub wsl_available: bool,
    /// `tailscaled` child is currently held + alive.
    pub daemon_running: bool,
    /// The node is authenticated + on the tailnet.
    pub connected: bool,
    /// WSL distro IP (`host:port` is what the user puts in the proxy field).
    pub wsl_ip: Option<String>,
    /// The `host:port` Cortex is (or should be) pointed at.
    pub proxy_addr: Option<String>,
    /// Login URL when the node still needs interactive authorisation.
    pub login_url: Option<String>,
    /// The node's tailnet IP once connected.
    pub tailnet_ip: Option<String>,
}

/// Ensure `$HOME` is set. When Cortex (a GUI process) spawns `wsl.exe`, the
/// child bash may inherit an empty `HOME`, which would make `$HOME/...` paths
/// expand to `/...`. Resolve it from the passwd db as a preamble on every
/// script so `WSL_DIR` is always valid.
const HOME_PREAMBLE: &str = r#"export HOME="${HOME:-$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f6)}"; export HOME="${HOME:-/home/$(id -un)}"; "#;

/// Wrap a script with the HOME-resolution preamble.
fn with_home(script: &str) -> String {
    format!("{HOME_PREAMBLE}{script}")
}

/// Run a bash script inside the default WSL distro and capture stdout.
/// Returns Err on a non-zero exit, with stderr as the message. Never pops a
/// console window (`sys::no_window`).
fn wsl_bash(script: &str) -> Result<String, String> {
    let out = crate::sys::no_window("wsl.exe")
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg(with_home(script))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("wsl.exe not available: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err.trim();
        return Err(if msg.is_empty() {
            format!("WSL command failed (exit {:?})", out.status.code())
        } else {
            msg.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether WSL is installed and a default distro answers.
pub fn available() -> bool {
    wsl_bash("echo ok").map(|s| s.contains("ok")).unwrap_or(false)
}

/// The default distro's primary IP (what the user dials from Windows).
fn wsl_ip() -> Option<String> {
    wsl_bash("hostname -I | awk '{print $1}'")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Install a rootless Tailscale static build inside WSL if not already present.
/// Idempotent: re-running is a fast no-op once the binary exists.
fn install() -> Result<(), String> {
    // Resolve the latest amd64 tarball name from the stable channel, download +
    // extract into WSL_DIR, and symlink stable `tailscale`/`tailscaled` names.
    let script = format!(
        r#"set -e
D="{dir}"
mkdir -p "$D"
if [ -x "$D/bin/tailscaled" ]; then echo "already-installed"; exit 0; fi
TARBALL=$(curl -fsSL "https://pkgs.tailscale.com/stable/?mode=json" | grep -oE 'tailscale_[0-9.]+_amd64\.tgz' | head -1)
if [ -z "$TARBALL" ]; then echo "could not resolve tailscale version" >&2; exit 1; fi
curl -fsSL "https://pkgs.tailscale.com/stable/$TARBALL" -o "$D/ts.tgz"
tar xzf "$D/ts.tgz" -C "$D"
SUB=$(find "$D" -maxdepth 1 -type d -name 'tailscale_*_amd64' | head -1)
mkdir -p "$D/bin"
ln -sf "$SUB/tailscaled" "$D/bin/tailscaled"
ln -sf "$SUB/tailscale" "$D/bin/tailscale"
echo "installed"
"#,
        dir = WSL_DIR
    );
    wsl_bash(&script).map(|_| ())
}

/// Spawn + hold the `tailscaled` child (userspace networking + SOCKS5). No-op if
/// one is already alive.
fn start_daemon() -> Result<(), String> {
    let mut guard = CHILD.lock();
    if let Some(child) = guard.as_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            return Ok(()); // already running
        }
    }
    // Foreground `exec` so this held wsl.exe child's lifetime == tailscaled's.
    let inner = format!(
        "mkdir -p {dir}/state; exec {dir}/bin/tailscaled \
         --tun=userspace-networking \
         --socks5-server=0.0.0.0:{port} \
         --statedir={dir}/state \
         --socket={dir}/tailscaled.sock",
        dir = WSL_DIR,
        port = WSL_SOCKS_PORT
    );
    let child = crate::sys::no_window("wsl.exe")
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg(with_home(&inner))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start tailscaled in WSL: {e}"))?;
    *guard = Some(child);
    Ok(())
}

/// True if the held daemon child is alive.
fn daemon_running() -> bool {
    let mut guard = CHILD.lock();
    match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

/// Bring the node up. Returns a login URL when interactive auth is still needed,
/// or `None` once connected. Uses a short timeout so we never block the UI.
fn up() -> Result<Option<String>, String> {
    let script = format!(
        "{dir}/bin/tailscale --socket={dir}/tailscaled.sock up \
         --hostname=cortex-wsl --accept-routes --timeout=10s 2>&1 || true",
        dir = WSL_DIR
    );
    let out = wsl_bash(&script)?;
    // `tailscale up` prints the login URL when the node isn't authorised yet.
    if let Some(url) = out
        .lines()
        .find(|l| l.contains("https://login.tailscale.com/"))
        .map(|l| l.trim().to_string())
    {
        return Ok(Some(url));
    }
    Ok(None)
}

/// The node's tailnet IP, if connected.
fn tailnet_ip() -> Option<String> {
    let script = format!("{dir}/bin/tailscale --socket={dir}/tailscaled.sock ip -4 2>/dev/null | head -1", dir = WSL_DIR);
    wsl_bash(&script)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("100."))
}

/// Full status snapshot for the Settings UI (does not start anything).
pub fn status() -> WslTsStatus {
    let wsl_available = available();
    let daemon_running = daemon_running();
    let ip = wsl_ip();
    let tnet = if daemon_running { tailnet_ip() } else { None };
    let proxy = ip.as_ref().map(|i| format!("{i}:{WSL_SOCKS_PORT}"));
    WslTsStatus {
        wsl_available,
        daemon_running,
        connected: tnet.is_some(),
        wsl_ip: ip,
        proxy_addr: proxy,
        login_url: None,
        tailnet_ip: tnet,
    }
}

/// One-click setup: install → start daemon → point Cortex's external SOCKS5 at
/// the WSL IP → `up`. Returns the resulting status (with a login URL if the node
/// still needs authorising). Idempotent and safe to re-run.
pub fn setup() -> Result<WslTsStatus, String> {
    if !available() {
        return Err("WSL is not installed or no default distro is available. Install WSL (wsl --install) and try again.".into());
    }
    install()?;
    start_daemon()?;
    // Give tailscaled a moment to bind its socket before we talk to it.
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let ip = wsl_ip().ok_or_else(|| "could not determine the WSL IP address".to_string())?;
    let proxy = format!("{ip}:{WSL_SOCKS_PORT}");
    // Point Cortex at the WSL proxy (persists + updates the live state).
    super::set_external_socks(Some(proxy.clone())).map_err(|e| e.to_string())?;

    let login_url = up()?;
    let tnet = tailnet_ip();
    Ok(WslTsStatus {
        wsl_available: true,
        daemon_running: daemon_running(),
        connected: tnet.is_some(),
        wsl_ip: Some(ip),
        proxy_addr: Some(proxy),
        login_url,
        tailnet_ip: tnet,
    })
}

/// Stop the held `tailscaled` child (best-effort). Called on app exit and when
/// the user turns the WSL proxy off.
pub fn stop() {
    let mut guard = CHILD.lock();
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}
