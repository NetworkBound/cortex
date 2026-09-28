//! WebSocket fan-out: `GET /ws`. Every connected client receives a JSON frame
//! for each [`MobileEvent`] published by the POST handlers via the shared
//! `broadcast` channel in [`MobileState`].
//!
//! Each frame is the serialized `MobileEvent` (`{ "type": "...", ... }`). The
//! client switches on `type` and correlates on `run_id`. Inbound client
//! messages are accepted and ignored (the protocol is server-push only); a
//! `Close` frame ends the connection.
//!
//! # Origin check
//! Browsers do **not** apply CORS to WebSocket handshakes, so the CORS layer in
//! `router.rs` (which stops any website reading `/api/*` responses) does nothing
//! for `/ws`: a drive-by page could open `ws://127.0.0.1:8788/ws` and read every
//! streamed chat token (cross-site WebSocket hijacking). [`ws_handler`] therefore
//! rejects upgrades whose `Origin` is a browser origin we don't recognise — see
//! [`origin_permitted`]. Non-browser clients send no `Origin` and are unaffected.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use super::state::MobileState;

/// `GET /ws` upgrade handler. Registered with `any(...)` (not `get(...)`) in the
/// router so the upgrade negotiation isn't method-gated.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
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
    ws.on_upgrade(move |socket| client(socket, state))
        .into_response()
}

/// Decide whether a WebSocket upgrade from `origin` may proceed.
///
/// - No `Origin` header → allowed (native/CLI clients; browsers always send it).
/// - Origin in the CORS allow-list (`MOBILE_ALLOWED_ORIGINS`) → allowed.
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

async fn client(mut socket: WebSocket, state: MobileState) {
    let mut rx = state.events.subscribe();

    loop {
        tokio::select! {
            // Server-push: forward each broadcast event as a text frame.
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    let body = match serde_json::to_string(&ev) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(error = %e, "mobile ws: encode failed");
                            continue;
                        }
                    };
                    if socket.send(Message::Text(body)).await.is_err() {
                        return; // client gone
                    }
                }
                // Lagged: a slow client missed frames. Keep going from the
                // newest available frame rather than tearing the socket down.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "mobile ws: client lagged, dropping frames");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },

            // Inbound: drain client messages. We don't act on them (push-only),
            // but we must read so ping/pong + close are handled.
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(_)) => {}
                Some(Err(_)) => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::origin_permitted;

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
}
