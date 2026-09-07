//! Tauri commands for automatic chat-history sync.
//!
//! Surface for the Settings "History Sync" section. Thin wrappers over
//! [`crate::history_sync`]; the session cookie/token never crosses the bridge in
//! a response.
//!
//! The login-fallback (`history_sync_connect`) no longer tries to *extract* the
//! provider's session cookie (HttpOnly / App-Bound-Encrypted, unreliable on
//! Windows/WebView2). Instead it opens a Tauri webview at the provider's **main
//! app**, lets the user land logged-in, then injects JS that fetches every
//! conversation through the provider's own API **inside that authenticated
//! webview** and hands the JSON back to Rust for the existing import pipeline.
//! See [`crate::history_sync::webview_fetch`] for the data-return mechanism.

use tauri::{AppHandle, State};

use crate::history_sync::{self, cookies::WebProvider};
use crate::observability::tracing_store::TracingStore;

/// Per-provider status returned to the Settings UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderSyncStatus {
    /// Canonical key: `"claude"` | `"chatgpt"`.
    pub provider: String,
    /// Human label for the UI.
    pub label: String,
    pub enabled: bool,
    /// Epoch-millis of the last sync, if any.
    pub last_sync: Option<i64>,
    /// Number of conversations imported from this provider so far.
    pub conversation_count: i64,
    /// `"browser"` | `"login"` | null — where the working session came from.
    pub session_source: Option<String>,
    /// True when no session is auto-detectable and none is stored, so the UI
    /// should show a "Connect / Sign in" button.
    pub needs_login: bool,
}

/// The providers that have a web chat history we can sync.
const PROVIDERS: &[(&str, &str)] = &[("claude", "Claude"), ("chatgpt", "ChatGPT")];

fn source_str(s: Option<crate::history_sync::config::SessionSource>) -> Option<String> {
    use crate::history_sync::config::SessionSource;
    s.map(|s| match s {
        SessionSource::Browser => "browser".to_string(),
        SessionSource::Login => "login".to_string(),
    })
}

/// Enable/disable automatic sync for a provider. Enabling spawns an immediate
/// sync + the recurring loop; disabling persists the flag (the loop self-exits).
#[tauri::command]
pub async fn history_sync_set_enabled(
    provider: String,
    enabled: bool,
    app: AppHandle,
    store: State<'_, TracingStore>,
) -> Result<(), String> {
    // Validate the provider key.
    let _ = history_sync::parse_provider(&provider)?;

    let mut cfg = history_sync::config::load();
    cfg.entry(&provider).enabled = enabled;
    history_sync::config::save(&cfg)?;

    if enabled {
        // Kick off an immediate sync + the recurring background loop.
        history_sync::scheduler::spawn_provider_loop(app, provider, store.inner().clone());
    }
    Ok(())
}

/// Current sync status for every web-history provider.
#[tauri::command]
pub async fn history_sync_status(
    store: State<'_, TracingStore>,
) -> Result<Vec<ProviderSyncStatus>, String> {
    let cfg = history_sync::config::load();
    let mut out = Vec::with_capacity(PROVIDERS.len());
    for (key, label) in PROVIDERS {
        let (enabled, last_sync, source) = history_sync::status_for(&cfg, key);
        let conversation_count = history_sync::imported_conversation_count(&store, key);
        // needs_login: enabled but we have neither a detectable browser session
        // nor a stored login session. Cheap-ish (a cookie read); only run when
        // enabled to avoid touching the browser store for off providers.
        let needs_login = enabled && !history_sync::has_any_session(key);
        out.push(ProviderSyncStatus {
            provider: key.to_string(),
            label: label.to_string(),
            enabled,
            last_sync,
            conversation_count,
            session_source: source_str(source),
            needs_login,
        });
    }
    Ok(out)
}

/// Run a sync for `provider` right now. Returns the new/skipped counts via the
/// status (the frontend re-reads `history_sync_status` to refresh).
#[tauri::command]
pub async fn history_sync_now(
    provider: String,
    app: AppHandle,
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    // Full fallback chain: browser/keychain auto-detect, then (if a prior webview
    // sign-in persisted a session) a hidden headless reuse — so "Sync now" works
    // after a Connect without making the user sign in again.
    match history_sync::sync_provider_auto(&provider, &app, &store).await? {
        history_sync::SyncOutcome::Imported { result, source } => Ok(format!(
            "Synced via {:?}: {} new, {} already present",
            source, result.imported, result.skipped
        )),
        history_sync::SyncOutcome::NeedsLogin => {
            Err("Not signed in — use Connect to sign in to this provider.".to_string())
        }
    }
}

/// Login fallback (reliable webview-fetch). Opens a Tauri webview at the
/// provider's **main app** so the user lands signed-in (or signs in), then
/// injects JS that fetches every conversation through the provider's own web API
/// **inside that authenticated, same-origin context** and hands the JSON back to
/// Rust via [`tauri::WebviewWindow::eval_with_callback`] — no fragile cookie
/// extraction, no IPC into a third-party origin (both unreliable / CSP-blocked
/// on Windows). The collected JSON runs straight through the existing
/// [`crate::chat_import`] parse + import pipeline.
///
/// Before opening a window we try the **browser auto-detect fast path** (a plain
/// sync): on non-ABE setups that already yields a session and avoids any popup.
/// Progress is emitted on the `history_sync:progress` event for the UI.
///
/// No cookie/token is ever read by Rust or returned across the bridge.
#[tauri::command]
pub async fn history_sync_connect(
    provider: String,
    app: AppHandle,
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    let web = history_sync::parse_provider(&provider)?;

    // Fast path: if a browser session is auto-detectable (non-ABE setups), a
    // normal sync works without ever opening a window.
    if history_sync::has_any_session(web.key()) {
        if let Ok(history_sync::SyncOutcome::Imported { result, source }) =
            history_sync::sync_provider(web.key(), &store).await
        {
            enable_and_schedule(&app, web, store.inner().clone())?;
            return Ok(format!(
                "Synced via {:?}: {} new, {} already present.",
                source, result.imported, result.skipped
            ));
        }
        // Auto-detect looked available but the sync didn't land — fall through
        // to the reliable webview fetch below.
    }

    // Reliable fallback: fetch inside the authenticated webview.
    let result = history_sync::webview_fetch::fetch_and_import(web, &app, store.inner()).await?;
    enable_and_schedule(&app, web, store.inner().clone())?;
    Ok(format!(
        "Synced {} via sign-in: {} new, {} already present.",
        web.key(),
        result.imported,
        result.skipped
    ))
}

/// Mark a provider enabled and (re)spawn its background sync loop. Shared by the
/// connect paths so a successful Connect also keeps history fresh on a schedule.
fn enable_and_schedule(app: &AppHandle, web: WebProvider, store: TracingStore) -> Result<(), String> {
    let mut cfg = history_sync::config::load();
    cfg.entry(web.key()).enabled = true;
    history_sync::config::save(&cfg)?;
    history_sync::scheduler::spawn_provider_loop(app.clone(), web.key().to_string(), store);
    Ok(())
}
