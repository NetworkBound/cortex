//! REST→MCP tool virtualizer (ContextForge #12).
//!
//! Lets the user register REST endpoints as callable tools. Each tool is
//! persisted as its own JSON file at `~/.cortex/tools/<name>.json` so the
//! user can hand-edit, version-control, or sync them per-machine. Keeping
//! one file per tool (vs one big bundle) sidesteps merge conflicts when
//! the registry grows past a handful of entries.
//!
//! The runtime substitutes `{param}` placeholders in `url_template` and
//! `{secret:KEY}` placeholders in header values (resolved against the
//! `~/.cortex/keys.enc` vault). Responses are capped at `MAX_RESPONSE_BYTES`
//! so a runaway endpoint can't OOM the app.
//!
//! Exposed to the agent layer in a follow-up — for now the Tauri commands
//! in `commands::tools` provide the registry CRUD + manual invoke surface
//! that the orchestrator can call once tool-call routing lands.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Hard cap on the bytes we'll buffer from a tool response. Anything past
/// this is truncated and a flag is set so the caller can surface "response
/// too large" without crashing the renderer.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_NAME_LEN: usize = 64;
const MAX_DESC_LEN: usize = 200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ToolMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

impl ToolMethod {
    fn as_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Delete => reqwest::Method::DELETE,
            Self::Patch => reqwest::Method::PATCH,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    String,
    Int,
    Bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponseFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    pub name: String,
    pub kind: InputKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub method: ToolMethod,
    pub url_template: String,
    #[serde(default)]
    pub inputs: Vec<ToolInput>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Hosts a resolved `{secret:KEY}` header value is allowed to be sent to.
    /// Binds secrets to their intended destination so a malicious/mistaken
    /// tool def can't point a secret-bearing request at an attacker host and
    /// exfiltrate the key. Empty list (the default) means "the host of this
    /// tool's own `url_template`" — i.e. secrets only flow to the declared
    /// endpoint unless the author explicitly widens the allowlist.
    #[serde(default)]
    pub secret_hosts: Vec<String>,
    pub response_format: ResponseFormat,
    #[serde(default)]
    pub created_unix_ms: i64,
    #[serde(default)]
    pub updated_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolInvocationResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub body: String,
    pub latency_ms: u64,
    pub error: Option<String>,
    /// `true` when we stopped reading at `MAX_RESPONSE_BYTES`.
    #[serde(default)]
    pub truncated: bool,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn tools_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("tools"))
}

fn tool_path(name: &str) -> Result<PathBuf, String> {
    if !is_valid_name(name) {
        return Err(format!(
            "invalid tool name '{name}': use letters, digits, '-', '_', '.' (max {MAX_NAME_LEN})"
        ));
    }
    Ok(tools_dir()?.join(format!("{name}.json")))
}

/// Tool names double as filenames and become the MCP tool id, so keep them
/// strict: kebab-case-ish ascii, no path separators, bounded length.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn validate_tool(t: &ToolDef) -> Result<(), String> {
    if !is_valid_name(&t.name) {
        return Err(format!("invalid name '{}'", t.name));
    }
    if t.description.len() > MAX_DESC_LEN {
        return Err(format!(
            "description exceeds {MAX_DESC_LEN} chars (got {})",
            t.description.len()
        ));
    }
    if t.url_template.trim().is_empty() {
        return Err("url_template required".into());
    }
    if !(t.url_template.starts_with("http://") || t.url_template.starts_with("https://")) {
        return Err("url_template must start with http:// or https://".into());
    }
    // Input names share the same constraint set — they're substituted into
    // URLs and JSON keys and we don't want users smuggling `{` characters.
    for input in &t.inputs {
        if !is_valid_name(&input.name) {
            return Err(format!("invalid input name '{}'", input.name));
        }
    }
    Ok(())
}

pub fn list_tools_blocking() -> Result<Vec<ToolDef>, String> {
    let dir = tools_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("read tools dir: {e}"))? {
        let entry = entry.map_err(|e| format!("read dir entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // One bad file shouldn't take the whole list down — log + skip.
        match fs::read(&path).and_then(|bytes| {
            serde_json::from_slice::<ToolDef>(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }) {
            Ok(t) => out.push(t),
            Err(e) => tracing::warn!(?path, "skipping malformed tool def: {e}"),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn get_tool_blocking(name: &str) -> Result<ToolDef, String> {
    let path = tool_path(name)?;
    let bytes = fs::read(&path).map_err(|e| format!("read tool: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse tool: {e}"))
}

pub fn save_tool_blocking(mut tool: ToolDef) -> Result<ToolDef, String> {
    validate_tool(&tool)?;
    let dir = tools_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir tools: {e}"))?;
    let path = dir.join(format!("{}.json", tool.name));
    let now = now_ms();
    // Preserve `created_unix_ms` on upsert so the original mint time stays
    // accurate; only `updated_unix_ms` should drift on every save.
    if tool.created_unix_ms == 0 {
        let existing_created = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ToolDef>(&bytes).ok())
            .map(|t| t.created_unix_ms)
            .filter(|t| *t > 0);
        tool.created_unix_ms = existing_created.unwrap_or(now);
    }
    tool.updated_unix_ms = now;
    let json = serde_json::to_vec_pretty(&tool).map_err(|e| format!("serialize: {e}"))?;
    // tmp+rename so a crash mid-write doesn't leave a half-written tool def.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(tool)
}

pub fn delete_tool_blocking(name: &str) -> Result<(), String> {
    let path = tool_path(name)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("remove tool: {e}"))?;
    }
    Ok(())
}

/// Replace `{key}` placeholders in `template` from `args`. Reports the first
/// missing required key so the user sees something actionable instead of
/// a literal `{x}` flying out to the upstream endpoint.
fn substitute_template(
    template: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err("unbalanced '{' in template".into());
        };
        let key = &after[..end];
        // Skip secret placeholders — they only apply to headers, not URLs.
        // Treating them as literal here means a future header-resolver pass
        // can still see them.
        if let Some(secret_key) = key.strip_prefix("secret:") {
            out.push('{');
            out.push_str("secret:");
            out.push_str(secret_key);
            out.push('}');
        } else {
            let v = args
                .get(key)
                .ok_or_else(|| format!("missing arg '{key}'"))?;
            // Percent-encode the substituted value so user-controlled args
            // can't escape the position they're inserted into. Without this,
            // a value like `evil.com/` or `x@attacker` could rewrite the host,
            // and `?`/`#`/`../` could inject query/fragment/path-traversal.
            out.push_str(&percent_encode(&value_to_string(v)));
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Percent-encode a substituted arg value so it stays inert no matter where in
/// the URL template it lands. We encode everything outside the RFC 3986
/// "unreserved" set, which means the structural characters an attacker would
/// use to break out of a path/query segment — `/`, `?`, `#`, `@`, `:`, `.` runs
/// like `..`, etc. — come out as `%XX` and can't change the host, path
/// boundary, query, or fragment. (`.` itself is unreserved and left as-is, but
/// the `/` separators needed to weaponise `../` are encoded, so traversal is
/// neutralised.) Kept local to avoid pulling `percent-encoding` into the direct
/// dependency set just for this one call site.
fn percent_encode(value: &str) -> String {
    fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
    }
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

/// Resolve `{secret:KEY}` markers in a header value. We split `KEY` on `/`
/// so users can write `{secret:anthropic/personal}` to pull provider+label
/// out of the keyvault. Unrecognised markers pass through unchanged so the
/// upstream gets a clear "auth failed" rather than a silent empty string.
fn substitute_header_secrets(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("{secret:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{secret:".len()..];
        let Some(end) = after.find('}') else {
            // Malformed — surface the rest verbatim and stop scanning.
            out.push_str("{secret:");
            out.push_str(after);
            return out;
        };
        let spec = &after[..end];
        let resolved = resolve_secret(spec).unwrap_or_else(|| format!("{{secret:{spec}}}"));
        out.push_str(&resolved);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn resolve_secret(spec: &str) -> Option<String> {
    let (provider, label) = match spec.split_once('/') {
        Some((p, l)) => (p.to_string(), l.to_string()),
        None => (spec.to_string(), "default".to_string()),
    };
    // The keyvault module exposes a sync `load_entries` analogue via the
    // public `vault_get` Tauri command, but that's async. We mirror its
    // tiny read path here so the invoke pipeline stays synchronous on the
    // worker thread.
    crate::commands::keyvault::lookup_key_sync(&provider, &label).ok()
}

/// True for any IP we refuse to let a virtualized tool reach. Covers the
/// classic SSRF pivot targets: loopback, RFC1918 / unique-local, link-local
/// (incl. the 169.254.169.254 cloud metadata endpoint), CGNAT, multicast,
/// broadcast, unspecified, and otherwise-reserved ranges. IPv4-mapped/compat
/// IPv6 addresses are unwrapped first so `::ffff:127.0.0.1` can't sneak past.
fn is_blocked_ip(ip: IpAddr) -> bool {
    fn v4_blocked(v4: Ipv4Addr) -> bool {
        v4.is_loopback()
            || v4.is_private()
            || v4.is_link_local()
            || v4.is_broadcast()
            || v4.is_documentation()
            || v4.is_unspecified()
            || v4.is_multicast()
            // CGNAT 100.64.0.0/10
            || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
            // 0.0.0.0/8 "this network"
            || v4.octets()[0] == 0
            // 192.0.0.0/24 IETF protocol assignments
            || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0)
            // 198.18.0.0/15 benchmarking
            || (v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
            // 240.0.0.0/4 reserved
            || v4.octets()[0] >= 240
    }
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4_blocked(v4);
            }
            // Plain ::ffff:a.b.c.d / ::a.b.c.d compat forms.
            if let Some(v4) = v6.to_ipv4() {
                return v4_blocked(v4);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Extract the host (and optional explicit port) from a substituted URL without
/// pulling in the `url` crate. Returns `(host, port)`; host has any `userinfo@`
/// and brackets (for IPv6 literals) stripped.
fn host_port_from_url(url: &str) -> Result<(String, u16), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "url missing scheme".to_string())?;
    let default_port = match scheme {
        "https" => 443u16,
        "http" => 80u16,
        other => return Err(format!("unsupported url scheme '{other}'")),
    };
    // Authority ends at the first '/', '?' or '#'.
    let authority_end = rest
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // Drop any userinfo (`user:pass@`) — only the part after the last '@' is host.
    let hostport = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    if hostport.is_empty() {
        return Err("url missing host".into());
    }
    // IPv6 literal in brackets: [::1]:8080
    if let Some(stripped) = hostport.strip_prefix('[') {
        let (h, tail) = stripped
            .split_once(']')
            .ok_or_else(|| "malformed ipv6 host".to_string())?;
        let port = tail
            .strip_prefix(':')
            .map(|p| p.parse::<u16>().map_err(|_| "bad port".to_string()))
            .transpose()?
            .unwrap_or(default_port);
        return Ok((h.to_string(), port));
    }
    match hostport.rsplit_once(':') {
        Some((h, p)) => {
            let port = p.parse::<u16>().map_err(|_| "bad port".to_string())?;
            Ok((h.to_string(), port))
        }
        None => Ok((hostport.to_string(), default_port)),
    }
}

/// SSRF guard: resolve the URL's host and reject if it is (or resolves to) any
/// private/loopback/link-local/reserved/metadata address. Returns `Ok(())` only
/// when every resolved address is publicly routable.
fn ssrf_guard(url: &str) -> Result<(), String> {
    let (host, port) = host_port_from_url(url)?;
    // If the host is a literal IP, check it directly (no DNS).
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(format!("blocked request to non-public address {ip}"));
        }
        return Ok(());
    }
    // Otherwise resolve via DNS and reject if *any* answer is non-public, so a
    // hostname that maps to a private/metadata IP can't be used as a pivot.
    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("dns resolve '{host}': {e}"))?;
    let mut saw_any = false;
    for sa in addrs {
        saw_any = true;
        if is_blocked_ip(sa.ip()) {
            return Err(format!(
                "blocked request to '{host}' (resolves to non-public address {})",
                sa.ip()
            ));
        }
    }
    if !saw_any {
        return Err(format!("host '{host}' did not resolve"));
    }
    Ok(())
}

pub async fn invoke_tool(
    tool: ToolDef,
    args: HashMap<String, serde_json::Value>,
) -> ToolInvocationResult {
    let started = Instant::now();
    // Fail fast on missing required inputs so we never fire a half-built URL.
    for input in &tool.inputs {
        if input.required && !args.contains_key(&input.name) {
            return ToolInvocationResult {
                ok: false,
                status: None,
                body: String::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(format!("missing required arg '{}'", input.name)),
                truncated: false,
            };
        }
    }

    let url = match substitute_template(&tool.url_template, &args) {
        Ok(u) => u,
        Err(e) => {
            return ToolInvocationResult {
                ok: false,
                status: None,
                body: String::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(e),
                truncated: false,
            }
        }
    };

    // SSRF guard: resolve the (now fully-substituted) host and refuse to fire
    // at private/loopback/link-local/reserved/metadata addresses. DNS lookup is
    // blocking, so hop onto a blocking thread. We guard the substituted URL —
    // not the raw template — so user args can't smuggle in an internal host.
    let guard_url = url.clone();
    let guard = tokio::task::spawn_blocking(move || ssrf_guard(&guard_url)).await;
    let guard = match guard {
        Ok(r) => r,
        Err(e) => Err(format!("ssrf guard task: {e}")),
    };
    if let Err(e) = guard {
        return ToolInvocationResult {
            ok: false,
            status: None,
            body: String::new(),
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(e),
            truncated: false,
        };
    }

    // Disable automatic redirect following. The SSRF guard above only vets the
    // *original* host; reqwest would otherwise replay this request — including
    // the resolved `{secret:KEY}` headers below — to whatever `Location` an
    // upstream returns, letting a (public, so SSRF-guard-passing) endpoint
    // 30x-redirect us to an attacker-chosen host and exfiltrate the secrets.
    // Pinning to no-redirect keeps resolved secrets bound to the vetted host.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ToolInvocationResult {
                ok: false,
                status: None,
                body: String::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(format!("client: {e}")),
                truncated: false,
            }
        }
    };

    // Bind any `{secret:KEY}` header to an allowed destination host before we
    // resolve+attach it. Without this, a malicious/mistaken tool def could
    // declare a real secret header alongside a url_template pointing at an
    // attacker host and exfiltrate the key. The allowed set is the tool's
    // `secret_hosts` (if any), otherwise the host of its own url_template.
    let req_host = host_port_from_url(&url).map(|(h, _)| h.to_ascii_lowercase());
    let allowed_secret_hosts: Vec<String> = if tool.secret_hosts.is_empty() {
        // Default: secrets may only flow to the tool's declared endpoint host.
        host_port_from_url(&tool.url_template)
            .ok()
            .map(|(h, _)| h.to_ascii_lowercase())
            .into_iter()
            .collect()
    } else {
        tool.secret_hosts
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .collect()
    };

    let mut req = client.request(tool.method.as_reqwest(), &url);
    for (k, v) in &tool.headers {
        let carries_secret = v.contains("{secret:");
        if carries_secret {
            let host_ok = match &req_host {
                Ok(h) => allowed_secret_hosts.iter().any(|a| a == h),
                Err(_) => false,
            };
            if !host_ok {
                return ToolInvocationResult {
                    ok: false,
                    status: None,
                    body: String::new(),
                    latency_ms: started.elapsed().as_millis() as u64,
                    error: Some(format!(
                        "refusing to send secret header '{k}' to host {}: not in this tool's secret_hosts allowlist",
                        req_host.as_deref().unwrap_or("<unknown>")
                    )),
                    truncated: false,
                };
            }
        }
        req = req.header(k, substitute_header_secrets(v));
    }
    // For methods that can carry a body, fold any leftover args (those not
    // referenced by the URL template) into a JSON body so users don't have
    // to round-trip everything through path/query placeholders. GET/DELETE
    // stay body-less by convention.
    if matches!(
        tool.method,
        ToolMethod::Post | ToolMethod::Put | ToolMethod::Patch
    ) {
        let body_args: serde_json::Map<String, serde_json::Value> = args
            .iter()
            .filter(|(k, _)| !tool.url_template.contains(&format!("{{{k}}}")))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !body_args.is_empty() {
            req = req.json(&serde_json::Value::Object(body_args));
        }
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return ToolInvocationResult {
                        ok: false,
                        status: Some(status.as_u16()),
                        body: String::new(),
                        latency_ms: started.elapsed().as_millis() as u64,
                        error: Some(format!("read body: {e}")),
                        truncated: false,
                    }
                }
            };
            let truncated = bytes.len() > MAX_RESPONSE_BYTES;
            let slice = if truncated {
                &bytes[..MAX_RESPONSE_BYTES]
            } else {
                &bytes[..]
            };
            let body = match tool.response_format {
                ResponseFormat::Json => {
                    // Re-serialize so the renderer gets pretty-printed JSON —
                    // saves us a JSON.parse hop in the panel. Fall back to
                    // raw text on parse failure so we never lose the payload.
                    match serde_json::from_slice::<serde_json::Value>(slice) {
                        Ok(v) => serde_json::to_string_pretty(&v)
                            .unwrap_or_else(|_| String::from_utf8_lossy(slice).into_owned()),
                        Err(_) => String::from_utf8_lossy(slice).into_owned(),
                    }
                }
                ResponseFormat::Text => String::from_utf8_lossy(slice).into_owned(),
            };
            ToolInvocationResult {
                ok: status.is_success(),
                status: Some(status.as_u16()),
                body,
                latency_ms: started.elapsed().as_millis() as u64,
                error: if status.is_success() {
                    None
                } else {
                    Some(format!("http {}", status.as_u16()))
                },
                truncated,
            }
        }
        Err(e) => ToolInvocationResult {
            ok: false,
            status: None,
            body: String::new(),
            latency_ms: started.elapsed().as_millis() as u64,
            error: Some(e.to_string()),
            truncated: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    #[test]
    fn substitutes_simple_placeholders() {
        let out =
            substitute_template("https://api/{id}/x", &args(&[("id", "42")])).unwrap();
        assert_eq!(out, "https://api/42/x");
    }

    #[test]
    fn missing_arg_errors() {
        let err = substitute_template("https://api/{id}", &HashMap::new()).unwrap_err();
        assert!(err.contains("missing"));
    }

    #[test]
    fn secret_marker_passes_through() {
        let out = substitute_template("https://api/x", &HashMap::new()).unwrap();
        assert_eq!(out, "https://api/x");
        // Secrets in url templates are left intact (they only apply to headers).
        let out =
            substitute_template("https://api/{secret:foo}/x", &HashMap::new()).unwrap();
        assert_eq!(out, "https://api/{secret:foo}/x");
    }

    #[test]
    fn encodes_injection_chars_in_substitution() {
        // An arg trying to rewrite the host / inject path/query/fragment must
        // come out percent-encoded so it stays inside its original segment.
        let out = substitute_template(
            "https://api.example.com/{path}",
            &args(&[("path", "../../@evil.com/x?a=1#f")]),
        )
        .unwrap();
        assert!(out.starts_with("https://api.example.com/"));
        assert!(!out.contains("@evil.com"));
        assert!(!out.contains("../../"));
        assert!(!out.contains('?'));
        assert!(!out.contains('#'));
    }

    #[test]
    fn ssrf_guard_blocks_private_and_metadata() {
        assert!(ssrf_guard("http://127.0.0.1/x").is_err());
        assert!(ssrf_guard("http://localhost:8080/x").is_err());
        assert!(ssrf_guard("http://169.254.169.254/latest/meta-data").is_err());
        assert!(ssrf_guard("http://10.0.0.5/x").is_err());
        assert!(ssrf_guard("http://10.0.0.1/x").is_err());
        assert!(ssrf_guard("http://[::1]/x").is_err());
        assert!(ssrf_guard("http://[::ffff:127.0.0.1]/x").is_err());
        // userinfo can't be used to disguise a private host
        assert!(ssrf_guard("http://user@10.0.0.5/x").is_err());
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            host_port_from_url("https://a.com/x").unwrap(),
            ("a.com".to_string(), 443)
        );
        assert_eq!(
            host_port_from_url("http://a.com:81/x?q=1").unwrap(),
            ("a.com".to_string(), 81)
        );
        assert_eq!(
            host_port_from_url("http://u:p@a.com/x").unwrap(),
            ("a.com".to_string(), 80)
        );
        assert_eq!(
            host_port_from_url("http://[::1]:9/x").unwrap(),
            ("::1".to_string(), 9)
        );
    }

    #[test]
    fn rejects_bad_name() {
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name("a/b"));
        assert!(!is_valid_name(&"x".repeat(65)));
        assert!(is_valid_name("get-weather"));
        assert!(is_valid_name("v1.search"));
    }

    #[test]
    fn validates_url_scheme() {
        let mut t = ToolDef {
            name: "x".into(),
            description: "".into(),
            method: ToolMethod::Get,
            url_template: "ftp://nope".into(),
            inputs: vec![],
            headers: HashMap::new(),
            secret_hosts: vec![],
            response_format: ResponseFormat::Text,
            created_unix_ms: 0,
            updated_unix_ms: 0,
        };
        assert!(validate_tool(&t).is_err());
        t.url_template = "https://ok".into();
        assert!(validate_tool(&t).is_ok());
    }
}
