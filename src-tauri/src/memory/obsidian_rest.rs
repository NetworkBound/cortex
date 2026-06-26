//! Obsidian Local REST API client (coddingtonbear/obsidian-local-rest-api).
//!
//! When the user runs the plugin, Cortex prefers it for live,
//! Obsidian-indexed search + active-note context — strictly better than the
//! filesystem walk (real search, current index, active note). It auto-discovers
//! the API key from the plugin's `data.json` (like the vault is discovered from
//! `obsidian.json`), so there's nothing to configure. When Obsidian isn't
//! running, `discover`/`status` return None/false and callers fall back to the
//! filesystem path — the integration never hard-fails.

use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

pub struct RestClient {
    base: String,
    key: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct RestHit {
    pub path: String,
    pub snippet: String,
}

#[derive(Deserialize)]
struct SimpleSearchRow {
    #[serde(default)]
    filename: String,
}

/// Read the plugin's `data.json` and derive the API base URL + key. Prefers the
/// insecure HTTP port when the plugin has it enabled (no cert hassle for a
/// localhost client), else the HTTPS port.
pub fn discover(vault: &Path) -> Option<(String, String)> {
    let data = std::fs::read_to_string(
        vault.join(".obsidian/plugins/obsidian-local-rest-api/data.json"),
    )
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let key = v.get("apiKey")?.as_str()?.to_string();
    if key.is_empty() {
        return None;
    }
    let insecure = v
        .get("enableInsecureServer")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let (scheme, port) = if insecure {
        ("http", v.get("insecurePort").and_then(|p| p.as_u64()).unwrap_or(27123))
    } else {
        ("https", v.get("port").and_then(|p| p.as_u64()).unwrap_or(27124))
    };
    Some((format!("{scheme}://127.0.0.1:{port}"), key))
}

/// Percent-encode a vault-relative path for safe interpolation into the REST
/// API URL. Path separators (`/`) are preserved so nested notes still resolve,
/// but `.`/`..` traversal segments are dropped and every other byte that isn't
/// an unreserved URL character is escaped. This prevents a server-returned
/// filename from traversing outside `/vault/` or injecting `?`/`#`/CR-LF and
/// thereby smuggling additional bits into the request.
fn encode_vault_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut first = true;
    for segment in path.split('/') {
        // Skip empty segments and `.`/`..` so traversal can't escape `/vault/`.
        if segment.is_empty() || segment == "." || segment == ".." {
            continue;
        }
        if !first {
            out.push('/');
        }
        first = false;
        for &byte in segment.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char);
                }
                _ => {
                    out.push('%');
                    out.push(char::from_digit((byte >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                    out.push(char::from_digit((byte & 0xF) as u32, 16).unwrap().to_ascii_uppercase());
                }
            }
        }
    }
    out
}

impl RestClient {
    /// Build a client from the plugin's discovered config. Returns None when the
    /// plugin isn't installed/configured (no liveness check yet — see `status`).
    pub fn from_vault(vault: &Path) -> Option<Self> {
        let (base, key) = discover(vault)?;
        // Accept the plugin's self-signed cert (localhost only).
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        Some(Self { base, key, http })
    }

    /// True when Obsidian is running and serving the API.
    pub async fn status(&self) -> bool {
        self.http
            .get(format!("{}/", self.base))
            .bearer_auth(&self.key)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Full-text search via the plugin's `/search/simple/` (Obsidian-indexed).
    pub async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<RestHit>> {
        let url = format!("{}/search/simple/", self.base);
        let rows = self
            .http
            .post(&url)
            .bearer_auth(&self.key)
            .query(&[("query", query), ("contextLength", "200")])
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<SimpleSearchRow>>()
            .await?;
        Ok(rows
            .into_iter()
            .take(limit)
            .map(|r| RestHit { path: r.filename, snippet: String::new() })
            .collect())
    }

    /// Read a vault file's raw markdown (used to build embedding snippets).
    pub async fn read_note(&self, path: &str) -> anyhow::Result<String> {
        // `path` originates from server-returned search results and is
        // interpolated into the request URL. Percent-encode each segment so a
        // crafted filename can't traverse out of `/vault/` or smuggle extra
        // request bits (`..`, `?`, `#`, control chars) into the REST API.
        let url = format!("{}/vault/{}", self.base, encode_vault_path(path));
        Ok(self
            .http
            .get(&url)
            .bearer_auth(&self.key)
            .header("Accept", "text/markdown")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }
}
