//! Request identity + the v2 bearer gate for the mobile server.
//!
//! Two layers:
//!
//! 1. [`identity`] (every route): when the request carries a
//!    `Tailscale-User-Login` header — injected by `tailscale serve` once it
//!    has authenticated the tailnet user — attach the login as an
//!    [`Identity`] extension. Attribution only; never rejects.
//! 2. [`v2_gate`] (the `/api/v2/*` routes except `/pair`): decide who the
//!    caller is with [`super::pairing::check_access`] — loopback + unforwarded
//!    stays open (the desktop webview, local dev, the PWA on 127.0.0.1); any
//!    other peer, anything forwarded by a proxy, and everything when
//!    `CORTEX_E2E_FORCE_AUTH=1` must present a paired device's bearer token or
//!    gets a JSON 401. The resolved [`Access`] is attached as an extension for
//!    handlers that care which device is calling.
//!
//! The legacy `/api/*` and `/v1/*` routes used by the current PWA are NOT
//! gated (the contract keeps them working until the new client replaces
//! them); security there still rests on the loopback bind + `tailscale
//! serve`, as before.

use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::{ConnectInfo, Request},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub use super::pairing::{Access, Denied};

/// The tailnet user login for the current request, when present. Attached as a
/// request extension by [`identity`]. Handlers can pull it with
/// `req.extensions().get::<Identity>()`.
#[derive(Debug, Clone)]
pub struct Identity(pub String);

/// Tailscale header carrying the authenticated user's login (e.g. an email).
const TAILSCALE_USER_HEADER: &str = "Tailscale-User-Login";

/// Middleware that reads `Tailscale-User-Login` and, if present, attaches it as
/// an [`Identity`] extension. Absent header → request proceeds anonymously.
pub async fn identity(mut req: Request, next: Next) -> Response {
    if let Some(login) = req
        .headers()
        .get(TAILSCALE_USER_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        req.extensions_mut().insert(Identity(login));
    }
    next.run(req).await
}

/// Was this request relayed by a proxy (`tailscale serve`, nginx, …)? Such a
/// request reaches us from loopback but is NOT local.
pub fn is_forwarded(headers: &HeaderMap) -> bool {
    headers.contains_key("x-forwarded-for")
        || headers.contains_key("forwarded")
        || headers.contains_key("x-real-ip")
        || headers.contains_key(TAILSCALE_USER_HEADER)
}

/// The TCP peer address recorded by `into_make_service_with_connect_info`.
pub fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// The JSON 401 body per the contract's error envelope.
pub fn unauthorized(d: Denied) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [("www-authenticate", "Bearer realm=\"cortex\"")],
        axum::Json(json!({
            "error": { "code": "unauthorized", "message": d.message() }
        })),
    )
        .into_response()
}

/// Decide access for a request from its peer + headers (+ optional `?token=`).
pub fn access_for(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<Access, Denied> {
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    super::pairing::check_access(
        peer,
        is_forwarded(headers),
        super::pairing::force_auth_env(),
        authorization,
        query_token,
    )
}

/// The v2 bearer gate (see module docs).
pub async fn v2_gate(mut req: Request, next: Next) -> Response {
    let peer = peer_ip(&req);
    match access_for(peer, req.headers(), None) {
        Ok(access) => {
            req.extensions_mut().insert(access);
            next.run(req).await
        }
        Err(d) => {
            tracing::debug!(
                peer = ?peer,
                path = %req.uri().path(),
                "mobile v2: rejected ({})",
                d.message()
            );
            unauthorized(d)
        }
    }
}
