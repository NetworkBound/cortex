//! Reliable login-fallback: fetch chat history **inside** the authenticated
//! provider webview, instead of trying to extract the (HttpOnly /
//! App-Bound-Encrypted) session cookie out of the system browser.
//!
//! ## Why
//! The old fallback opened a Tauri webview at the provider login and polled
//! `WebviewWindow::cookies_for_url` for the session cookie. On real
//! Windows/WebView2 that is unreliable: the session cookies are HttpOnly (often
//! invisible to the runtime cookie store) and Edge's App-Bound-Encryption locks
//! the *system* browser's cookies. So it "just opened the popup" and never
//! captured anything.
//!
//! ## What this does instead
//! We open a [`WebviewWindow`] at the provider's **main app** (`claude.ai` /
//! `chatgpt.com`) so the user lands logged-in (or signs in). Then we inject a
//! small JS collector that, running in the page's **own authenticated
//! same-origin context**, calls the provider's private web API to enumerate and
//! download every conversation as JSON. That JSON is handed back to Rust and run
//! through the EXISTING [`crate::chat_import`] parse + import pipeline — the same
//! one the file-import and (old) token-pull paths use.
//!
//! ## Data-return mechanism — `WebviewWindow::eval_with_callback`
//! The hard part is getting a multi-MB payload *out* of a third-party webview
//! whose CSP (`connect-src`) forbids `fetch()` to any Tauri/localhost origin and
//! forbids the IPC bridge. We deliberately avoid:
//!   - a custom URI scheme + `fetch()` — blocked by the page's `connect-src`,
//!   - enabling Tauri IPC in the third-party window — also CSP-blocked, and a
//!     security footgun on a remote origin,
//!   - `on_navigation` / `document.title` smuggling — racy and fragile.
//!
//! Instead we use [`WebviewWindow::eval_with_callback`], which the Tauri runtime
//! services through the native webview's own script-evaluation API (WebView2
//! `ExecuteScriptAsync` / WKWebView `evaluateJavaScript`). It returns the
//! JS expression's value, JSON-serialized, to a Rust callback — and it is **not
//! governed by the page CSP at all**, because it is the embedder evaluating the
//! script, not the page making a network request. The collector buffers its
//! results in-page; Rust polls `__cortexHistorySync.drain()` and reassembles the
//! base64 chunks. Chunking keeps each `eval` result small and dodges any
//! per-call string-size limits.
//!
//! ## Secrets
//! No cookie/token is ever read by Rust here — the credential never leaves the
//! page. We only ever see the resulting conversation JSON. Errors carry the HTTP
//! status + which step failed, never any header/credential value.

use std::collections::HashSet;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::chat_import::{self, ImportResult};
use crate::history_sync::cookies::WebProvider;
use crate::observability::tracing_store::TracingStore;

/// Event name the UI listens on for live "fetched N conversations" progress.
pub const PROGRESS_EVENT: &str = "history_sync:progress";

/// Overall wall-clock budget for an interactive login + full fetch. Generous
/// because the user may need to sign in / solve a captcha first.
const OVERALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long we'll wait, after the window is open, for the user to be
/// authenticated before giving up with a clear "needs login" message. Reset
/// implicitly: once the collector reports it's running we stop caring about it.
/// Generous on purpose — a real sign-in can involve an email verification code,
/// 2FA, a captcha, and an off-origin SSO (Google) round-trip; closing the window
/// out from under a still-typing user is far worse than waiting. Bounded above
/// by `OVERALL_TIMEOUT`, which still caps a truly abandoned window.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(9 * 60);

/// Poll cadence for draining the in-page collector.
const POLL_INTERVAL: Duration = Duration::from_millis(750);

/// Headless (background) re-sync gives up on the login phase quickly: there is
/// no user to sign in, so if the persisted session has expired the collector
/// will sit in `login` forever. A short probe lets the page load + the collector
/// run its auth check, then we conclude "session expired → needs re-Connect".
const HEADLESS_LOGIN_PROBE: Duration = Duration::from_secs(60);

/// Overall cap for a HEADLESS background re-sync — far shorter than the
/// interactive budget so a stalled background fetch can never pin "Sync now" or
/// a scheduler tick for the full interactive timeout.
const HEADLESS_OVERALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Providers with a fetch (interactive or headless) currently in flight. A
/// per-provider single-flight guard: the in-page collector's `drain()` is
/// destructive (it splices the pending-chunk queue), so two concurrent fetches
/// on the same provider — e.g. a manual "Sync now" landing during a scheduler
/// tick — would consume each other's chunks and corrupt both payloads. We let
/// the first win and make the second a no-op.
fn fetch_in_flight() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard: marks `key` in-flight on acquire, clears it on drop. `None` if a
/// fetch for this provider is already running.
struct FetchGuard(&'static str);
impl Drop for FetchGuard {
    fn drop(&mut self) {
        if let Ok(mut s) = fetch_in_flight().lock() {
            s.remove(self.0);
        }
    }
}
fn try_begin_fetch(key: &'static str) -> Option<FetchGuard> {
    let mut s = fetch_in_flight().lock().ok()?;
    if !s.insert(key.to_string()) {
        return None; // already in flight
    }
    Some(FetchGuard(key))
}

/// The dedicated, persistent WebView2 profile directory for a provider's
/// history-sync sign-in (`~/.cortex/webview-sessions/<provider>`). Isolated from
/// the main app webview so the session survives app-version wipes, and reused by
/// the background headless re-sync. `None` only if the home dir can't resolve.
pub fn provider_profile_dir(provider: WebProvider) -> Option<std::path::PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".cortex")
            .join("webview-sessions")
            .join(provider.key()),
    )
}

/// True once a provider has been signed in via the interactive webview at least
/// once (its persistent profile directory exists), so a hidden headless re-sync
/// can reuse that stored session without any credential crossing into Rust.
pub fn has_webview_profile(provider: WebProvider) -> bool {
    provider_profile_dir(provider)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Progress payload emitted to the frontend (`history_sync:progress`). Carries
/// no secrets — only counts + a coarse phase string.
#[derive(Clone, serde::Serialize)]
pub struct Progress {
    pub provider: &'static str,
    /// `"login"` | `"running"` | `"importing"` | `"done"` | `"error"`.
    pub phase: String,
    /// Conversations fetched so far (best-effort).
    pub fetched: u64,
    /// Total conversations discovered (0 until the list is known).
    pub total: u64,
    /// Human message (never a credential).
    pub message: String,
}

/// What a single `drain()` poll returns from the in-page collector. All fields
/// are optional/defaulted because the page shape must never hard-fail Rust.
#[derive(Debug, Default, Deserialize)]
struct DrainState {
    /// `"login"` | `"running"` | `"done"` | `"error"`.
    #[serde(default)]
    status: String,
    /// Defensive, secret-free error (HTTP status + step).
    #[serde(default)]
    error: String,
    #[serde(default)]
    fetched: u64,
    #[serde(default)]
    total: u64,
    /// Base64 chunks of the final conversations-JSON, emitted once `status` is
    /// `"done"`. Drained (cleared in-page) each poll, so we just concatenate.
    #[serde(default)]
    chunks: Vec<String>,
    /// True once every chunk of the final payload has been handed over.
    #[serde(default)]
    complete: bool,
}

/// Drive the authenticated-webview fetch for `provider` end-to-end: open the
/// window, inject the collector, poll it to completion, reassemble + parse +
/// import via the existing pipeline. Returns the [`ImportResult`].
///
/// `Err` distinguishes the user-actionable "needs login / cancelled" case from a
/// real failure via the message; the caller maps it for the UI. No credential
/// ever appears in any return value or log line.
pub async fn fetch_and_import(
    provider: WebProvider,
    app: &AppHandle,
    store: &TracingStore,
) -> Result<ImportResult, String> {
    fetch_and_import_inner(provider, app, store, false).await
}

/// Background variant: reuse a previously-persisted provider sign-in in a HIDDEN
/// webview (no user interaction) and import. Returns `Err` if the stored session
/// has expired (the caller maps that to "needs re-Connect"). No credential ever
/// crosses into Rust — same same-origin collector, just an invisible window.
pub async fn headless_fetch_and_import(
    provider: WebProvider,
    app: &AppHandle,
    store: &TracingStore,
) -> Result<ImportResult, String> {
    fetch_and_import_inner(provider, app, store, true).await
}

async fn fetch_and_import_inner(
    provider: WebProvider,
    app: &AppHandle,
    store: &TracingStore,
    headless: bool,
) -> Result<ImportResult, String> {
    // Single-flight per provider: refuse to run a second concurrent fetch (held
    // until this function returns), so a manual Sync and a scheduled headless
    // tick can't consume each other's drained chunks.
    let _guard = try_begin_fetch(provider.key()).ok_or_else(|| {
        format!("a history sync is already in progress for {}", provider.key())
    })?;
    let raw_json = collect_conversations_json(provider, app, headless).await?;
    // A genuinely signed-in account with no conversations assembles to an empty
    // array — that is "nothing to import", not an error (import_from_str would
    // otherwise reject an empty parse).
    if raw_json.trim() == "[]" {
        emit(app, provider, "done", 0, 0, "no conversations found");
        return Ok(ImportResult::default());
    }
    emit(app, provider, "importing", 0, 0, "parsing fetched conversations");

    // Feed the collected JSON straight through the EXISTING importer. The JS
    // assembles an array shaped exactly like the provider's export
    // (`chat_messages` for Claude, `mapping` for ChatGPT), so the existing
    // `parse_claude` / `parse_chatgpt` consume it unchanged — no importer
    // rewrite, just format-pinned dispatch.
    let format = match provider {
        WebProvider::Claude => Some(chat_import::Format::Claude),
        WebProvider::ChatGpt => Some(chat_import::Format::ChatGpt),
    };
    let result = chat_import::import_from_str(&raw_json, format, store).await?;
    emit(
        app,
        provider,
        "done",
        result.imported as u64,
        result.imported as u64,
        &format!("{} new, {} already present", result.imported, result.skipped),
    );
    Ok(result)
}

/// Open the provider webview, inject the collector, and pump it until it returns
/// the assembled conversations JSON (a base64-chunked array reassembled here).
async fn collect_conversations_json(
    provider: WebProvider,
    app: &AppHandle,
    headless: bool,
) -> Result<String, String> {
    // Distinct label so a background headless window never collides with (or gets
    // reused as) an interactive sign-in window — they share the same profile dir.
    let label = if headless {
        format!("history-fetch-headless-{}", provider.key())
    } else {
        format!("history-fetch-{}", provider.key())
    };
    let app_url = match provider {
        WebProvider::Claude => "https://claude.ai/",
        WebProvider::ChatGpt => "https://chatgpt.com/",
    };
    let parsed: tauri::Url = app_url.parse().map_err(|_| "bad provider URL".to_string())?;

    // The init script installs the collector before page scripts run, so it's
    // available the moment the SPA boots and survives in-page navigations.
    let init = init_script(provider);

    // Reuse an existing window if a fetch re-opens mid-flight.
    let window = if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        existing
    } else {
        // Build the WebviewWindow on the MAIN (event-loop) thread. On Windows,
        // window/WebView2 creation must happen there; the headless re-sync runs on
        // a background tokio task, so we marshal the build onto the main thread
        // and hand the window back over a oneshot.
        let title = format!("Sign in to {} & sync history", provider.key());
        let profile = provider_profile_dir(provider);
        let app_for_build = app.clone();
        let label_for_build = label.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<tauri::WebviewWindow, String>>();
        app.run_on_main_thread(move || {
            let mut builder = WebviewWindowBuilder::new(
                &app_for_build,
                label_for_build,
                WebviewUrl::External(parsed),
            )
            .title(title)
            .inner_size(560.0, 760.0)
            // Headless re-sync runs invisibly; interactive sign-in is shown.
            .visible(!headless)
            .initialization_script(&init);
            // Persist the provider sign-in in a DEDICATED WebView2 profile,
            // isolated from the main app webview, so the session survives window
            // close/reopen AND app updates (the main profile is wiped by
            // `clear_all_browsing_data` on version bump — see lib.rs). It is also
            // the same persisted jar the background headless re-sync reuses.
            if let Some(dir) = profile {
                builder = builder.data_directory(dir);
            }
            let _ = tx.send(
                builder
                    .build()
                    .map_err(|e| format!("failed to open provider window: {e}")),
            );
        })
        .map_err(|e| format!("failed to schedule window creation: {e}"))?;
        rx.await
            .map_err(|_| "window creation did not complete".to_string())??
    };

    if !headless {
        emit(app, provider, "login", 0, 0, "waiting for sign-in…");
    }

    let started = Instant::now();
    // Headless reuse can't wait for a human, so it gives up on the login phase
    // fast (expired session); interactive sign-in waits generously. The overall
    // cap is likewise much shorter headless so a stalled background fetch can't
    // pin "Sync now" or a scheduler tick for the full interactive timeout.
    let login_budget = if headless { HEADLESS_LOGIN_PROBE } else { LOGIN_TIMEOUT };
    let overall_budget = if headless { HEADLESS_OVERALL_TIMEOUT } else { OVERALL_TIMEOUT };
    let mut login_seen = false;
    let mut assembled = String::new();
    let mut last_emitted_fetched = u64::MAX;

    // Kick the collector off (idempotent in-page). If the window is still
    // loading, the init-script global may not exist yet — ignore eval errors and
    // retry on the next tick.
    let _ = window.eval("window.__cortexHistorySync && window.__cortexHistorySync.start();");

    loop {
        if started.elapsed() > overall_budget {
            let _ = window.close();
            return Err("history sync timed out before completing".to_string());
        }
        // Window vanished (user closed an interactive one, or a headless one was
        // torn down).
        if app.get_webview_window(&label).is_none() {
            return Err(if headless {
                "headless history sync window closed".to_string()
            } else {
                "Sign-in window was closed before syncing completed.".to_string()
            });
        }

        // (Re)start is cheap + idempotent; covers the "global not ready on first
        // tick" race without a separate readiness handshake.
        let _ = window.eval("window.__cortexHistorySync && window.__cortexHistorySync.start();");

        let drained = drain_once(&window).await;
        let Some(state) = drained else {
            // Collector global not present yet (page still loading). Keep waiting.
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        };

        match state.status.as_str() {
            "login" => {
                if started.elapsed() > login_budget && !login_seen {
                    let _ = window.close();
                    return Err(if headless {
                        "stored session expired — re-Connect to refresh sign-in".to_string()
                    } else {
                        "Not signed in. Open the window, sign in, then try Connect again."
                            .to_string()
                    });
                }
            }
            "running" => {
                login_seen = true;
                if state.fetched != last_emitted_fetched {
                    last_emitted_fetched = state.fetched;
                    emit(
                        app,
                        provider,
                        "running",
                        state.fetched,
                        state.total,
                        &format!("fetched {} of {} conversations", state.fetched, state.total),
                    );
                }
                for chunk in &state.chunks {
                    push_chunk(&mut assembled, chunk)?;
                }
            }
            "done" => {
                login_seen = true;
                for chunk in &state.chunks {
                    push_chunk(&mut assembled, chunk)?;
                }
                if state.complete {
                    let _ = window.close();
                    if assembled.trim().is_empty() {
                        return Err(
                            "authenticated but no conversation data was returned".to_string(),
                        );
                    }
                    return Ok(assembled);
                }
            }
            "error" => {
                let _ = window.close();
                // The in-page error is already secret-free (status + step).
                let msg = if state.error.trim().is_empty() {
                    "provider fetch failed (unknown step)".to_string()
                } else {
                    state.error.clone()
                };
                return Err(format!("{}: {msg}", provider.key()));
            }
            // Empty/unknown status: collector still warming up.
            _ => {}
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Append a base64 chunk's decoded UTF-8 to the assembled buffer.
fn push_chunk(buf: &mut String, b64: &str) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|_| "history sync: corrupt chunk (base64)".to_string())?;
    let s = String::from_utf8(bytes)
        .map_err(|_| "history sync: corrupt chunk (utf8)".to_string())?;
    buf.push_str(&s);
    Ok(())
}

/// One `drain()` poll: evaluate the collector's drain in-page and parse the
/// JSON it returns. `eval_with_callback` hands us the value as a JSON *string*
/// (so the collector's object arrives double-encoded — a JSON string containing
/// JSON); we strip the outer layer then parse. Returns `None` if the global
/// isn't present yet or the eval produced nothing parseable.
async fn drain_once(window: &tauri::WebviewWindow) -> Option<DrainState> {
    let (tx, rx) = mpsc::channel::<String>();
    // The collector returns its state already JSON.stringify'd, so the page
    // expression yields a String; eval_with_callback then JSON-encodes *that*
    // string. We unwrap both layers below.
    let js = "(window.__cortexHistorySync ? window.__cortexHistorySync.drain() : \"\")";
    if window
        .eval_with_callback(js, move |res| {
            let _ = tx.send(res);
        })
        .is_err()
    {
        return None;
    }

    // The callback fires asynchronously off the webview thread; wait briefly.
    let raw = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)))
        .await
        .ok()?
        .ok()?;

    // `raw` is the JSON encoding of the page expression's value (a JSON string).
    // First decode the outer JSON to get the inner string the collector built.
    let inner: String = serde_json::from_str(&raw).unwrap_or(raw);
    if inner.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<DrainState>(&inner).ok()
}

/// Emit a secret-free progress event to the frontend.
fn emit(app: &AppHandle, provider: WebProvider, phase: &str, fetched: u64, total: u64, message: &str) {
    let _ = app.emit(
        PROGRESS_EVENT,
        Progress {
            provider: provider.key(),
            phase: phase.to_string(),
            fetched,
            total,
            message: message.to_string(),
        },
    );
}

/// The in-page collector, injected as an `initialization_script`. It installs
/// `window.__cortexHistorySync` with `start()` (idempotent kick-off) and
/// `drain()` (returns a JSON string of `{status, phase, error, fetched, total,
/// chunks, complete}` and clears the chunk buffer). All network calls are
/// same-origin in the authenticated page, so cookies + CSP are satisfied.
fn init_script(provider: WebProvider) -> String {
    let body = match provider {
        WebProvider::Claude => CLAUDE_COLLECTOR,
        WebProvider::ChatGpt => CHATGPT_COLLECTOR,
    };
    format!("{COLLECTOR_PRELUDE}\n{body}\n{COLLECTOR_EPILOGUE}")
}

/// Shared collector scaffolding: the state object, chunking, and the `drain()`
/// contract. The provider-specific body defines `__collect()` (an async fn that
/// drives fetching and calls `__emit(convArray)` once, plus `__progress(n,t)`).
const COLLECTOR_PRELUDE: &str = r#"
(function () {
  if (window.__cortexHistorySync) { return; }
  var S = {
    started: false,
    status: "login",      // login | running | done | error
    phase: "login",
    error: "",
    fetched: 0,
    total: 0,
    pending: [],          // base64 chunks not yet drained
    complete: false,      // all chunks emitted
  };

  function b64(str) {
    // UTF-8 safe base64 (btoa is latin1-only).
    var bytes = new TextEncoder().encode(str);
    var bin = "";
    for (var i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    return btoa(bin);
  }

  // Split the final JSON into ~256KB UTF-16 slices, base64 each, queue them.
  function emitPayload(convArray) {
    var json = JSON.stringify(convArray);
    var CHUNK = 256 * 1024;
    for (var i = 0; i < json.length; i += CHUNK) {
      S.pending.push(b64(json.slice(i, i + CHUNK)));
    }
    S.status = "done";
    S.phase = "done";
  }

  function progress(n, t) {
    S.fetched = n;
    if (typeof t === "number") S.total = t;
    if (S.status === "login") { S.status = "running"; S.phase = "running"; }
  }

  function fail(msg) {
    S.status = "error";
    S.phase = "error";
    S.error = String(msg || "unknown error");
  }

  // Expose helpers to the provider body.
  window.__hsEmit = emitPayload;
  window.__hsProgress = progress;
  window.__hsFail = fail;
  window.__hsState = S;
"#;

/// Closes the IIFE, wiring `start()` / `drain()` to the shared state. The
/// provider body must define `window.__hsCollect` (an async function).
const COLLECTOR_EPILOGUE: &str = r#"
  window.__cortexHistorySync = {
    start: function () {
      if (S.started) return;
      S.started = true;
      // Drive the provider-specific async collection. Any throw → error state.
      Promise.resolve()
        .then(function () { return window.__hsCollect(); })
        .then(function () {
          // If the collector returned still on "login" (user not signed in
          // yet), unlatch so the next tick retries — this is what makes the
          // sign-in-then-sync flow work regardless of whether the login
          // transition is a full navigation or an in-page SPA route change.
          if (S.status === "login") S.started = false;
        })
        .catch(function (e) {
          // A "needs login" signal from ANY step (not just bootstrap) means the
          // user simply isn't signed in yet — stay in the login state and
          // unlatch so the next tick retries once they do. This is what makes
          // signing in AFTER the window opens work; otherwise a logged-out
          // 401/403 on /organizations or /chat_conversations would latch an
          // error and Rust would close the window before the user could log in.
          if (e && (e.__needsLogin || e.message === "__needs_login__")) {
            S.status = "login";
            S.phase = "login";
            S.started = false;
            return;
          }
          // Keep the message secret-free: status code + step only (the body
          // builds these); fall back to the error name.
          fail((e && e.message) ? e.message : "fetch failed");
        });
    },
    drain: function () {
      // Hand over a bounded slice of queued chunks (keeps each eval result
      // small) and mark complete only once we're done AND every chunk has been
      // drained. Returning all of S.pending at once would defeat the chunking
      // and risk the eval string-size limit on large histories.
      var chunks = S.pending.splice(0, 4);
      if (S.status === "done" && S.pending.length === 0) {
        S.complete = true;
      }
      return JSON.stringify({
        status: S.status,
        phase: S.phase,
        error: S.error,
        fetched: S.fetched,
        total: S.total,
        chunks: chunks,
        complete: S.complete,
      });
    },
  };
})();
"#;

/// claude.ai collector body. Discovers the org uuid, lists conversations, then
/// downloads each full tree. Builds an array of objects shaped like the Claude
/// export (`{ uuid, name, created_at, chat_messages: [...] }`) so the existing
/// `parse_claude` consumes it unchanged.
const CLAUDE_COLLECTOR: &str = r#"
  window.__hsCollect = async function () {
    // Only operate inside the authenticated provider origin. During sign-in the
    // webview may navigate to an SSO/OAuth origin (e.g. accounts.google.com) or
    // a transient claude.ai page where our relative API calls would 404 / return
    // HTML; treat anything off-origin as "still signing in" so the window stays
    // open until the user lands back, authenticated, on claude.ai.
    if (!/(^|\.)claude\.ai$/i.test(location.hostname)) return;

    async function getJSON(url, step) {
      var r;
      try { r = await fetch(url, { credentials: "include", headers: { "accept": "application/json" } }); }
      catch (e) { throw new Error("claude " + step + ": network error"); }
      if (r.status === 401 || r.status === 403) {
        // Not authed yet — stay in login state and let Rust keep waiting.
        var err = new Error("__needs_login__");
        err.__needsLogin = true;
        throw err;
      }
      if (!r.ok) throw new Error("claude " + step + ": HTTP " + r.status);
      try { return await r.json(); }
      catch (e) { throw new Error("claude " + step + ": bad JSON"); }
    }

    // Phase 1 — resolve the org uuid. This doubles as the auth probe: ANY
    // failure here (401/403, HTML/redirect on a mid-login page, a network blip,
    // or shape drift) means we are not yet usefully authenticated. Stay in the
    // login state and let Rust keep polling — NEVER fatal here, so the window
    // stays open while the user finishes signing in.
    var org = null;
    try {
      try {
        var boot = await getJSON("/api/bootstrap", "bootstrap");
        org = (boot && boot.account && boot.account.memberships &&
               boot.account.memberships[0] &&
               boot.account.memberships[0].organization &&
               boot.account.memberships[0].organization.uuid) || null;
      } catch (e) { /* fall through to /api/organizations */ }
      if (!org) {
        var orgs = await getJSON("/api/organizations", "organizations");
        if (Array.isArray(orgs) && orgs.length) {
          // Prefer a chat-capable org if flagged; else first.
          var chosen = orgs.find(function (o) {
            return o && o.capabilities && o.capabilities.indexOf &&
                   o.capabilities.indexOf("chat") !== -1;
          }) || orgs[0];
          org = chosen && chosen.uuid;
        }
      }
    } catch (e) {
      return; // not signed in yet / transient — keep window open and retry
    }
    if (!org) return; // keep waiting for sign-in

    // 2) List conversation summaries. A transient failure here (a 5xx, a brief
    //    Cloudflare/HTML interstitial right after the sign-in redirect settles,
    //    or a 401 if the session lapses) must NOT be fatal — it would close the
    //    window mid-flow. Stay in the login state and let Rust retry next tick.
    var list;
    try {
      list = await getJSON(
        "/api/organizations/" + org + "/chat_conversations", "list");
    } catch (e) {
      return; // transient / needs-login — keep window open and retry
    }
    var ids = (Array.isArray(list) ? list : [])
      .map(function (c) { return c && c.uuid; })
      .filter(Boolean);
    __hsProgress(0, ids.length);

    // 3) Fetch each full conversation tree. A single failure is skipped, not fatal.
    var out = [];
    for (var i = 0; i < ids.length; i++) {
      try {
        var detail = await getJSON(
          "/api/organizations/" + org + "/chat_conversations/" + ids[i] +
          "?tree=True&rendering_mode=raw", "conversation");
        if (detail) out.push(detail);
      } catch (e) { /* skip this conversation */ }
      __hsProgress(i + 1, ids.length);
    }

    // Authenticated with zero conversations is a valid "no history" outcome, not
    // an error — emit the (possibly empty) array and let Rust finish cleanly.
    __hsEmit(out);
  };
"#;

/// chatgpt.com collector body. Gets an accessToken from /api/auth/session, lists
/// conversations (paginated), downloads each, and builds an array of objects
/// shaped like the ChatGPT export (each with a top-level `mapping`) so the
/// existing `parse_chatgpt` consumes it unchanged.
const CHATGPT_COLLECTOR: &str = r#"
  window.__hsCollect = async function () {
    // Only operate inside the authenticated provider origin (see Claude note):
    // during sign-in the webview may navigate to an SSO/OAuth origin where our
    // relative API calls would fail; treat off-origin as "still signing in".
    if (!/(^|\.)chatgpt\.com$|(^|\.)chat\.openai\.com$/i.test(location.hostname)) return;
    // 1) accessToken via the same endpoint the web app uses.
    var token = null;
    try {
      var sresp = await fetch("/api/auth/session", { credentials: "include" });
      if (sresp.status === 401 || sresp.status === 403) return; // not authed yet
      if (sresp.ok) {
        var sj = await sresp.json();
        token = sj && sj.accessToken;
      }
    } catch (e) { /* fall through; no token → treat as login */ }
    if (!token) return; // stay in login state; Rust keeps polling

    function authHeaders() {
      return { "authorization": "Bearer " + token, "accept": "application/json" };
    }
    async function getJSON(url, step) {
      var r;
      try { r = await fetch(url, { credentials: "include", headers: authHeaders() }); }
      catch (e) { throw new Error("chatgpt " + step + ": network error"); }
      if (!r.ok) throw new Error("chatgpt " + step + ": HTTP " + r.status);
      try { return await r.json(); }
      catch (e) { throw new Error("chatgpt " + step + ": bad JSON"); }
    }

    // 2) Page the conversation list.
    var ids = [];
    var offset = 0, limit = 100, guard = 0;
    while (guard++ < 100) {
      var page = await getJSON(
        "/backend-api/conversations?offset=" + offset + "&limit=" + limit +
        "&order=updated", "list");
      var items = (page && page.items) || [];
      for (var k = 0; k < items.length; k++) {
        if (items[k] && items[k].id) ids.push(items[k].id);
      }
      if (items.length < limit) break;     // last page
      offset += limit;
    }
    __hsProgress(0, ids.length);

    // 3) Download each conversation's mapping. Skip failures.
    var out = [];
    for (var i = 0; i < ids.length; i++) {
      try {
        var detail = await getJSON("/backend-api/conversation/" + ids[i], "conversation");
        if (detail) out.push(detail);
      } catch (e) { /* skip */ }
      __hsProgress(i + 1, ids.length);
    }

    // Authenticated with zero conversations is a valid "no history" outcome, not
    // an error — emit the (possibly empty) array and let Rust finish cleanly.
    __hsEmit(out);
  };
"#;
