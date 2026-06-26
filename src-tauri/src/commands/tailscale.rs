//! Tauri commands for embedded Tailscale.
//!
//! Thin wrappers over [`crate::tailscale`] + [`crate::tailscale::manager`] that
//! the frontend invokes. The auth key never crosses the bridge in responses;
//! `ts_set_authkey`/`ts_enable` accept it and stash it in the OS keychain.

use crate::tailscale::{self, manager, TsStatus};

/// Default tailnet hostname for the embedded node.
const DEFAULT_HOSTNAME: &str = "cortex";

/// Enable embedded Tailscale: optionally store a fresh auth key, persist the
/// enabled flag, spawn the sidecar, and return the (initial) status.
///
/// The status returned is whatever the sidecar has reported so far — typically
/// `Disconnected` immediately, transitioning to `NeedsLogin`/`Connected`
/// asynchronously. Poll [`ts_status`] for updates.
#[tauri::command]
pub async fn ts_enable(authkey: Option<String>) -> Result<TsStatus, String> {
    if let Some(key) = authkey.as_deref().filter(|k| !k.trim().is_empty()) {
        tailscale::set_authkey(key).map_err(|e| e.to_string())?;
    }

    let socks = tailscale::socks_addr();

    // Persist enabled = true (+ current socks addr).
    let mut cfg = tailscale::load_config();
    cfg.enabled = true;
    cfg.socks_addr = socks.clone();
    tailscale::save_config(&cfg).map_err(|e| e.to_string())?;
    *tailscale::shared().enabled.write() = true;

    // Prefer the just-passed key, else whatever's in the keychain.
    let key = authkey
        .filter(|k| !k.trim().is_empty())
        .or_else(tailscale::get_authkey);

    manager::start(key, &socks, DEFAULT_HOSTNAME)?;
    Ok(tailscale::current_status())
}

/// Disable embedded Tailscale: stop the sidecar and persist the disabled flag.
#[tauri::command]
pub async fn ts_disable() -> Result<(), String> {
    manager::stop();
    *tailscale::shared().enabled.write() = false;
    let mut cfg = tailscale::load_config();
    cfg.enabled = false;
    tailscale::save_config(&cfg).map_err(|e| e.to_string())?;
    Ok(())
}

/// Current embedded-Tailscale status (includes the login URL when `NeedsLogin`).
#[tauri::command]
pub async fn ts_status() -> Result<TsStatus, String> {
    Ok(tailscale::current_status())
}

/// Store a tailnet auth key in the OS keychain (never logged).
#[tauri::command]
pub async fn ts_set_authkey(key: String) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("auth key cannot be empty".into());
    }
    tailscale::set_authkey(&key).map_err(|e| e.to_string())
}

/// The local SOCKS5 address (`host:port`) the sidecar listens on.
#[tauri::command]
pub async fn ts_get_socks_addr() -> Result<String, String> {
    Ok(tailscale::socks_addr())
}
