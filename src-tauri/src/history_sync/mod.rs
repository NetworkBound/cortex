//! Automatic chat-history sync from providers' web apps into Cortex.
//!
//! Chat history lives in a provider's **web** app behind a **web** session,
//! which the API/CLI login does NOT include. This module captures that web
//! session — **browser auto-detect first** ([`cookies`]), with a **one-time
//! in-app login fallback** (a Tauri webview, see `commands::history_sync`) — and
//! then reuses the existing [`crate::chat_import`] pull + import pipeline to
//! write resumable, searchable sessions into the [`TracingStore`].
//!
//! ## Pieces
//! - [`config`]    — persisted per-provider toggle/last-sync/source.
//! - [`cookies`]   — browser cookie extraction (Windows-priority Chromium DPAPI
//!                   + AES-GCM; Firefox/best-effort elsewhere).
//! - [`scheduler`] — tokio background loop that re-syncs enabled providers.
//!
//! ## Dedup / incremental
//! Sync is incremental for free: the import pipeline
//! ([`crate::chat_import::pipeline`]) derives a **deterministic** session id from
//! each conversation's content and **skips conversations whose session already
//! has messages**. So re-pulling the full conversation list every cycle never
//! duplicates already-imported chats — only new/changed conversations get
//! written. We surface that as `imported` (new) vs `skipped` (already present).
//!
//! ## Secrets
//! Session cookies / tokens are **never logged**. The login-fallback session is
//! stored in the OS keychain; the config file holds only flags + a timestamp.

pub mod config;
pub mod cookies;
pub mod scheduler;

use crate::chat_import::{self, ImportResult};
use crate::observability::tracing_store::TracingStore;
use config::{SessionSource, HistorySyncConfig};
use cookies::WebProvider;

/// OS-keychain service shared with the rest of Cortex.
const KEYRING_SERVICE: &str = "dev.connor.cortex";

/// Default re-sync cadence for the background scheduler.
pub const DEFAULT_SYNC_INTERVAL_HOURS: u64 = 6;

/// Outcome of a single provider sync attempt.
#[derive(Debug, Clone)]
pub enum SyncOutcome {
    /// Imported (with the pipeline result; `imported`/`skipped` reflect dedup).
    Imported {
        result: ImportResult,
        source: SessionSource,
    },
    /// No web session could be auto-detected and none is stored — the UI should
    /// trigger the login fallback (`history_sync_connect`).
    NeedsLogin,
}

/// Parse a `"claude"|"chatgpt"` provider key into a [`WebProvider`].
pub fn parse_provider(provider: &str) -> Result<WebProvider, String> {
    match provider.to_ascii_lowercase().as_str() {
        "claude" => Ok(WebProvider::Claude),
        "chatgpt" => Ok(WebProvider::ChatGpt),
        other => Err(format!("unknown provider: {other} (expected claude|chatgpt)")),
    }
}

/// Keychain key under which a login-fallback web session is stashed.
fn keychain_user(provider: WebProvider) -> String {
    format!("history_sync_session_{}", provider.key())
}

/// Store a web-session credential captured via the login fallback. Never logged.
pub fn store_login_session(provider: WebProvider, value: &str) -> Result<(), String> {
    keyring::Entry::new(KEYRING_SERVICE, &keychain_user(provider))
        .and_then(|e| e.set_password(value))
        .map_err(|e| format!("keychain store failed: {e}"))?;
    // Record that this provider now has a login-sourced session.
    let mut cfg = config::load();
    cfg.entry(provider.key()).session_source = Some(SessionSource::Login);
    config::save(&cfg)
}

/// Fetch a previously stored login-fallback session, if any.
fn load_login_session(provider: WebProvider) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, &keychain_user(provider))
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.trim().is_empty())
}

/// Current epoch-millis.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Obtain a web session for `provider`: browser auto-detect first, then a stored
/// login-fallback session. Returns `(credential, source)` or `None` if neither
/// is available (→ caller returns [`SyncOutcome::NeedsLogin`]).
fn obtain_session(provider: WebProvider) -> Option<(String, SessionSource)> {
    match cookies::detect_session_cookie(provider) {
        Ok(v) if !v.trim().is_empty() => return Some((v, SessionSource::Browser)),
        Ok(_) => {}
        Err(e) => {
            // No cookie value in the message — only structural info.
            tracing::info!("history_sync: browser auto-detect failed for {}: {e}", provider.key());
        }
    }
    load_login_session(provider).map(|v| (v, SessionSource::Login))
}

/// For ChatGPT, the web session cookie must be exchanged for an `accessToken`
/// (the bearer JWT `pull_chatgpt` expects) via `/api/auth/session`. For Claude,
/// the `sessionKey` cookie is used directly by `pull_claude`.
///
/// Returns the credential to hand to the matching puller. Never logs the cookie
/// or the resulting token.
async fn credential_for_pull(
    provider: WebProvider,
    session_cookie: &str,
) -> Result<String, String> {
    match provider {
        WebProvider::Claude => Ok(session_cookie.to_string()),
        WebProvider::ChatGpt => exchange_chatgpt_access_token(session_cookie).await,
    }
}

/// Browser-like UA — the ChatGPT auth endpoint rejects obviously-automated
/// clients. Cosmetic, not auth.
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Call `https://chatgpt.com/api/auth/session` with the session cookie to get
/// the `accessToken`, mirroring what the manual flow does. Never logs values.
async fn exchange_chatgpt_access_token(session_cookie: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "failed to build HTTP client".to_string())?;

    let cookie = format!("__Secure-next-auth.session-token={session_cookie}");
    let resp: serde_json::Value = client
        .get("https://chatgpt.com/api/auth/session")
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|_| "chatgpt: /api/auth/session request failed (network or blocked)".to_string())?
        .error_for_status()
        .map_err(|e| {
            format!(
                "chatgpt: /api/auth/session returned {}",
                e.status().map(|s| s.as_u16().to_string()).unwrap_or_else(|| "error".into())
            )
        })?
        .json()
        .await
        .map_err(|_| "chatgpt: could not parse auth/session response".to_string())?;

    resp.get("accessToken")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "chatgpt: no accessToken in session response (expired session?)".to_string())
}

/// Sync one provider: obtain a web session, exchange it as needed, pull + import
/// (incremental via the pipeline's dedup), and update `last_sync_ts`.
///
/// On no-session-available, returns [`SyncOutcome::NeedsLogin`] (and still
/// records the attempt timestamp). Errors are returned as strings; the session
/// credential never appears in any of them.
pub async fn sync_provider(
    provider: &str,
    store: &TracingStore,
) -> Result<SyncOutcome, String> {
    let web = parse_provider(provider)?;

    let Some((session, source)) = obtain_session(web) else {
        // Stamp the attempt so the UI shows we tried.
        stamp_last_sync(web, None);
        return Ok(SyncOutcome::NeedsLogin);
    };

    let credential = credential_for_pull(web, &session).await?;
    // Reuse the existing pull + import pipeline. `provider` here is the puller
    // key ("claude"/"chatgpt") which import_from_pull understands.
    let result = chat_import::import_from_pull(web.key(), &credential, store).await?;

    stamp_last_sync(web, Some(source));
    Ok(SyncOutcome::Imported { result, source })
}

/// Update `last_sync_ts` (always) and `session_source` (when known) in config.
fn stamp_last_sync(provider: WebProvider, source: Option<SessionSource>) {
    let mut cfg = config::load();
    let c = cfg.entry(provider.key());
    c.last_sync_ts = Some(now_ms());
    if let Some(s) = source {
        c.session_source = Some(s);
    }
    if let Err(e) = config::save(&cfg) {
        tracing::warn!("history_sync: failed to persist config: {e}");
    }
}

/// Number of imported sessions currently in the store for a provider, by the
/// pipeline's `agent_id = "import:<source>"` tag. Used for the UI count.
///
/// `source` is the provenance label the pipeline writes: `"claude.ai"` for
/// Claude, `"chatgpt"` for ChatGPT (see `chat_import::parse`).
pub fn imported_conversation_count(store: &TracingStore, provider: &str) -> i64 {
    let source = match provider.to_ascii_lowercase().as_str() {
        "claude" => "claude.ai",
        "chatgpt" => "chatgpt",
        _ => return 0,
    };
    store.count_imported_sessions(&format!("import:{source}")).unwrap_or(0)
}

/// True if a working web session is obtainable for the provider key — either
/// auto-detected from a browser or stored from a prior login fallback. Used by
/// the status command to decide whether to surface the "Connect" button.
pub fn has_any_session(provider: &str) -> bool {
    match parse_provider(provider) {
        Ok(web) => obtain_session(web).is_some(),
        Err(_) => false,
    }
}

/// Snapshot a provider's config into the status fields the UI needs.
pub fn status_for(cfg: &HistorySyncConfig, provider: &str) -> (bool, Option<i64>, Option<SessionSource>) {
    match cfg.get(provider) {
        Some(c) => (c.enabled, c.last_sync_ts, c.session_source),
        None => (false, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_maps() {
        assert_eq!(parse_provider("claude").unwrap(), WebProvider::Claude);
        assert_eq!(parse_provider("ChatGPT").unwrap(), WebProvider::ChatGpt);
        assert!(parse_provider("bogus").is_err());
    }

    #[test]
    fn imported_count_unknown_provider_is_zero() {
        let store = TracingStore::in_memory();
        assert_eq!(imported_conversation_count(&store, "bogus"), 0);
    }
}
