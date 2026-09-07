//! In-process TCP forwarder for the `remote_client` ("Cortex Home") build.
//!
//! WebView2's proxy is fixed at environment-init and can't be per-window or
//! updated live, so the app points it at a STABLE local address
//! (`127.0.0.1:1055`). This bridges that to the WSL Tailscale SOCKS5 proxy,
//! which lives at a *dynamic* `<wsl-ip>:1055` (the WSL IP changes between
//! sessions and, in NAT mode, is NOT reachable at `127.0.0.1` from Windows).
//!
//! Robustness: it resolves the current WSL IP itself (`wsl hostname -I`) and
//! waits until the proxy actually accepts a connection before it starts
//! bridging — so a slow/first-run tunnel bring-up just delays the first paint
//! instead of failing. It re-resolves if a bridged connection can't reach the
//! upstream (WSL IP changed mid-session). No admin, no external deps.

use std::io::copy;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const SOCKS_PORT: u16 = 1055;

/// Resolve the default WSL distro's IPv4. Minimal distros may lack `hostname`,
/// so try three ways: `hostname -I`, then `ip addr`, then `ip route`.
fn wsl_ip() -> Option<String> {
    // `ip route get` yields the src IP of the interface that actually reaches
    // out (eth0) — the one Windows reaches WSL on — so try it first. `hostname`
    // is next (absent on minimal distros). The bare `ip addr` scan is last and
    // must exclude `lo`, whose 10.255.255.254/32 is marked scope-global on WSL2
    // but is NOT reachable from Windows.
    let attempts: [&[&str]; 3] = [
        &["sh", "-c", "ip route get 1.1.1.1 2>/dev/null | grep -oE 'src [0-9.]+' | awk '{print $2}'"],
        &["hostname", "-I"],
        &["sh", "-c", "ip -4 -o addr show scope global 2>/dev/null | grep -vw lo | awk '{print $4}' | cut -d/ -f1"],
    ];
    let lo_quirk = Ipv4Addr::new(10, 255, 255, 254);
    for args in attempts {
        let Ok(out) = crate::sys::no_window("wsl.exe").args(args).output() else {
            continue;
        };
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(ip) = s
            .split_whitespace()
            .filter_map(|t| t.parse::<Ipv4Addr>().ok())
            .find(|ip| !ip.is_loopback() && *ip != lo_quirk)
        {
            return Some(ip.to_string());
        }
    }
    None
}

/// Current reachable upstream `<wsl-ip>:1055`, or None if not reachable yet.
fn upstream() -> Option<String> {
    let addr = format!("{}:{SOCKS_PORT}", wsl_ip()?);
    // A TCP connect proves the SOCKS listener is up (the daemon binds it).
    TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_secs(2))
        .ok()
        .map(|_| addr)
}

/// Bind `listen` (127.0.0.1:1055) and forward every connection to the live WSL
/// proxy. Blocks. Waits (bounded) for the tunnel to come up first.
pub fn run(listen: &str) {
    let listener = match TcpListener::bind(listen) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("remote_forward: bind {listen} failed: {e}");
            return;
        }
    };
    // Wait for the WSL proxy to accept connections (first run installs + auths
    // Tailscale, which can take a while). Up to ~3 min, then serve anyway and
    // resolve per-connection.
    let mut initial = None;
    for _ in 0..90 {
        if let Some(u) = upstream() {
            initial = Some(u);
            break;
        }
        thread::sleep(Duration::from_secs(2));
    }
    match &initial {
        Some(u) => tracing::info!("remote_forward: {listen} -> {u} (WSL proxy live)"),
        None => tracing::warn!("remote_forward: WSL proxy not reachable yet; will resolve per-connection"),
    }

    for incoming in listener.incoming() {
        let Ok(client) = incoming else { continue };
        thread::spawn(move || {
            // Resolve fresh (cheap cache miss is fine); the WSL IP can change.
            let Some(up) = upstream() else { return };
            let Ok(server) = TcpStream::connect(&up) else { return };
            let (mut cr, mut sw) = match (client.try_clone(), server.try_clone()) {
                (Ok(a), Ok(b)) => (a, b),
                _ => return,
            };
            let t = thread::spawn(move || {
                let _ = copy(&mut cr, &mut sw);
            });
            let (mut sr, mut cw) = (server, client);
            let _ = copy(&mut sr, &mut cw);
            let _ = t.join();
        });
    }
}
