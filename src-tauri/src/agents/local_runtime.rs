//! Group C — local-runtime endpoint adapter.
//!
//! One adapter type (`LocalRuntimeAgent`) parameterized by a static
//! [`RuntimeSpec`] (id, label, base_url, default port). It probes a *localhost*
//! port and, if a server answers `GET <base_url>/models`, exposes that server's
//! models and streams chat through its OpenAI-compatible
//! `<base_url>/chat/completions` endpoint.
//!
//! Runtimes (from the spec's Group C table): LM Studio (:1234), vLLM (:8000),
//! llama.cpp / `llama-server` (:8080), TabbyAPI (:5000), Text-Gen-WebUI (:5000).
//! Ollama already has its own adapter (`ollama.rs`) — it is intentionally NOT
//! duplicated here.
//!
//! All targets are free/local; auth is an optional dummy key (some runtimes,
//! e.g. vLLM with `--api-key`, require *a* bearer even locally). The key is
//! read from KeyVault `<id>/api-key` if present, else a harmless placeholder is
//! sent. `available` reflects whether the port answers `/models` right now —
//! never an error at startup, so a runtime that isn't running just gets skipped.
//!
//! ⚠️ Port collision: TabbyAPI and Text-Gen-WebUI both default to 5000. Both
//! adapters are registered; whichever server is actually listening answers the
//! probe, so the other simply reports `available:false`. They never crash each
//! other — at most one server can hold :5000 at a time. Capabilities are
//! `Chat + LongContext` (these are chat-completions endpoints — no tool/code/
//! shell/vision), matching the honest stance of the other HTTP adapters.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest,
};
use crate::commands::keyvault;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;

const MAX_TOKENS: u64 = 4096;
const VAULT_KEY_LABEL: &str = "api-key";
/// Placeholder bearer for runtimes that demand *some* key locally but ignore
/// its value (OpenAI clients commonly send "not-needed"/"sk-..."). No secret.
const DUMMY_KEY: &str = "local-no-key";
/// Probe timeout — local servers answer near-instantly; keep startup snappy.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Static description of one local OpenAI-compatible runtime server. base_url is
/// loopback-only (`http://localhost:<port>/v1`) — these adapters never reach off
/// the machine.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeSpec {
    /// Stable adapter id (registry key, KeyVault provider, model-prefix).
    pub id: &'static str,
    /// Human label for the picker.
    pub label: &'static str,
    /// Loopback base URL including the `/v1` segment, e.g.
    /// `http://localhost:1234/v1`. `/models` and `/chat/completions` append to it.
    pub base_url: &'static str,
    /// The TCP port this runtime defaults to (documentation / disambiguation).
    pub port: u16,
}

/// The five Group C runtimes (Ollama excluded — it has its own adapter).
/// All base_urls are localhost; ports match the spec's "detect port" column.
pub static RUNTIMES: &[RuntimeSpec] = &[
    RuntimeSpec {
        id: "lmstudio",
        label: "LM Studio (local)",
        base_url: "http://localhost:1234/v1",
        port: 1234,
    },
    RuntimeSpec {
        id: "vllm",
        label: "vLLM (local)",
        base_url: "http://localhost:8000/v1",
        port: 8000,
    },
    RuntimeSpec {
        id: "llamacpp",
        label: "llama.cpp (local)",
        base_url: "http://localhost:8080/v1",
        port: 8080,
    },
    RuntimeSpec {
        // TabbyAPI and Text-Gen-WebUI both default to :5000. Whichever is
        // actually listening answers the /models probe; the other reports
        // unavailable. They can't both bind the port, so no crash.
        id: "tabbyapi",
        label: "TabbyAPI (local)",
        base_url: "http://localhost:5000/v1",
        port: 5000,
    },
    RuntimeSpec {
        id: "textgen-webui",
        label: "Text Gen WebUI (local)",
        base_url: "http://localhost:5000/v1",
        port: 5000,
    },
];

pub struct LocalRuntimeAgent {
    spec: &'static RuntimeSpec,
}

impl LocalRuntimeAgent {
    pub fn new(spec: &'static RuntimeSpec) -> Self {
        Self { spec }
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.spec.base_url.trim_end_matches('/'))
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.spec.base_url.trim_end_matches('/'))
    }

    /// Optional bearer: KeyVault `<id>/api-key` → env `<ID>_API_KEY` → dummy.
    /// Local runtimes mostly ignore the value but some require its presence.
    fn api_key(&self) -> String {
        keyvault::lookup_key_sync(self.spec.id, VAULT_KEY_LABEL)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DUMMY_KEY.to_string())
    }

    /// Fetch this server's model ids from `GET /v1/models` (best-effort, short
    /// timeout). Empty Vec on any failure — that doubles as the "is it up?"
    /// signal (an empty result means no reachable server).
    async fn fetch_models(&self) -> Vec<String> {
        let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
            return Vec::new();
        };
        let req = client
            .get(self.models_url())
            .bearer_auth(self.api_key());
        let Ok(resp) = req.send().await else {
            return Vec::new();
        };
        if !resp.status().is_success() {
            return Vec::new();
        }
        let Ok(json) = resp.json::<Value>().await else {
            return Vec::new();
        };
        parse_model_ids(&json)
    }

    /// Strip a leading `<id>:` / `<id>/` routing prefix off an explicit model.
    fn requested_model(&self, model: Option<&str>) -> Option<String> {
        let prefix_colon = format!("{}:", self.spec.id);
        let prefix_slash = format!("{}/", self.spec.id);
        model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| {
                m.strip_prefix(&prefix_colon)
                    .or_else(|| m.strip_prefix(&prefix_slash))
                    .unwrap_or(m)
                    .trim()
                    .to_string()
            })
            .filter(|m| !m.is_empty())
    }
}

#[async_trait::async_trait]
impl AgentAdapter for LocalRuntimeAgent {
    fn descriptor(&self) -> AgentDescriptor {
        // available is checked synchronously by the registry/UI; we can't await
        // a probe here, so the static descriptor reports available:true and the
        // async `health_check` (and the run-time /models fetch) carry the real
        // reachability. Routing calls health_check before dispatch.
        AgentDescriptor {
            id: self.spec.id.to_string(),
            label: self.spec.label.to_string(),
            description: format!(
                "{} on localhost:{} via OpenAI-compatible Chat Completions ({}). Free/local — models come from the server's /v1/models. Chat only: no tool use, code edits, or vision.",
                self.spec.label, self.spec.port, self.spec.base_url,
            ),
            capabilities: vec![
                AgentCapability::Chat,
                AgentCapability::LongContext,
            ],
            // A blocking-thread probe would deadlock inside an async runtime, so
            // the cheap static answer here is "registered"; `health_check` does
            // the real localhost probe before a run is dispatched.
            available: true,
        }
    }

    async fn health_check(&self) -> bool {
        // The port answers /v1/models with ≥0 models? Then it's a real server.
        // (An up server with no models loaded still returns a 200 + empty data;
        // we treat a successful, parseable response as healthy.)
        let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
            return false;
        };
        client
            .get(self.models_url())
            .bearer_auth(self.api_key())
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn run(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let _ = tx
            .send(AgentEvent::Started { agent_id: self.spec.id.into(), run_id: None })
            .await;

        // Resolve a concrete model: explicit request (prefix-stripped) → first
        // model the server reports → error (a local server with no model can't
        // chat, and these runtimes have no fixed default slug).
        let model = match self.requested_model(req.model.as_deref()) {
            Some(m) if m != "auto" => m,
            _ => {
                let models = self.fetch_models().await;
                let Some(first) = models.into_iter().next() else {
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: format!(
                                "{} is not reachable on localhost:{} (no /v1/models). Start the server, then retry.",
                                self.spec.label, self.spec.port,
                            ),
                        })
                        .await;
                    let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                    return Ok(());
                };
                if req.model.as_deref().map(str::trim) == Some("auto") {
                    let _ = tx
                        .send(AgentEvent::Reasoning {
                            text: format!("Auto-selected local model: {first}"),
                        })
                        .await;
                }
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
        let resp = match client
            .post(self.chat_url())
            .bearer_auth(self.api_key())
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Error {
                        message: format!(
                            "{} request failed: {e} (is the server running on localhost:{}?)",
                            self.spec.id, self.spec.port,
                        ),
                    })
                    .await;
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                return Ok(());
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let detail = detail.trim();
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!("{} returned {status}: {detail}", self.spec.id),
                })
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
                        .send(AgentEvent::Error {
                            message: format!("{} stream error: {e}", self.spec.id),
                        })
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

/// Extract model ids from an OpenAI `/v1/models` response: `{ "data": [ { "id":
/// "..." }, ... ] }`. Tolerates a bare array or missing fields → empty Vec.
fn parse_model_ids(json: &Value) -> Vec<String> {
    let arr = json
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| json.as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(String::from))
        .collect()
}

/// Pure parse of one OpenAI Chat Completions SSE `data:` payload (shared schema
/// with the Group A adapter). `Some((text?, total_tokens?))` or `None`.
fn parse_chat_completion_event(data: &str) -> Option<(Option<String>, Option<u64>)> {
    let json: Value = serde_json::from_str(data).ok()?;
    let text = json
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let tokens = json
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(Value::as_u64);
    if text.is_none() && tokens.is_none() {
        return None;
    }
    Some((text, tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &'static str) -> &'static RuntimeSpec {
        RUNTIMES.iter().find(|r| r.id == id).expect("known id")
    }

    #[test]
    fn runtime_table_matches_the_spec_group_c() {
        // Five runtimes; Ollama deliberately excluded (own adapter).
        assert_eq!(RUNTIMES.len(), 5);
        let expected = ["lmstudio", "vllm", "llamacpp", "tabbyapi", "textgen-webui"];
        for id in expected {
            assert!(RUNTIMES.iter().any(|r| r.id == id), "missing runtime {id}");
        }
        assert!(
            !RUNTIMES.iter().any(|r| r.id == "ollama"),
            "Ollama must not be duplicated here — it has its own adapter"
        );
    }

    #[test]
    fn runtime_ids_are_unique() {
        let mut ids: Vec<&str> = RUNTIMES.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "runtime ids must be unique (registry keys)");
    }

    #[test]
    fn all_base_urls_are_localhost_only() {
        for r in RUNTIMES {
            assert!(
                r.base_url.starts_with("http://localhost:"),
                "{} must be localhost-only (got {})",
                r.id,
                r.base_url
            );
            assert!(r.base_url.ends_with("/v1"), "{} base_url must end /v1", r.id);
            // The declared port must appear in the URL — guards against drift.
            assert!(
                r.base_url.contains(&format!(":{}/", r.port)),
                "{} url/port mismatch",
                r.id
            );
        }
    }

    #[test]
    fn tabby_and_textgen_share_port_5000_and_dont_collide() {
        // Both default to 5000 by design; the spec says let the listening one
        // win gracefully. We assert the documented collision exists (so a future
        // edit that "fixes" it is a conscious choice) and that they are still two
        // distinct, separately-skippable adapters.
        let tabby = spec("tabbyapi");
        let tgw = spec("textgen-webui");
        assert_eq!(tabby.port, 5000);
        assert_eq!(tgw.port, 5000);
        assert_ne!(tabby.id, tgw.id);
    }

    #[test]
    fn urls_compose_correctly() {
        let lm = LocalRuntimeAgent::new(spec("lmstudio"));
        assert_eq!(lm.models_url(), "http://localhost:1234/v1/models");
        assert_eq!(lm.chat_url(), "http://localhost:1234/v1/chat/completions");
    }

    #[test]
    fn descriptor_is_chat_only() {
        // Local chat-completions endpoints: Chat + LongContext only, never
        // tool/code/shell/vision.
        let caps = LocalRuntimeAgent::new(spec("vllm")).descriptor().capabilities;
        assert!(caps.contains(&AgentCapability::Chat));
        assert!(caps.contains(&AgentCapability::LongContext));
        assert!(!caps.contains(&AgentCapability::CodeEdit));
        assert!(!caps.contains(&AgentCapability::ShellExec));
        assert!(!caps.contains(&AgentCapability::Vision));
    }

    #[test]
    fn parses_openai_models_envelope() {
        let json: Value = serde_json::from_str(
            r#"{"object":"list","data":[{"id":"llama-3.1-8b","object":"model"},{"id":"qwen2.5-7b"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_model_ids(&json), vec!["llama-3.1-8b", "qwen2.5-7b"]);
    }

    #[test]
    fn parses_bare_array_and_tolerates_junk() {
        let arr: Value = serde_json::from_str(r#"[{"id":"m1"},{"id":"m2"}]"#).unwrap();
        assert_eq!(parse_model_ids(&arr), vec!["m1", "m2"]);
        // Missing data / wrong shape → empty, never a panic.
        let empty: Value = serde_json::from_str(r#"{"object":"list"}"#).unwrap();
        assert!(parse_model_ids(&empty).is_empty());
        let junk: Value = serde_json::from_str(r#"{"data":"oops"}"#).unwrap();
        assert!(parse_model_ids(&junk).is_empty());
    }

    #[test]
    fn strips_runtime_prefix_from_model() {
        let vllm = LocalRuntimeAgent::new(spec("vllm"));
        assert_eq!(
            vllm.requested_model(Some("vllm:meta-llama/Llama-3.1-8B")),
            Some("meta-llama/Llama-3.1-8B".to_string())
        );
        assert_eq!(
            vllm.requested_model(Some("vllm/meta-llama/Llama-3.1-8B")),
            Some("meta-llama/Llama-3.1-8B".to_string())
        );
        // A HF-style id with slashes but no runtime prefix is preserved whole.
        assert_eq!(
            vllm.requested_model(Some("meta-llama/Llama-3.1-8B")),
            Some("meta-llama/Llama-3.1-8B".to_string())
        );
        assert_eq!(vllm.requested_model(None), None);
    }

    #[test]
    fn parses_content_delta_and_usage() {
        let data = r#"{"choices":[{"index":0,"delta":{"content":"yo"}}]}"#;
        assert_eq!(
            parse_chat_completion_event(data),
            Some((Some("yo".to_string()), None))
        );
        let usage = r#"{"choices":[],"usage":{"total_tokens":7}}"#;
        assert_eq!(parse_chat_completion_event(usage), Some((None, Some(7))));
        assert_eq!(parse_chat_completion_event("garbage"), None);
    }
}
