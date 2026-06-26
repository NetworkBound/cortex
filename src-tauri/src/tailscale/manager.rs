//! Lifecycle manager for the `cortex-tsnet` sidecar.
//!
//! Spawns the Go sidecar with `crate::sys::no_window` (no console flash on
//! Windows), reads its stdout status protocol on a background thread, and
//! mirrors the parsed state into [`super::shared`]. Holds the child handle so
//! [`stop`] can kill it.

use super::{shared, TsStatus};
use parking_lot::Mutex;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::Arc;

/// The single running sidecar process, if any.
static CHILD: once_cell::sync::Lazy<Arc<Mutex<Option<Child>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

/// Resolve the path to the `cortex-tsnet` binary.
///
/// - Dev: `<repo>/sidecar/cortex-tsnet/cortex-tsnet` (and `.exe` on Windows),
///   discovered by walking up from `CARGO_MANIFEST_DIR` / the current exe.
/// - Bundled: the Tauri resource/sidecar dir next to the app binary, where the
///   externalBin lands named `cortex-tsnet` (Tauri strips the target triple at
///   bundle time).
///
/// Returns a clear error (never panics) if no candidate exists.
pub fn sidecar_path() -> Result<PathBuf, String> {
    let exe_name = if cfg!(windows) { "cortex-tsnet.exe" } else { "cortex-tsnet" };

    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Bundled: alongside the current executable (Tauri sidecar location).
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            candidates.push(dir.join(exe_name));
        }
    }

    // 2. Dev: walk up from the manifest dir looking for sidecar/cortex-tsnet.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // .../src-tauri
    let mut walk = Some(manifest.as_path());
    while let Some(dir) = walk {
        let cand = dir.join("sidecar").join("cortex-tsnet").join(exe_name);
        candidates.push(cand);
        walk = dir.parent();
    }

    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }

    Err(format!(
        "tsnet sidecar not built — expected `{exe_name}` in the app resource dir \
         or `sidecar/cortex-tsnet/`. Run `cd sidecar/cortex-tsnet && go build -o {exe_name} .`"
    ))
}

/// Default tsnet state dir: `~/.cortex/tsnet/<hostname>`.
fn state_dir(hostname: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".cortex")
        .join("tsnet")
        .join(hostname)
}

/// Start the sidecar if it isn't already running. `authkey` (if any) is passed
/// via the `TS_AUTHKEY` env var only — never on the command line, never logged.
/// Sets status to `Disconnected` on a clean (re)start and lets the status reader
/// update it from the sidecar's stdout.
pub fn start(authkey: Option<String>, socks_addr: &str, hostname: &str) -> Result<(), String> {
    let mut guard = CHILD.lock();
    if let Some(child) = guard.as_mut() {
        // Already running and alive? No-op.
        if matches!(child.try_wait(), Ok(None)) {
            return Ok(());
        }
    }

    let bin = sidecar_path()?;
    let dir = state_dir(hostname);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create state dir: {e}"))?;

    let mut cmd = crate::sys::no_window(&bin);
    cmd.arg("--hostname")
        .arg(hostname)
        .arg("--state-dir")
        .arg(&dir)
        .arg("--socks")
        .arg(socks_addr)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    if let Some(key) = authkey.as_deref().filter(|k| !k.trim().is_empty()) {
        cmd.env("TS_AUTHKEY", key);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn tsnet sidecar: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "sidecar stdout unavailable".to_string())?;

    *shared().status.write() = TsStatus::Disconnected;
    *guard = Some(child);
    drop(guard);

    // Read status lines on a detached thread for the life of the process.
    std::thread::Builder::new()
        .name("cortex-tsnet-status".into())
        .spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(st) = parse_status_line(line) {
                    *shared().status.write() = st;
                }
            }
            // stdout closed → process exited. If we still think we're connected,
            // demote to Disconnected (unless an Error was the last word).
            let mut cur = shared().status.write();
            if matches!(*cur, TsStatus::Connected { .. } | TsStatus::NeedsLogin { .. }) {
                *cur = TsStatus::Disconnected;
            }
        })
        .map_err(|e| format!("failed to spawn status reader: {e}"))?;

    Ok(())
}

/// Parse one stdout JSON line from the sidecar into a [`TsStatus`].
///
/// The sidecar's wire `state` values are kebab/lowercase
/// (`starting | needs-login | connected | error`); map them onto our enum.
/// `starting` maps to `Disconnected` (transient, pre-auth).
fn parse_status_line(line: &str) -> Option<TsStatus> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let state = v.get("state")?.as_str()?;
    let s = match state {
        "starting" => TsStatus::Disconnected,
        "needs-login" => TsStatus::NeedsLogin {
            url: v.get("url").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        },
        "connected" => TsStatus::Connected {
            ip: v.get("ip").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            dnsname: v.get("dnsname").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        },
        "error" => TsStatus::Error {
            msg: v.get("msg").and_then(|x| x.as_str()).unwrap_or("unknown error").to_string(),
        },
        _ => return None,
    };
    Some(s)
}

/// Stop the sidecar if running. Idempotent. Resets status to `Disconnected`.
pub fn stop() {
    let mut guard = CHILD.lock();
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *shared().status.write() = TsStatus::Disconnected;
}

/// Is the sidecar process currently running?
pub fn is_running() -> bool {
    let mut guard = CHILD.lock();
    match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_state() {
        assert!(matches!(
            parse_status_line(r#"{"state":"starting"}"#),
            Some(TsStatus::Disconnected)
        ));
        assert!(matches!(
            parse_status_line(r#"{"state":"needs-login","url":"https://login.tailscale.com/a/x"}"#),
            Some(TsStatus::NeedsLogin { url }) if url.contains("login.tailscale.com")
        ));
        assert!(matches!(
            parse_status_line(r#"{"state":"connected","ip":"100.1.2.3","dnsname":"cortex.t.ts.net"}"#),
            Some(TsStatus::Connected { ip, dnsname }) if ip == "100.1.2.3" && dnsname == "cortex.t.ts.net"
        ));
        assert!(matches!(
            parse_status_line(r#"{"state":"error","msg":"boom"}"#),
            Some(TsStatus::Error { msg }) if msg == "boom"
        ));
        assert!(parse_status_line("not json").is_none());
        assert!(parse_status_line(r#"{"state":"weird"}"#).is_none());
    }
}
