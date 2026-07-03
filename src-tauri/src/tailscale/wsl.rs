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

/// Resolve `$HOME` from the passwd db. When Cortex (a GUI process) spawns
/// `wsl.exe`, the child bash inherits an EMPTY `HOME`, so `$HOME/...` would
/// expand to `/...`. Every script starts with this so `WSL_DIR` is always valid.
/// (Scripts are run from a file — see [`run_script`] — because passing a complex
/// script inline through `wsl.exe` mangles quoting; a file is read verbatim.)
const HOME_PREAMBLE: &str = "export HOME=\"${HOME:-$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f6)}\"\nexport HOME=\"${HOME:-/home/$(id -un)}\"\n";

/// Convert a Windows path (`C:\a\b`) to its WSL automount path (`/mnt/c/a/b`).
fn win_to_wsl_path(p: &std::path::Path) -> Option<String> {
    let s = p.to_str()?;
    let bytes = s.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' {
        return None;
    }
    let drive = (bytes[0] as char).to_ascii_lowercase();
    let rest = s[2..].replace('\\', "/");
    Some(format!("/mnt/{drive}{rest}"))
}

/// Write a bash script to a Windows temp file (LF line endings) and run it in the
/// default WSL distro via its `/mnt/c` path — only a simple path is passed
/// inline, so nothing gets mangled. Returns stdout, or Err(stderr) on failure.
/// `keep` leaves the file on disk (for the long-lived daemon launcher).
fn run_script(body: &str, keep: bool) -> Result<String, String> {
    let path = write_script(body)?;
    let wsl_path = win_to_wsl_path(&path)
        .ok_or_else(|| "could not map temp path into WSL".to_string())?;
    let out = crate::sys::no_window("wsl.exe")
        .arg("--")
        .arg("bash")
        .arg(&wsl_path)
        .stdin(Stdio::null())
        .output();
    if !keep {
        let _ = std::fs::remove_file(&path);
    }
    let out = out.map_err(|e| format!("wsl.exe not available: {e}"))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if msg.is_empty() {
            format!("WSL command failed (exit {:?})", out.status.code())
        } else {
            msg
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Write `body` (prefixed with the HOME preamble, LF-normalised) to a uniquely
/// named temp `.sh` and return its Windows path.
fn write_script(body: &str) -> Result<std::path::PathBuf, String> {
    let full = format!("#!/bin/bash\n{HOME_PREAMBLE}{body}").replace("\r\n", "\n");
    let mut path = std::env::temp_dir();
    // Unique-ish name without pulling in extra deps: pid + a monotonic counter.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.push(format!("cortex-wsl-{}-{}.sh", std::process::id(), n));
    std::fs::write(&path, full).map_err(|e| format!("cannot write temp script: {e}"))?;
    Ok(path)
}

/// Whether WSL is installed and a default distro answers.
pub fn available() -> bool {
    run_script("echo ok", false).map(|s| s.contains("ok")).unwrap_or(false)
}

/// Candidate addresses the Windows side might reach the WSL SOCKS5 at. In NAT
/// mode it's the distro's own IP; in *mirrored* networking mode localhost is
/// shared (and `hostname -I` yields nothing useful), so `127.0.0.1` is included
/// as a fallback. Order: distro IPs first, then loopback.
fn candidate_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    // Two detection methods — distros/network modes differ; take any IPv4s.
    let probe = "ip route get 1.1.1.1 2>/dev/null | grep -oE 'src [0-9.]+' | awk '{print $2}'; \
                 hostname -I 2>/dev/null";
    if let Ok(out) = run_script(probe, false) {
        for tok in out.split_whitespace() {
            let t = tok.trim();
            if t.contains('.') && t != "127.0.0.1" && !hosts.iter().any(|h| h == t) {
                hosts.push(t.to_string());
            }
        }
    }
    hosts.push("127.0.0.1".to_string());
    hosts
}

/// The distro's primary IP, best-effort (used for display; may be absent in
/// mirrored networking mode).
fn wsl_ip() -> Option<String> {
    candidate_hosts().into_iter().find(|h| h != "127.0.0.1")
}

/// Pick the first candidate `host:PORT` that actually accepts a TCP connection
/// from Windows — this directly validates the address Cortex will dial, across
/// NAT vs mirrored networking. Retries briefly so a just-started daemon has time
/// to bind. Returns `None` only if nothing is reachable.
fn reachable_proxy(port: u16) -> Option<String> {
    use std::net::{TcpStream, ToSocketAddrs};
    let hosts = candidate_hosts();
    for _ in 0..6 {
        for host in &hosts {
            let addr = format!("{host}:{port}");
            if let Ok(mut sas) = addr.to_socket_addrs() {
                if let Some(sa) = sas.next() {
                    if TcpStream::connect_timeout(&sa, std::time::Duration::from_millis(600)).is_ok() {
                        return Some(addr);
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    None
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
    run_script(&script, false).map(|_| ())
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
    // Reap any stale daemon first: if Cortex was force-killed or crashed, the
    // previous held child leaked and would still hold the SOCKS port, so the
    // fresh `exec` below would fail to bind. Then `exec` in the foreground so
    // this held wsl.exe child's lifetime == tailscaled's. Run from a kept script
    // file (reliable arg passing) via its /mnt path.
    let body = format!(
        "pkill -f '\\.cortex-ts/bin/tailscaled' 2>/dev/null || true\nsleep 0.3\n\
         mkdir -p {dir}/state\nexec {dir}/bin/tailscaled \
         --tun=userspace-networking \
         --socks5-server=0.0.0.0:{port} \
         --statedir={dir}/state \
         --socket={dir}/tailscaled.sock\n",
        dir = WSL_DIR,
        port = WSL_SOCKS_PORT
    );
    let path = write_script(&body)?;
    let wsl_path = win_to_wsl_path(&path)
        .ok_or_else(|| "could not map temp path into WSL".to_string())?;
    let child = crate::sys::no_window("wsl.exe")
        .arg("--")
        .arg("bash")
        .arg(&wsl_path)
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
    let out = run_script(&script, false)?;
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
    run_script(&script, false)
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
    // Give tailscaled a moment to bind its socket before we probe it.
    std::thread::sleep(std::time::Duration::from_millis(1200));

    // Pick the address that actually reaches the proxy from Windows (handles NAT
    // vs mirrored networking). Fall back to a best-guess IP, then loopback, so we
    // never hard-fail — the user can adjust the field if needed.
    let proxy = reachable_proxy(WSL_SOCKS_PORT)
        .or_else(|| wsl_ip().map(|ip| format!("{ip}:{WSL_SOCKS_PORT}")))
        .unwrap_or_else(|| format!("127.0.0.1:{WSL_SOCKS_PORT}"));
    // Point Cortex at the WSL proxy (persists + updates the live state).
    super::set_external_socks(Some(proxy.clone())).map_err(|e| e.to_string())?;

    let login_url = up()?;
    let tnet = tailnet_ip();
    Ok(WslTsStatus {
        wsl_available: true,
        daemon_running: daemon_running(),
        connected: tnet.is_some(),
        wsl_ip: proxy.rsplit_once(':').map(|(h, _)| h.to_string()),
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
