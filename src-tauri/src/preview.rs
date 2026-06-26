//! Localhost dev-server preview port sniffer (Terax-AI #7 port).
//!
//! Polls a fixed list of common dev-server ports on 127.0.0.1 every
//! `POLL_INTERVAL` seconds. For each port that accepts a TCP connection we
//! issue a short-timeout HEAD request via `reqwest`. Responses with 2xx/3xx
//! statuses are surfaced as `DetectedServer` records (with the HTML `<title>`
//! parsed when available) and the full live list is emitted to the frontend
//! as a `preview:servers` window event.
//!
//! NOTE: `reqwest::blocking` is not enabled in this workspace (the rest of
//! the app uses the async client), so the "blocking HEAD" requirement from
//! the spec is implemented with the async client + per-request timeout
//! inside a Tokio task. Behaviour from the frontend's perspective is
//! identical.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{timeout, Instant};

/// Ports the sniffer probes on each tick. Covers Vite, CRA, Next, common
/// Python/Go HTTP servers, and a few extras developers reach for.
const PROBE_PORTS: &[u16] = &[
    3000, 3001, 4000, 5000, 5173, 5174, 8000, 8080, 8088, 8888, 9000,
];

/// How often the background loop sweeps the port list.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Per-request TCP+HTTP timeout. Kept tight so a slow port can never stall
/// the whole sweep.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Public surface for one detected dev server.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DetectedServer {
    pub port: u16,
    pub url: String,
    pub title: Option<String>,
}

/// Process-wide handle for the running watcher task. `None` when stopped.
struct WatcherHandle {
    stop_tx: oneshot::Sender<()>,
}

static WATCHER: OnceCell<Arc<Mutex<Option<WatcherHandle>>>> = OnceCell::new();

fn slot() -> Arc<Mutex<Option<WatcherHandle>>> {
    WATCHER
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// One-shot sweep of every probe port. Public so the
/// `list_dev_servers` command can call it synchronously from the frontend
/// without touching the background watcher.
pub async fn sweep_once() -> Vec<DetectedServer> {
    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("preview: failed to build reqwest client: {e:#}");
            return Vec::new();
        }
    };

    let started = Instant::now();
    let mut handles = Vec::with_capacity(PROBE_PORTS.len());
    for &port in PROBE_PORTS {
        let client = client.clone();
        handles.push(tauri::async_runtime::spawn(async move { probe_port(&client, port).await }));
    }

    let mut found = Vec::new();
    for h in handles {
        if let Ok(Some(server)) = h.await {
            found.push(server);
        }
    }
    found.sort_by_key(|s| s.port);
    tracing::debug!(
        "preview: sweep finished in {}ms, {} server(s)",
        started.elapsed().as_millis(),
        found.len()
    );
    found
}

/// Start the background watcher task. Idempotent — if a watcher is already
/// running it is replaced. Errors are logged and surfaced as `Err`.
pub fn start(app: AppHandle) -> Result<()> {
    // Replace any existing watcher first so we never leak two loops.
    stop();

    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    {
        let cell = slot();
        let mut g = cell.lock();
        *g = Some(WatcherHandle { stop_tx });
    }

    // Use Tauri's async runtime rather than `tokio::spawn` — the Tauri
    // `setup` hook runs on the main thread *outside* of a Tokio reactor,
    // so a bare `tokio::spawn` panics with "no reactor running". Tauri's
    // async-runtime wrapper picks the right runtime regardless of where
    // we're called from.
    tauri::async_runtime::spawn(async move {
        let mut last: Option<Vec<DetectedServer>> = None;
        loop {
            // Either tick the sleep OR break early on stop.
            tokio::select! {
                _ = &mut stop_rx => {
                    tracing::info!("preview: watcher stopped");
                    break;
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }

            let servers = sweep_once().await;
            let changed = last.as_ref().map_or(true, |prev| prev != &servers);
            if changed {
                if let Err(e) = app.emit("preview:servers", &servers) {
                    tracing::warn!("preview: emit failed: {e}");
                }
                last = Some(servers);
            }
        }
    });

    Ok(())
}

/// Stop the watcher if running. Returns `true` if we actually stopped one.
pub fn stop() -> bool {
    let cell = slot();
    let mut g = cell.lock();
    if let Some(handle) = g.take() {
        let _ = handle.stop_tx.send(());
        true
    } else {
        false
    }
}

/// Whether the watcher loop is currently active.
pub fn is_running() -> bool {
    let cell = slot();
    let guard = cell.lock();
    guard.is_some()
}

// ───────────────────────────── internals ────────────────────────────────

fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(PROBE_TIMEOUT)
        .user_agent("Cortex-PreviewSniffer/0.1")
        // Don't follow redirects — we already accept any 3xx as a hit and
        // following could blow our timeout budget.
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

async fn probe_port(client: &reqwest::Client, port: u16) -> Option<DetectedServer> {
    // 1. Fast TCP reachability check. Skips dead ports without doing HTTP.
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    match timeout(PROBE_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(_stream)) => { /* port is open, fall through to HTTP probe */ }
        _ => return None,
    }

    let url = format!("http://127.0.0.1:{port}/");

    // 2. HEAD first — cheapest. Some dev servers reject HEAD (looking at
    //    you, Vite + custom middleware) so we fall back to a GET on 4xx/5xx.
    let mut status = None;
    if let Ok(Ok(resp)) = timeout(PROBE_TIMEOUT, client.head(&url).send()).await {
        status = Some(resp.status());
    }

    let needs_get = match status {
        Some(s) => !(s.is_success() || s.is_redirection()),
        None => true,
    };

    let title = if needs_get {
        let Ok(Ok(resp)) = timeout(PROBE_TIMEOUT, client.get(&url).send()).await else {
            // If GET also failed, we still consider the port alive (TCP
            // connected) but with no metadata. The user can still preview.
            return Some(DetectedServer { port, url, title: None });
        };
        if !(resp.status().is_success() || resp.status().is_redirection()) {
            // Something is listening but actively rejecting — skip.
            return None;
        }
        let body = timeout(PROBE_TIMEOUT, resp.text()).await.ok()?.ok()?;
        extract_title(&body)
    } else {
        // HEAD succeeded — quick GET for the body so we can grab <title>.
        if let Ok(Ok(resp)) = timeout(PROBE_TIMEOUT, client.get(&url).send()).await {
            if let Ok(Ok(body)) = timeout(PROBE_TIMEOUT, resp.text()).await {
                extract_title(&body)
            } else {
                None
            }
        } else {
            None
        }
    };

    Some(DetectedServer { port, url, title })
}

/// Largest index `<= index` that lands on a UTF-8 char boundary.
/// Mirrors the unstable `str::floor_char_boundary` so slicing is panic-safe.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else {
        let mut i = index;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}

/// Extract the contents of the first `<title>…</title>` element.
/// Case-insensitive, tolerates whitespace, caps at 200 chars so a hostile
/// page can't stuff the dropdown with megabytes.
fn extract_title(body: &str) -> Option<String> {
    // Look only at the first ~16KB — every real <title> sits in <head>.
    // Clamp to a UTF-8 char boundary so slicing can't panic mid-character.
    let cap = floor_char_boundary(body, body.len().min(16 * 1024));
    let head_window = &body[..cap];
    let re = title_regex();
    let caps = re.captures(head_window)?;
    let raw = caps.get(1)?.as_str().trim();
    if raw.is_empty() {
        return None;
    }
    let decoded = decode_basic_entities(raw);
    let trimmed: String = decoded.chars().take(200).collect();
    Some(trimmed)
}

fn title_regex() -> &'static Regex {
    static RE: OnceCell<Regex> = OnceCell::new();
    RE.get_or_init(|| {
        Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("preview: title regex compiles")
    })
}

/// Decode the handful of HTML entities most likely to appear in a page
/// title. A full HTML parser is overkill for a sniffer.
fn decode_basic_entities(input: &str) -> String {
    let map: HashMap<&str, &str> = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&nbsp;", " "),
    ]
    .into_iter()
    .collect();

    let mut out = input.to_string();
    for (k, v) in map.iter() {
        if out.contains(k) {
            out = out.replace(k, v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_extraction_basic() {
        let body = "<html><head><title>Vite + React</title></head></html>";
        assert_eq!(extract_title(body).as_deref(), Some("Vite + React"));
    }

    #[test]
    fn title_extraction_case_insensitive() {
        let body = "<HTML><HEAD><TITLE>  My App  </TITLE></HEAD>";
        assert_eq!(extract_title(body).as_deref(), Some("My App"));
    }

    #[test]
    fn title_extraction_entities() {
        let body = "<title>Foo &amp; Bar &lt;3</title>";
        assert_eq!(extract_title(body).as_deref(), Some("Foo & Bar <3"));
    }

    #[test]
    fn title_extraction_missing() {
        let body = "<html><body>no title here</body></html>";
        assert_eq!(extract_title(body), None);
    }

    #[test]
    fn title_extraction_empty() {
        let body = "<title>   </title>";
        assert_eq!(extract_title(body), None);
    }
}
