//! Tailscale-identity middleware for the mobile server.
//!
//! When the request carries a `Tailscale-User-Login` header — injected by
//! `tailscale serve` once it has authenticated the tailnet user — we attach the
//! login as a request extension so handlers can read the caller's identity. We
//! deliberately do **not** reject requests that lack the header: in local dev
//! the server is reached directly over loopback with no proxy in front, and
//! hard-failing would make the whole surface unusable there.
//!
//! Security rests on the `127.0.0.1` bind plus `tailscale serve` terminating
//! and authenticating in front — see the module docs in `mobile_server/mod.rs`.
//! This middleware is identity *attribution*, not access control.

use axum::{extract::Request, middleware::Next, response::Response};

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
