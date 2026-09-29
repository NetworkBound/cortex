//! WebSocket fan-out: `GET /ws`.
//!
//! Two vocabularies share the socket:
//!
//! - **Legacy** (`MobileEvent`, `{ "type": "chat_token", ... }`): what the
//!   current PWA consumes. Forwarded until the client subscribes to v2.
//! - **v2** (`events::V2Event`, the mobile contract): forwarded once the
//!   client sends `{ "type": "subscribe", "threads": ["*"] | [ids], "since_ms"? }`.
//!   On subscribe, `thread_updated` frames are replayed for every thread
//!   touched since `since_ms` so a backgrounded phone can catch up without a
//!   full refetch. A `{ "type": "ping" }` is sent every 25 s.
//!
//! # Auth
//! Non-local peers (see `pairing::is_local_peer`) must authenticate: either
//! `?token=<bearer>` on the upgrade URL, or a first frame
//! `{ "type": "auth", "token": "<bearer>" }` within [`AUTH_GRACE`]. Until then
//! nothing is forwarded; a bad/missing token closes the socket. Local peers
//! are open, as on the legacy routes.
//!
//! # Origin check
//! Browsers do **not** apply CORS to WebSocket handshakes, so the CORS layer in
//! `router.rs` (which stops any website reading `/api/*` responses) does nothing
//! for `/ws`: a drive-by page could open `ws://127.0.0.1:8788/ws` and read every
//! streamed chat token (cross-site WebSocket hijacking). [`ws_handler`] therefore
//! rejects upgrades whose `Origin` is a browser origin we don't recognise — see
//! [`origin_permitted`]. Non-browser clients send no `Origin` and are unaffected.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::auth::{self, Access};
use super::events::V2Event;
use super::state::MobileState;

/// How long an unauthenticated remote socket may wait for its `auth` frame.
const AUTH_GRACE: Duration = Duration::from_secs(10);
/// Keepalive interval.
const PING_INTERVAL: Duration = Duration::from_secs(25);

/// `GET /ws` upgrade handler. Registered with `any(...)` (not `get(...)`) in the
/// router so the upgrade negotiation isn't method-gated.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<MobileState>,
) -> Response {
    let hdr = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    let origin = hdr(header::ORIGIN);
    let host = hdr(header::HOST);
    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok());
    if !origin_permitted(origin, host, forwarded_host) {
        tracing::warn!(
            origin = origin.unwrap_or(""),
            "mobile ws: rejected cross-origin upgrade"
        );
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    // Pre-upgrade auth: local peers and `?token=` holders are decided now; a
    // remote peer without a token gets the socket but must send an `auth`
    // frame first (the browser WebSocket API can't set headers).
    let peer_ip = peer.map(|c| c.0.ip());
    let access = match auth::access_for(peer_ip, &headers, query.get("token").map(String::as_str)) {
        Ok(a) => Some(a),
        Err(super::pairing::Denied::Invalid) => {
            return auth::unauthorized(super::pairing::Denied::Invalid);
        }
        Err(super::pairing::Denied::Missing) => None,
    };
    ws.on_upgrade(move |socket| client(socket, state, access))
        .into_response()
}

/// Decide whether a WebSocket upgrade from `origin` may proceed.
///
/// - No `Origin` header → allowed (native/CLI clients; browsers always send it).
/// - Origin in the CORS allow-list (`MOBILE_ALLOWED_ORIGINS`) → allowed. That
///   list includes the Capacitor shells (`capacitor://localhost` on iOS,
///   `https://localhost` on Android) — they still need a bearer to do
///   anything.
/// - Origin authority equal to the request's `Host` (or `X-Forwarded-Host`, set
///   by `tailscale serve`) → allowed: that's the bundled SPA served same-origin,
///   whatever hostname it was reached on.
/// - Origin host ending in `.ts.net` → allowed (the tailnet-served SPA, in case
///   the proxy rewrote `Host`).
/// - Anything else (any real website, `null`) → rejected.
pub(super) fn origin_permitted(
    origin: Option<&str>,
    host: Option<&str>,
    forwarded_host: Option<&str>,
) -> bool {
    let Some(origin) = origin.map(str::trim).filter(|o| !o.is_empty()) else {
        return true;
    };
    if super::router::MOBILE_ALLOWED_ORIGINS
        .iter()
        .any(|a| a.eq_ignore_ascii_case(origin))
    {
        return true;
    }
    let Some(authority) = origin_authority(origin) else {
        return false;
    };
    let same_host = |h: Option<&str>| {
        h.map(str::trim)
            .filter(|h| !h.is_empty())
            .map(|h| h.eq_ignore_ascii_case(authority))
            .unwrap_or(false)
    };
    if same_host(host) || same_host(forwarded_host) {
        return true;
    }
    // Bare hostname (strip `:port`), ignoring bracketed IPv6 literals.
    let hostname = if authority.starts_with('[') {
        authority
    } else {
        authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority)
    };
    hostname.to_ascii_lowercase().ends_with(".ts.net")
}

/// `scheme://authority[/...]` → `authority` (host[:port]); `None` when the value
/// isn't a URL-shaped origin (e.g. `null`).
fn origin_authority(origin: &str) -> Option<&str> {
    let (_, rest) = origin.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        None
    } else {
        Some(authority)
    }
}

/// Which threads a v2 subscriber wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadFilter {
    All,
    Only(Vec<String>),
}

impl ThreadFilter {
    pub fn allows(&self, thread_id: Option<&str>) -> bool {
        match (self, thread_id) {
            (ThreadFilter::All, _) => true,
            // Thread-less frames (ping) always pass.
            (_, None) => true,
            (ThreadFilter::Only(ids), Some(t)) => ids.iter().any(|i| i == t),
        }
    }
}

/// Parse a `subscribe` frame's `threads` list: `["*"]` / missing → all.
pub fn parse_thread_filter(v: &Value) -> ThreadFilter {
    match v.get("threads").and_then(Value::as_array) {
        None => ThreadFilter::All,
        Some(items) => {
            let ids: Vec<String> = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            if ids.is_empty() || ids.iter().any(|i| i == "*") {
                ThreadFilter::All
            } else {
                ThreadFilter::Only(ids)
            }
        }
    }
}

fn text_frame<T: serde::Serialize>(v: &T) -> Option<Message> {
    match serde_json::to_string(v) {
        Ok(b) => Some(Message::Text(b)),
        Err(e) => {
            tracing::warn!(error = %e, "mobile ws: encode failed");
            None
        }
    }
}

async fn client(mut socket: WebSocket, state: MobileState, mut access: Option<Access>) {
    let mut legacy_rx = state.events.subscribe();
    let mut v2_rx = state.v2.subscribe();
    let mut filter: Option<ThreadFilter> = None; // Some(..) once subscribed to v2
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await; // first tick fires immediately; skip it
    let auth_deadline = tokio::time::Instant::now() + AUTH_GRACE;

    loop {
        tokio::select! {
            // Legacy push (until the client switches to v2).
            ev = legacy_rx.recv() => match ev {
                Ok(ev) => {
                    if access.is_none() || filter.is_some() {
                        continue;
                    }
                    if let Some(frame) = text_frame(&ev) {
                        if socket.send(frame).await.is_err() {
                            return;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "mobile ws: client lagged, dropping frames");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },

            // v2 push (once subscribed).
            ev = v2_rx.recv() => match ev {
                Ok(ev) => {
                    let Some(f) = &filter else { continue };
                    if access.is_none() || !f.allows(ev.thread_id()) {
                        continue;
                    }
                    if let Some(frame) = text_frame(&ev) {
                        if socket.send(frame).await.is_err() {
                            return;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "mobile ws: v2 client lagged, dropping frames");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },

            // Keepalive; also enforces the auth grace period.
            _ = ping.tick() => {
                if access.is_none() {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                if filter.is_some() {
                    if let Some(frame) = text_frame(&V2Event::Ping) {
                        if socket.send(frame).await.is_err() {
                            return;
                        }
                    }
                } else if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    return;
                }
            },

            // Auth grace timeout for remote sockets that never authenticated.
            _ = tokio::time::sleep_until(auth_deadline), if access.is_none() => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            },

            // Inbound: `auth`, `subscribe`; everything else ignored.
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(Message::Text(text))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                    match v.get("type").and_then(Value::as_str) {
                        Some("auth") => {
                            if access.is_some() {
                                continue;
                            }
                            let token = v.get("token").and_then(Value::as_str).unwrap_or("");
                            match super::pairing::authenticate(token) {
                                Some(dev) => {
                                    access = Some(Access::Device(dev));
                                    if let Some(frame) = text_frame(&serde_json::json!({
                                        "type": "auth_ok"
                                    })) {
                                        if socket.send(frame).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                None => {
                                    if let Some(frame) = text_frame(&serde_json::json!({
                                        "type": "error",
                                        "thread_id": "",
                                        "run_id": "",
                                        "message": "unauthorized: invalid bearer token"
                                    })) {
                                        let _ = socket.send(frame).await;
                                    }
                                    let _ = socket.send(Message::Close(None)).await;
                                    return;
                                }
                            }
                        }
                        Some("subscribe") => {
                            if access.is_none() {
                                continue;
                            }
                            let f = parse_thread_filter(&v);
                            // Catch-up: replay `thread_updated` for threads
                            // touched since the client last saw us.
                            if let Some(since) = v.get("since_ms").and_then(Value::as_i64) {
                                for id in state.v2.threads_active_since(since) {
                                    if !f.allows(Some(&id)) {
                                        continue;
                                    }
                                    if let Some(thread) =
                                        super::threads::thread_view(&state.store, &state.v2, &id)
                                    {
                                        if let Some(frame) =
                                            text_frame(&V2Event::ThreadUpdated { thread })
                                        {
                                            if socket.send(frame).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                            filter = Some(f);
                            if let Some(frame) = text_frame(&serde_json::json!({
                                "type": "subscribed",
                                "threads": v.get("threads").cloned().unwrap_or(Value::Array(vec![Value::String("*".into())])),
                            })) {
                                if socket.send(frame).await.is_err() {
                                    return;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{origin_permitted, parse_thread_filter, ThreadFilter};

    #[test]
    fn missing_origin_is_allowed_for_native_clients() {
        assert!(origin_permitted(None, Some("127.0.0.1:8788"), None));
        assert!(origin_permitted(Some("  "), Some("127.0.0.1:8788"), None));
    }

    #[test]
    fn allow_listed_origins_pass() {
        assert!(origin_permitted(Some("http://localhost:8788"), None, None));
        assert!(origin_permitted(Some("tauri://localhost"), None, None));
        assert!(origin_permitted(Some("HTTP://LOCALHOST:1420"), None, None));
        // Capacitor shells.
        assert!(origin_permitted(Some("capacitor://localhost"), None, None));
        assert!(origin_permitted(Some("https://localhost"), None, None));
    }

    #[test]
    fn same_origin_via_host_or_forwarded_host_passes() {
        assert!(origin_permitted(
            Some("https://cortex.tail1234.ts.net"),
            Some("cortex.tail1234.ts.net"),
            None
        ));
        assert!(origin_permitted(
            Some("http://192.168.1.20:8788"),
            Some("127.0.0.1:8788"),
            Some("192.168.1.20:8788")
        ));
    }

    #[test]
    fn tailnet_origin_passes_even_if_host_was_rewritten() {
        assert!(origin_permitted(
            Some("https://cortex.tail1234.ts.net"),
            Some("127.0.0.1:8788"),
            None
        ));
    }

    #[test]
    fn foreign_websites_and_null_are_rejected() {
        assert!(!origin_permitted(
            Some("https://evil.example"),
            Some("127.0.0.1:8788"),
            None
        ));
        assert!(!origin_permitted(
            Some("http://127.0.0.1:9999"),
            Some("127.0.0.1:8788"),
            None
        ));
        assert!(!origin_permitted(
            Some("null"),
            Some("127.0.0.1:8788"),
            None
        ));
        // A lookalike that merely *contains* the host must not pass.
        assert!(!origin_permitted(
            Some("https://cortex.tail1234.ts.net.evil.example"),
            Some("cortex.tail1234.ts.net"),
            None
        ));
    }

    #[test]
    fn subscribe_filter_parsing() {
        let all =
            parse_thread_filter(&serde_json::json!({ "type": "subscribe", "threads": ["*"] }));
        assert_eq!(all, ThreadFilter::All);
        assert!(all.allows(Some("anything")));
        let none_given = parse_thread_filter(&serde_json::json!({ "type": "subscribe" }));
        assert_eq!(none_given, ThreadFilter::All);
        let some = parse_thread_filter(&serde_json::json!({ "threads": ["a", "b"] }));
        assert_eq!(some, ThreadFilter::Only(vec!["a".into(), "b".into()]));
        assert!(some.allows(Some("a")));
        assert!(!some.allows(Some("c")));
        assert!(some.allows(None)); // ping
        assert_eq!(
            parse_thread_filter(&serde_json::json!({ "threads": [] })),
            ThreadFilter::All
        );
    }
}
