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

    // If an external SOCKS5 proxy is configured (e.g. Tailscale in WSL), route
    // through it and never start the embedded sidecar.
    if let Some(addr) = tailscale::external_socks_addr() {
        tracing::info!(
            "tailscale: external SOCKS5 proxy configured ({addr}) — enable is a no-op (embedded sidecar NOT started)"
        );
        return Ok(tailscale::current_status());
    }

    // If the OS already runs a system Tailscale, the machine is on the tailnet
    // directly: don't spin up the embedded sidecar. Home/tailnet traffic reaches
    // hosts directly (`maybe_tailscale_proxy` is a no-op when `prefer_system()`).
    if tailscale::prefer_system() {
        tracing::info!(
            "tailscale: system Tailscale detected — enable is a no-op (using it directly, embedded sidecar NOT started)"
        );
        return Ok(tailscale::current_status());
    }

    // Prefer the just-passed key, else whatever's in the keychain.
    let key = authkey
        .filter(|k| !k.trim().is_empty())
        .or_else(tailscale::get_authkey);

    tracing::info!("tailscale: no system Tailscale — starting embedded tsnet sidecar (socks5h proxy)");
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

/// The external SOCKS5 proxy (`host:port`), if configured — the address Cortex
/// routes home traffic through instead of the embedded sidecar (e.g. Tailscale
/// running in WSL). Empty string means unset.
#[tauri::command]
pub async fn ts_get_external_socks() -> Result<String, String> {
    Ok(tailscale::external_socks_addr().unwrap_or_default())
}

/// Set or clear (empty string) the external SOCKS5 proxy. When set, the embedded
/// sidecar is never started and home traffic routes through this address.
#[tauri::command]
pub async fn ts_set_external_socks(addr: String) -> Result<(), String> {
    let value = if addr.trim().is_empty() { None } else { Some(addr) };
    tailscale::set_external_socks(value).map_err(|e| e.to_string())
}

/// One-click: install + run Tailscale inside WSL, point Cortex's external SOCKS5
/// at it, and bring the node up (returns a login URL if it needs authorising).
/// The heavy lifting (download/extract/spawn) runs on a blocking thread.
#[tauri::command]
pub async fn ts_wsl_setup() -> Result<tailscale::wsl::WslTsStatus, String> {
    tokio::task::spawn_blocking(tailscale::wsl::setup)
        .await
        .map_err(|e| format!("wsl setup task failed: {e}"))?
}

/// Current WSL-Tailscale status (availability, daemon, connection, WSL IP).
#[tauri::command]
pub async fn ts_wsl_status() -> Result<tailscale::wsl::WslTsStatus, String> {
    Ok(tokio::task::spawn_blocking(tailscale::wsl::status)
        .await
        .map_err(|e| format!("wsl status task failed: {e}"))?)
}

/// Stop the WSL-Tailscale daemon Cortex is holding.
#[tauri::command]
pub async fn ts_wsl_stop() -> Result<(), String> {
    tailscale::wsl::stop();
    Ok(())
}

/// A phone-pairing payload for the Tailscale-fronted mobile server: the
/// reachable URL plus a QR-code SVG the Settings panel renders so a phone on
/// the tailnet can scan instead of typing the URL.
#[derive(Debug, serde::Serialize)]
pub struct MobilePairing {
    /// `https://<magicdns-name>/` — the tailnet-authenticated mobile entry point.
    pub url: String,
    /// Inline SVG of the QR encoding `url` (theme-neutral: currentColor-ready
    /// black modules on transparent, sized by the caller's container).
    pub qr_svg: String,
}

/// Derive the mobile pairing URL from live Tailscale status and render its QR.
///
/// Only meaningful once the node is `Connected` (MagicDNS name assigned and
/// `tailscale serve` fronting the loopback mobile server on 443). Any other
/// state returns a humanized error the UI shows as guidance rather than a QR.
#[tauri::command]
pub async fn ts_mobile_pairing() -> Result<MobilePairing, String> {
    let dnsname = match tailscale::current_status() {
        TsStatus::Connected { dnsname, .. } if !dnsname.trim().is_empty() => dnsname,
        TsStatus::Connected { .. } => {
            return Err("Tailscale is connected but hasn't been assigned a MagicDNS name yet — retry in a moment.".into())
        }
        TsStatus::NeedsLogin { .. } => {
            return Err("Finish the Tailscale login first, then pair your phone.".into())
        }
        _ => {
            return Err("Enable Tailscale and wait for it to connect before pairing a device.".into())
        }
    };
    Ok(build_mobile_pairing(&dnsname))
}

/// Pure builder — URL shaping + QR-SVG render — split out so it is unit-testable
/// without a live tailnet.
fn build_mobile_pairing(dnsname: &str) -> MobilePairing {
    let url = format!("https://{}/", dnsname.trim().trim_end_matches('/'));
    let qr_svg = render_qr_svg(&url);
    MobilePairing { url, qr_svg }
}

/// Encode `data` as a QR code and return a standalone SVG string. On the
/// (practically impossible for a short URL) encode failure, returns a tiny
/// placeholder SVG so the caller never has to handle a second error path.
fn render_qr_svg(data: &str) -> String {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};
    match QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M) {
        Ok(code) => code
            .render::<svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
        Err(_) => {
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_url_is_https_magicdns_root() {
        let p = build_mobile_pairing("cortex.tail1234.ts.net");
        assert_eq!(p.url, "https://cortex.tail1234.ts.net/");
    }

    #[test]
    fn pairing_url_normalizes_trailing_slash() {
        let p = build_mobile_pairing("cortex.tail1234.ts.net/");
        assert_eq!(p.url, "https://cortex.tail1234.ts.net/");
    }

    #[test]
    fn qr_svg_is_nonempty_svg_encoding_the_url() {
        let p = build_mobile_pairing("cortex.tail1234.ts.net");
        assert!(p.qr_svg.starts_with("<?xml") || p.qr_svg.starts_with("<svg"));
        assert!(p.qr_svg.contains("<svg"));
        // A real QR of a ~30-char URL is substantial, never the 1x1 placeholder.
        assert!(p.qr_svg.len() > 500, "unexpectedly tiny QR svg: {}", p.qr_svg.len());
    }
}
