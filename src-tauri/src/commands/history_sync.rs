//! Tauri commands for automatic chat-history sync.
//!
//! Surface for the Settings "History Sync" section. Thin wrappers over
//! [`crate::history_sync`]; the session cookie/token never crosses the bridge in
//! a response. The login-fallback (`history_sync_connect`) opens a Tauri webview
//! at the provider's web app and reads the session cookie from that webview's
//! cookie store after the user signs in.

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

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
    store: State<'_, TracingStore>,
) -> Result<(), String> {
    // Validate the provider key.
    let _ = history_sync::parse_provider(&provider)?;

    let mut cfg = history_sync::config::load();
    cfg.entry(&provider).enabled = enabled;
    history_sync::config::save(&cfg)?;

    if enabled {
        // Kick off an immediate sync + the recurring background loop.
        history_sync::scheduler::spawn_provider_loop(provider, store.inner().clone());
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
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    match history_sync::sync_provider(&provider, &store).await? {
        history_sync::SyncOutcome::Imported { result, source } => Ok(format!(
            "Synced via {:?}: {} new, {} already present",
            source, result.imported, result.skipped
        )),
        history_sync::SyncOutcome::NeedsLogin => {
            Err("Not signed in — use Connect to sign in to this provider.".to_string())
        }
    }
}

/// Login fallback: open a webview at the provider's web app. After the user
/// signs in, we read the session cookie from that webview's cookie store and
/// persist it (session_source = "login"), then close the window and run a sync.
///
/// Tauri v2 exposes `WebviewWindow::cookies_for_url`, which returns the runtime
/// cookie store (incl. HttpOnly cookies) for an http(s) URL — exactly the
/// session cookie we need. We poll it for a bounded time because login is async
/// (the cookie appears only after the user authenticates).
#[tauri::command]
pub async fn history_sync_connect(
    provider: String,
    app: AppHandle,
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    let web = history_sync::parse_provider(&provider)?;
    let (login_url, cookie_url, cookie_name) = login_target(web);

    let label = format!("history-login-{}", web.key());
    // Reuse an existing window if the user re-clicks Connect.
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
    } else {
        let parsed: tauri::Url = login_url.parse().map_err(|_| "bad login URL".to_string())?;
        WebviewWindowBuilder::new(&app, label.clone(), WebviewUrl::External(parsed))
            .title(format!("Sign in to {}", web.key()))
        .inner_size(480.0, 720.0)
        .build()
        .map_err(|e| format!("failed to open login window: {e}"))?;
    }

    // Poll the webview cookie store for the session cookie. Up to ~3 minutes,
    // which is plenty for an interactive login without hanging forever.
    let url: tauri::Url = cookie_url.parse().map_err(|_| "bad cookie URL".to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        if std::time::Instant::now() > deadline {
            return Err("Timed out waiting for sign-in. Try again.".to_string());
        }
        // The window may have been closed by the user.
        let Some(win) = app.get_webview_window(&label) else {
            return Err("Sign-in window was closed before completing.".to_string());
        };
        // cookies_for_url must run off the main thread on Windows (WebView2
        // deadlock); Tauri commands already run on the async runtime, so this
        // is fine. Errors here are transient — keep polling.
        if let Ok(cookies) = win.cookies_for_url(url.clone()) {
            if let Some(value) = cookies
                .iter()
                .find(|c| c.name() == cookie_name)
                .map(|c| c.value().to_string())
                .filter(|v| !v.trim().is_empty())
            {
                // Persist (never logged) and close the login window.
                history_sync::store_login_session(web, &value)?;
                let _ = win.close();
                // Mark enabled so the scheduler keeps it fresh, and sync now.
                let mut cfg = history_sync::config::load();
                cfg.entry(web.key()).enabled = true;
                history_sync::config::save(&cfg)?;
                history_sync::scheduler::spawn_provider_loop(web.key().to_string(), store.inner().clone());
                return Ok(format!("Signed in to {} — syncing now.", web.key()));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
}

/// `(login_page_url, cookie_url, cookie_name)` for the login fallback.
fn login_target(provider: WebProvider) -> (&'static str, &'static str, &'static str) {
    match provider {
        WebProvider::Claude => (
            "https://claude.ai/login",
            "https://claude.ai",
            "sessionKey",
        ),
        WebProvider::ChatGpt => (
            "https://chatgpt.com/auth/login",
            "https://chatgpt.com",
            "__Secure-next-auth.session-token",
        ),
    }
}
