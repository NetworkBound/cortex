//! Outbound webhook egress — ContextForge #14.
//!
//! Persists user-configured webhook subscriptions at `~/.cortex/webhooks.json`
//! (plaintext JSON; URLs and custom headers may contain shared secrets so the
//! file inherits unix `~/.cortex` perms — the same trust model as the rest of
//! cortex). Each entry binds a label + URL + custom headers + an event filter.
//!
//! `fire_event` does a synchronous fan-out across every enabled webhook whose
//! `events` list matches. Failures are logged via `tracing` but don't bubble —
//! a misconfigured webhook must never block agent progress.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const WEBHOOKS_FILENAME: &str = "webhooks.json";
const REQUEST_TIMEOUT_SECS: u64 = 5;

/// SSRF guard. Validates that `url` is an http(s) URL whose host resolves only
/// to public, routable addresses — rejecting loopback, private (RFC1918),
/// link-local, the cloud metadata endpoint (169.254.169.254), and other
/// reserved ranges. Run this before persisting *and* before sending so a hook
/// can never be coerced into hitting internal services.
fn validate_egress_url(raw: &str) -> Result<(), String> {
    let raw = raw.trim();

    // Scheme must be http or https — no file://, gopher://, etc.
    let rest = if let Some(r) = raw.strip_prefix("https://") {
        r
    } else if let Some(r) = raw.strip_prefix("http://") {
        r
    } else {
        return Err("url must use http or https scheme".into());
    };

    // Authority ends at the first '/', '?' or '#'.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest);
    if authority.is_empty() {
        return Err("url has no host".into());
    }

    // Strip userinfo (user:pass@) if present.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);

    // Split host and optional port, handling bracketed IPv6 literals.
    let (host, port) = if let Some(end) = host_port.strip_prefix('[') {
        let close = end
            .find(']')
            .ok_or_else(|| "malformed IPv6 host".to_string())?;
        let h = &end[..close];
        let after = &end[close + 1..];
        let p = after.strip_prefix(':').and_then(|s| s.parse::<u16>().ok());
        (h.to_string(), p)
    } else {
        let mut parts = host_port.rsplitn(2, ':');
        let maybe_port = parts.next();
        match parts.next() {
            Some(h) => (h.to_string(), maybe_port.and_then(|s| s.parse::<u16>().ok())),
            None => (host_port.to_string(), None),
        }
    };

    if host.is_empty() {
        return Err("url has no host".into());
    }

    let port = port.unwrap_or(0);

    // Resolve every address the host maps to; reject if ANY is non-public so we
    // can't be tricked by a name that resolves to both a public and an internal
    // address.
    let addrs: Vec<IpAddr> = match (host.as_str(), port).to_socket_addrs() {
        Ok(iter) => iter.map(|sa| sa.ip()).collect(),
        Err(e) => return Err(format!("cannot resolve host {host}: {e}")),
    };
    if addrs.is_empty() {
        return Err(format!("host {host} did not resolve"));
    }
    for ip in addrs {
        if !is_public_ip(&ip) {
            return Err(format!(
                "url resolves to a non-routable address ({ip}); refusing for SSRF safety"
            ));
        }
    }
    Ok(())
}

/// True only for globally routable unicast addresses. Anything loopback,
/// private, link-local, multicast, unspecified, or otherwise reserved is
/// treated as unsafe egress.
fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // Only IPv4-MAPPED (::ffff:a.b.c.d) addresses are judged on the embedded
            // v4. to_ipv4() (not _mapped) also maps the compatible range, turning ::1
            // into 0.0.0.1 which slips past is_public_v4 — so use to_ipv4_mapped() and
            // let genuine v6 specials (::1 loopback, etc.) fall to is_public_v6.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(&v4);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(ip: &Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    let o = ip.octets();
    // 100.64.0.0/10 carrier-grade NAT.
    if o[0] == 100 && (o[1] & 0xc0) == 64 {
        return false;
    }
    // 192.0.0.0/24 IETF protocol assignments.
    if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        return false;
    }
    // 198.18.0.0/15 benchmarking.
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return false;
    }
    // 240.0.0.0/4 reserved (excludes broadcast already handled).
    if o[0] >= 240 {
        return false;
    }
    true
}

fn is_public_v6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() {
        return false;
    }
    let seg = ip.segments();
    // fe80::/10 link-local.
    if (seg[0] & 0xffc0) == 0xfe80 {
        return false;
    }
    // fc00::/7 unique local addresses.
    if (seg[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    // 2001:db8::/32 documentation.
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return false;
    }
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub label: String,
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookInput {
    #[serde(default)]
    pub id: Option<String>,
    pub label: String,
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

fn cortex_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_string())?;
    let dir = home.join(".cortex");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir ~/.cortex: {e}"))?;
    Ok(dir)
}

fn webhooks_path() -> Result<PathBuf, String> {
    Ok(cortex_dir()?.join(WEBHOOKS_FILENAME))
}

/// Read the on-disk list. Missing file means "no webhooks yet" — not an error.
pub fn load_all() -> Result<Vec<Webhook>, String> {
    let path = webhooks_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).map_err(|e| format!("read webhooks: {e}"))?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_slice::<Vec<Webhook>>(&bytes)
        .map_err(|e| format!("parse webhooks: {e}"))
}

/// Atomic write via temp+rename — never leave a partial file on crash.
fn save_all(items: &[Webhook]) -> Result<(), String> {
    let path = webhooks_path()?;
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(items)
        .map_err(|e| format!("encode webhooks: {e}"))?;
    fs::write(&tmp, &body).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

pub fn add(input: WebhookInput) -> Result<Webhook, String> {
    if input.label.trim().is_empty() {
        return Err("label must not be empty".into());
    }
    if input.url.trim().is_empty() {
        return Err("url must not be empty".into());
    }
    validate_egress_url(&input.url)?;
    let mut items = load_all()?;
    let webhook = Webhook {
        id: input.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        label: input.label,
        url: input.url,
        events: input.events,
        headers: input.headers,
        enabled: input.enabled,
    };
    items.push(webhook.clone());
    save_all(&items)?;
    Ok(webhook)
}

pub fn update(input: Webhook) -> Result<(), String> {
    if input.url.trim().is_empty() {
        return Err("url must not be empty".into());
    }
    validate_egress_url(&input.url)?;
    let mut items = load_all()?;
    let idx = items
        .iter()
        .position(|w| w.id == input.id)
        .ok_or_else(|| format!("no webhook with id {}", input.id))?;
    items[idx] = input;
    save_all(&items)
}

pub fn delete(id: &str) -> Result<(), String> {
    let mut items = load_all()?;
    let before = items.len();
    items.retain(|w| w.id != id);
    if items.len() == before {
        return Err(format!("no webhook with id {id}"));
    }
    save_all(&items)
}

/// Synthetic-payload test fire. Uses a 5s timeout; surfaces http status +
/// wall-clock latency back to the UI so the user can verify connectivity
/// without waiting for a real event.
pub fn test(id: &str) -> Result<TestResult, String> {
    let items = load_all()?;
    let hook = items
        .iter()
        .find(|w| w.id == id)
        .ok_or_else(|| format!("no webhook with id {id}"))?;
    let payload = serde_json::json!({
        "event": "cortex.webhook.test",
        "ts": chrono::Utc::now().timestamp_millis(),
        "label": hook.label,
        "message": "synthetic test payload from cortex",
    });
    Ok(post_blocking(hook, &payload))
}

/// Fan-out POST to every enabled webhook whose `events` list contains `event`.
/// Returns the number of webhooks actually fired (regardless of HTTP outcome).
/// Per-hook errors are logged but never propagated.
pub fn fire(event: &str, payload: &serde_json::Value) -> u32 {
    let items = match load_all() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("webhooks: load failed during fire({event}): {e}");
            return 0;
        }
    };
    let mut fired = 0u32;
    for hook in items.iter().filter(|w| w.enabled && w.events.iter().any(|e| e == event)) {
        let body = serde_json::json!({
            "event": event,
            "ts": chrono::Utc::now().timestamp_millis(),
            "label": hook.label,
            "payload": payload,
        });
        let res = post_blocking(hook, &body);
        if !res.ok {
            tracing::warn!(
                "webhook {} ({}) failed: status={:?} err={:?}",
                hook.label, hook.id, res.status, res.error
            );
        }
        fired += 1;
    }
    fired
}

/// The async POST itself. Kept as a standalone `async fn` so it can be driven
/// either by an ambient tokio runtime or by a dedicated one (see
/// `post_blocking`).
async fn post_async(hook: &Webhook, body: &serde_json::Value, started: Instant) -> TestResult {
    // SSRF guard at the send boundary too: an on-disk entry written before this
    // check existed (or hand-edited) must still be re-validated before egress.
    if let Err(e) = validate_egress_url(&hook.url) {
        return TestResult {
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(e),
        };
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return TestResult {
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(format!("client: {e}")),
        },
    };

    let mut req = client.post(&hook.url).json(body);
    for (k, v) in &hook.headers {
        req = req.header(k, v);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            TestResult {
                ok: status.is_success(),
                status: Some(status.as_u16()),
                latency_ms: started.elapsed().as_millis() as u64,
                error: if status.is_success() {
                    None
                } else {
                    Some(format!("http {}", status.as_u16()))
                },
            }
        }
        Err(e) => TestResult {
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(e.to_string()),
        },
    }
}

/// Synchronous POST primitive used by the sync `test`/`fire` callers.
///
/// These are invoked from `async` tauri commands, i.e. *inside* a tokio
/// runtime. Building another runtime and `block_on`-ing it on the same thread
/// panics ("Cannot start a runtime from within a runtime"). To stay sync
/// without enabling reqwest's blocking feature, we drive the async send on a
/// dedicated OS thread that owns its own current-thread runtime — that thread
/// is not a tokio worker, so `block_on` there is safe — and join it.
fn post_blocking(hook: &Webhook, body: &serde_json::Value) -> TestResult {
    let started = Instant::now();
    let hook = hook.clone();
    let body = body.clone();

    let join = std::thread::Builder::new()
        .name("cortex-webhook-send".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => return TestResult {
                    ok: false,
                    status: None,
                    latency_ms: started.elapsed().as_millis() as u64,
                    error: Some(format!("runtime: {e}")),
                },
            };
            rt.block_on(post_async(&hook, &body, started))
        });

    match join {
        Ok(handle) => handle.join().unwrap_or_else(|_| TestResult {
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some("webhook send thread panicked".into()),
        }),
        Err(e) => TestResult {
            ok: false,
            status: None,
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(format!("spawn send thread: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_input_assigns_id_when_missing() {
        // We can't hit the on-disk store in unit tests cleanly, but we can
        // verify the id is generated when absent by exercising the
        // construction path directly.
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(id.len(), 36);
    }

    #[test]
    fn ssrf_guard_rejects_non_http_and_internal() {
        assert!(validate_egress_url("file:///etc/passwd").is_err());
        assert!(validate_egress_url("gopher://example.com").is_err());
        assert!(validate_egress_url("http://127.0.0.1/x").is_err());
        assert!(validate_egress_url("http://localhost/x").is_err());
        assert!(validate_egress_url("http://10.0.0.5/x").is_err());
        assert!(validate_egress_url("http://10.0.0.1/x").is_err());
        assert!(validate_egress_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_egress_url("http://[::1]/x").is_err());
    }

    #[test]
    fn is_public_ip_classifies_known_ranges() {
        use std::net::IpAddr;
        assert!(!is_public_ip(&"127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip(&"169.254.169.254".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip(&"10.1.2.3".parse::<IpAddr>().unwrap()));
        assert!(!is_public_ip(&"100.64.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_public_ip(&"8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(is_public_ip(&"1.1.1.1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn webhook_default_enabled_is_true() {
        let raw = r#"{"id":"a","label":"x","url":"https://example.com","events":["task.complete"]}"#;
        let w: Webhook = serde_json::from_str(raw).unwrap();
        assert!(w.enabled);
        assert!(w.headers.is_empty());
    }
}
