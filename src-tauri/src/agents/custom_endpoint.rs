//! Homelab Model Fabric — user-defined OpenAI-compatible endpoints.
//!
//! Unlike [`super::local_runtime`] (a fixed table of *localhost* runtimes), this
//! adapter is parameterized by an *owned*, user-supplied [`EndpointCfg`] loaded
//! from `~/.cortex/endpoints.json`, so a homelab box on a LAN IP or tailnet node
//! becomes a first-class adapter (`fabric-<slug>`). It reuses the two pure
//! OpenAI parsers from `local_runtime` (`parse_model_ids`,
//! `parse_chat_completion_event`) — no protocol duplication.
//!
//! ## Security posture
//! - **Probes never send the vault key.** [`probe`] (health / model discovery /
//!   the Settings "Test" button) issues an *unauthenticated* `GET /models`, so a
//!   hostile or mistyped endpoint URL can never harvest a stored API key. The
//!   key is only ever attached to a user-initiated **chat** (`run`), where the
//!   user has explicitly chosen to send their prompt to that endpoint.
//! - **Chat-only capabilities.** Like the other HTTP adapters, a fabric endpoint
//!   advertises `Chat + LongContext` only — never tool/code/shell/vision — so it
//!   preserves the invariant that chat-only adapters never receive tool work.
//! - Endpoint ids are `fabric-` prefixed, which can never collide with a
//!   built-in adapter id.

use super::adapter::{AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest};
use super::local_runtime::{parse_chat_completion_event, parse_model_ids};
use crate::commands::keyvault;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const MAX_TOKENS: u64 = 4096;
const VAULT_KEY_LABEL: &str = "api-key";
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// Registry-id prefix — guarantees no collision with a built-in adapter id.
pub const FABRIC_PREFIX: &str = "fabric-";

/// A user-defined OpenAI-compatible endpoint. Persisted to
/// `~/.cortex/endpoints.json`. The API key is NOT stored here — it lives in the
/// OS KeyVault under `<id>/api-key`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointCfg {
    /// Registry id, always `fabric-<slug>` (normalized at save time).
    pub id: String,
    /// Human label for the picker/Settings.
    pub label: String,
    /// Base URL including the OpenAI `/v1` segment, e.g.
    /// `http://192.168.1.50:8000/v1`. `/models` and `/chat/completions` append.
    pub base_url: String,
    /// `"local"` (LAN/tailnet/localhost homelab box) or `"remote"` (a hosted
    /// OpenAI-compatible API). Display + future local-first routing only.
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_kind() -> String {
    "local".to_string()
}
fn default_true() -> bool {
    true
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cortex").join("endpoints.json"))
}

/// Load persisted endpoints. Missing/malformed file ⇒ empty list (today's
/// behavior), never an error — Model Fabric is purely additive.
pub fn load_endpoints() -> Vec<EndpointCfg> {
    let Some(path) = config_path() else { return Vec::new() };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<EndpointCfg>>(&raw).ok())
        .unwrap_or_default()
}

/// Persist endpoints to `~/.cortex/endpoints.json`.
pub fn save_endpoints(endpoints: &[EndpointCfg]) -> anyhow::Result<()> {
    let path = config_path().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(endpoints)?)?;
    Ok(())
}

/// Slugify a user-supplied id/label into a stable `fabric-<slug>` registry id.
/// Lowercase, `[a-z0-9-]` only, collapse/trim dashes. Empty ⇒ `None`.
pub fn normalize_id(raw: &str) -> Option<String> {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in raw.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return None;
    }
    // Idempotent: don't double-prefix if the caller already passed fabric-<slug>.
    Some(if let Some(rest) = slug.strip_prefix("fabric-") {
        format!("{FABRIC_PREFIX}{rest}")
    } else {
        format!("{FABRIC_PREFIX}{slug}")
    })
}

/// Result of an unauthenticated endpoint probe.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub latency_ms: Option<i64>,
    pub models: Vec<String>,
    pub error: Option<String>,
}

fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// Probe an endpoint's `GET /models` — UNAUTHENTICATED (never attaches the vault
/// key; see the module security note). Returns reachability, latency, and any
/// discovered model ids. Never errors out of band; failures land in `error`.
pub async fn probe(base_url: &str) -> ProbeResult {
    let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
        return ProbeResult { ok: false, latency_ms: None, models: vec![], error: Some("client build failed".into()) };
    };
    let started = Instant::now();
    // No bearer_auth here — intentional SSRF/key-harvest guard.
    match client.get(models_url(base_url)).send().await {
        Ok(resp) => {
            let latency = started.elapsed().as_millis() as i64;
            let status = resp.status();
            if !status.is_success() {
                // Reachable but not a 2xx (often 401 for a key-protected
                // endpoint — expected, since we probe without a key).
                return ProbeResult {
                    ok: false,
                    latency_ms: Some(latency),
                    models: vec![],
                    error: Some(format!("HTTP {status} (reachable; models discover on first chat if auth is required)")),
                };
            }
            let models = resp.json::<Value>().await.map(|j| parse_model_ids(&j)).unwrap_or_default();
            ProbeResult { ok: true, latency_ms: Some(latency), models, error: None }
        }
        Err(e) => ProbeResult {
            ok: false,
            latency_ms: None,
            models: vec![],
            error: Some(format!("unreachable: {e}")),
        },
    }
}

pub struct CustomEndpointAgent {
    cfg: EndpointCfg,
}

impl CustomEndpointAgent {
    pub fn new(cfg: EndpointCfg) -> Self {
        Self { cfg }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'))
    }

    /// Real API key for the CHAT path only (KeyVault `<id>/api-key`), if the user
    /// stored one. `None` ⇒ an open endpoint, no Authorization header sent.
    fn chat_api_key(&self) -> Option<String> {
        keyvault::lookup_key_sync(&self.cfg.id, VAULT_KEY_LABEL)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Strip a leading `<id>:` / `<id>/` routing prefix off an explicit model.
    fn requested_model(&self, model: Option<&str>) -> Option<String> {
        let colon = format!("{}:", self.cfg.id);
        let slash = format!("{}/", self.cfg.id);
        model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| m.strip_prefix(&colon).or_else(|| m.strip_prefix(&slash)).unwrap_or(m).trim().to_string())
            .filter(|m| !m.is_empty())
    }
}

#[async_trait::async_trait]
impl AgentAdapter for CustomEndpointAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: self.cfg.id.clone(),
            label: self.cfg.label.clone(),
            description: format!(
                "Model Fabric endpoint ({}) at {} via OpenAI-compatible Chat Completions. Models come from the server's /v1/models. Chat only: no tool use, code edits, or vision.",
                self.cfg.kind, self.cfg.base_url,
            ),
            capabilities: vec![AgentCapability::Chat, AgentCapability::LongContext],
            available: self.cfg.enabled,
        }
    }

    async fn health_check(&self) -> bool {
        // Unauthenticated probe — same key-harvest guard as `probe`.
        self.cfg.enabled && probe(&self.cfg.base_url).await.ok
    }

    async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        let _ = tx
            .send(AgentEvent::Started { agent_id: self.cfg.id.clone(), run_id: None })
            .await;

        // Resolve a concrete model: explicit (prefix-stripped) → first discovered.
        let model = match self.requested_model(req.model.as_deref()) {
            Some(m) if m != "auto" => m,
            _ => {
                let discovered = probe(&self.cfg.base_url).await.models;
                let Some(first) = discovered.into_iter().next() else {
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: format!(
                                "{} is not reachable at {} (no /v1/models). Check the endpoint, then retry.",
                                self.cfg.label, self.cfg.base_url,
                            ),
                        })
                        .await;
                    let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                    return Ok(());
                };
                first
            }
        };

        let mut messages: Vec<Value> = req
            .history
            .iter()
            .map(|t| json!({ "role": t.role, "content": t.content }))
            .collect();
        messages.push(json!({ "role": "user", "content": req.message }));
        let body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "max_tokens": MAX_TOKENS,
            "stream_options": { "include_usage": true },
        });

        let client = reqwest::Client::new();
        // Attach the key ONLY on the chat path, and only if the user stored one.
        let mut builder = client
            .post(self.chat_url())
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = self.chat_api_key() {
            builder = builder.bearer_auth(key);
        }
        let resp = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Error { message: format!("{} request failed: {e}", self.cfg.id) })
                    .await;
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                return Ok(());
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(AgentEvent::Error { message: format!("{} returned {status}: {}", self.cfg.id, detail.trim()) })
                .await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        }

        let mut stream = resp.bytes_stream().eventsource();
        let mut total_tokens: Option<u64> = None;
        while let Some(event) = stream.next().await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx
                        .send(AgentEvent::Error { message: format!("{} stream error: {e}", self.cfg.id) })
                        .await;
                    break;
                }
            };
            if event.data == "[DONE]" {
                break;
            }
            if let Some((delta, tokens)) = parse_chat_completion_event(&event.data) {
                if let Some(text) = delta {
                    if !text.is_empty() {
                        let _ = tx.send(AgentEvent::Token { delta: text }).await;
                    }
                }
                if let Some(t) = tokens {
                    total_tokens = Some(t);
                }
            }
        }
        let _ = tx.send(AgentEvent::Done { total_tokens, run_id: None }).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_id_slugs_and_prefixes() {
        assert_eq!(normalize_id("My GPU Box!").as_deref(), Some("fabric-my-gpu-box"));
        assert_eq!(normalize_id("fabric-vllm").as_deref(), Some("fabric-vllm"));
        assert_eq!(normalize_id("  ").as_deref(), None);
        assert_eq!(normalize_id("...").as_deref(), None);
        // Prefix guarantees no collision with a built-in adapter id.
        assert!(normalize_id("gateway-remote").unwrap().starts_with(FABRIC_PREFIX));
    }

    #[test]
    fn descriptor_is_chat_only_and_reflects_enabled() {
        let agent = CustomEndpointAgent::new(EndpointCfg {
            id: "fabric-gpu".into(),
            label: "GPU box".into(),
            base_url: "http://192.168.1.50:8000/v1".into(),
            kind: "local".into(),
            enabled: false,
        });
        let d = agent.descriptor();
        assert_eq!(d.id, "fabric-gpu");
        assert!(!d.available, "disabled endpoint reports unavailable");
        assert!(d.capabilities.contains(&AgentCapability::Chat));
        // Never code-edit/shell/vision — preserves the routing safety invariant.
        assert!(!d.capabilities.iter().any(|c| matches!(
            c,
            AgentCapability::CodeEdit | AgentCapability::ShellExec | AgentCapability::Vision
        )));
    }

    #[test]
    fn requested_model_strips_the_fabric_prefix() {
        let agent = CustomEndpointAgent::new(EndpointCfg {
            id: "fabric-gpu".into(),
            label: "GPU".into(),
            base_url: "http://h/v1".into(),
            kind: "local".into(),
            enabled: true,
        });
        assert_eq!(agent.requested_model(Some("fabric-gpu:llama-3")).as_deref(), Some("llama-3"));
        assert_eq!(agent.requested_model(Some("fabric-gpu/llama-3")).as_deref(), Some("llama-3"));
        assert_eq!(agent.requested_model(Some("llama-3")).as_deref(), Some("llama-3"));
        assert_eq!(agent.requested_model(None), None);
    }
}
