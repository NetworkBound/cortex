//! Group A — one generic OpenAI-compatible API adapter.
//!
//! A single adapter type (`OpenAiCompatAgent`) parameterized by a static
//! [`ProviderSpec`] (id, label, base_url, api_key_env, default_model). It POSTs
//! the OpenAI Chat Completions shape (`<base_url>/chat/completions`, streaming
//! SSE) to whichever per-token provider the spec names, reading the bearer key
//! from the user's KeyVault (preferred) or the provider's documented env var.
//!
//! This is the deliberate sibling of `anthropic_direct.rs` / `openai_direct.rs`:
//! **chat-only**. These are plain Chat Completions endpoints — we do NOT send
//! `tools`, so there is no tool use / code-edit / shell-exec, and no vision.
//! Capabilities are therefore `Chat + LongContext` only, and the descriptor says
//! so. An adapter whose key is absent reports `available:false` so routing /
//! the picker skip it — it never errors at startup.
//!
//! The 13 providers live in [`PROVIDERS`], lifted verbatim from the spec's
//! "Group A" table. Each base_url already includes the version segment, so the
//! adapter just appends `/chat/completions` (trailing slash normalized).
//!
//! Key resolution per run (re-read every time, so a Settings change takes effect
//! without a restart): KeyVault `<id>/api-key` → env `<api_key_env>`. Model
//! resolution: explicit `req.model` (verbatim, stripped of a leading `<id>:` /
//! `<id>/` prefix) → KeyVault `<id>/default-model` → spec `default_model`.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest,
};
use crate::commands::keyvault;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const MAX_TOKENS: u64 = 4096;
/// KeyVault label for a provider's bearer key (provider == spec id).
const VAULT_KEY_LABEL: &str = "api-key";
/// Optional KeyVault label overriding a provider's default model.
const VAULT_MODEL_LABEL: &str = "default-model";

/// Static description of one OpenAI-compatible, per-token provider. All fields
/// are `&'static str` so the table is `const`-constructible and zero-alloc.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    /// Stable adapter id (registry key, KeyVault provider, model-prefix).
    pub id: &'static str,
    /// Human label for the picker.
    pub label: &'static str,
    /// Base URL up to and including the version segment, e.g.
    /// `https://api.groq.com/openai/v1`. `/chat/completions` is appended.
    pub base_url: &'static str,
    /// Documented env var carrying the bearer key (fallback when no vault entry).
    pub api_key_env: &'static str,
    /// Model id used when the request names none and no vault override is set.
    /// Kept on a current, doc-stable slug per the spec's "fetch /v1/models at
    /// runtime" caveat — a wrong slug surfaces as the provider's own 404.
    pub default_model: &'static str,
}

/// The 13 Group A providers, verbatim from `provider-integration-spec.md`.
/// base_urls already carry their version path; default models are the
/// spec-flagged stable ids where given, else a current widely-served slug.
pub static PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "groq",
        label: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        api_key_env: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
    },
    ProviderSpec {
        id: "together",
        label: "Together AI",
        base_url: "https://api.together.xyz/v1",
        api_key_env: "TOGETHER_API_KEY",
        default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    },
    ProviderSpec {
        id: "fireworks",
        label: "Fireworks",
        base_url: "https://api.fireworks.ai/inference/v1",
        api_key_env: "FIREWORKS_API_KEY",
        default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct",
    },
    ProviderSpec {
        id: "deepseek",
        label: "DeepSeek",
        base_url: "https://api.deepseek.com",
        api_key_env: "DEEPSEEK_API_KEY",
        default_model: "deepseek-chat",
    },
    ProviderSpec {
        id: "mistral",
        label: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        api_key_env: "MISTRAL_API_KEY",
        default_model: "mistral-large-latest",
    },
    ProviderSpec {
        id: "xai",
        label: "xAI Grok",
        base_url: "https://api.x.ai/v1",
        api_key_env: "XAI_API_KEY",
        default_model: "grok-4.3",
    },
    ProviderSpec {
        id: "perplexity",
        label: "Perplexity",
        base_url: "https://api.perplexity.ai",
        api_key_env: "PPLX_API_KEY",
        default_model: "sonar",
    },
    ProviderSpec {
        id: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        api_key_env: "OPENROUTER_API_KEY",
        default_model: "openrouter/auto",
    },
    ProviderSpec {
        id: "dashscope",
        label: "Qwen / DashScope",
        base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        api_key_env: "DASHSCOPE_API_KEY",
        default_model: "qwen-max",
    },
    ProviderSpec {
        id: "moonshot",
        label: "Moonshot Kimi",
        base_url: "https://api.moonshot.ai/v1",
        api_key_env: "MOONSHOT_API_KEY",
        default_model: "kimi-k2.6",
    },
    ProviderSpec {
        id: "cohere",
        label: "Cohere",
        base_url: "https://api.cohere.ai/compatibility/v1",
        api_key_env: "COHERE_API_KEY",
        default_model: "command-r-plus",
    },
    ProviderSpec {
        id: "gemini-api",
        label: "Google Gemini (API)",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        api_key_env: "GEMINI_API_KEY",
        default_model: "gemini-2.5-flash",
    },
    ProviderSpec {
        id: "llama-api",
        label: "Meta Llama API",
        base_url: "https://api.llama.com/compat/v1",
        api_key_env: "LLAMA_API_KEY",
        default_model: "Llama-4-Maverick-17B-128E-Instruct-FP8",
    },
];

pub struct OpenAiCompatAgent {
    spec: &'static ProviderSpec,
}

impl OpenAiCompatAgent {
    pub fn new(spec: &'static ProviderSpec) -> Self {
        Self { spec }
    }

    /// `<base_url>/chat/completions`, tolerating a trailing slash on base_url.
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.spec.base_url.trim_end_matches('/'))
    }

    /// Bearer key for this provider: KeyVault `<id>/api-key` first (Settings →
    /// Providers), then the documented env var. Trimmed; empty → `None`.
    fn api_key(&self) -> Option<String> {
        keyvault::lookup_key_sync(self.spec.id, VAULT_KEY_LABEL)
            .ok()
            .or_else(|| std::env::var(self.spec.api_key_env).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Trimmed explicit model off the request, stripped of a leading `<id>:` or
    /// `<id>/` routing prefix (so `groq:llama-3.3-70b` → `llama-3.3-70b`).
    /// `openrouter/...` ids legitimately contain a slash, so we only strip the
    /// `<id>/` prefix, never the first slash blindly. Sent verbatim otherwise —
    /// a bad slug surfaces as the provider's own 404.
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

    /// Default model: KeyVault `<id>/default-model` → spec `default_model`.
    fn default_model(&self) -> String {
        keyvault::lookup_key_sync(self.spec.id, VAULT_MODEL_LABEL)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.spec.default_model.to_string())
    }

    fn resolve_model(&self, req: &ChatRequest) -> String {
        self.requested_model(req.model.as_deref())
            .unwrap_or_else(|| self.default_model())
    }
}

#[async_trait::async_trait]
impl AgentAdapter for OpenAiCompatAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: self.spec.id.to_string(),
            label: self.spec.label.to_string(),
            // Honest about the path: a plain Chat Completions stream (system
            // message + history as messages). No `tools` are sent, so there is
            // no tool use / code-edit / shell-exec, and content is text-only so
            // no vision. Mirrors the deliberately chat-only direct adapters.
            description: format!(
                "{} via OpenAI-compatible Chat Completions ({}). Streams chat using your stored API key (KeyVault {}/api-key or ${}). Chat only — tool use, code edits, and vision are not available on this endpoint.",
                self.spec.label, self.spec.base_url, self.spec.id, self.spec.api_key_env,
            ),
            capabilities: vec![
                AgentCapability::Chat,
                AgentCapability::LongContext,
            ],
            // Absent key → unavailable, so routing skips it. Never an error.
            available: self.api_key().is_some(),
        }
    }

    async fn health_check(&self) -> bool {
        // Presence of a key is the readiness signal — no token-burning probe.
        self.api_key().is_some()
    }

    async fn run(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let _ = tx
            .send(AgentEvent::Started { agent_id: self.spec.id.into(), run_id: None })
            .await;

        let Some(api_key) = self.api_key() else {
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!(
                        "No API key for {}. Add one in Settings → Providers (vault {}/api-key) or set ${}.",
                        self.spec.label, self.spec.id, self.spec.api_key_env,
                    ),
                })
                .await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        };

        let model = self.resolve_model(&req);

        // Chat Completions takes the system prompt as a `role:"system"` entry
        // inline, so any "system" history turn passes through verbatim.
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
            .post(self.endpoint())
            .bearer_auth(&api_key)
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
                            "{} request failed: {e} (check your network / base URL)",
                            self.spec.id
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
            let hint = if status.as_u16() == 401 {
                " — API key invalid. Re-enter it in Settings → Providers."
            } else if status.as_u16() == 404 {
                " — model not recognized. Pick a current model id (the provider's /v1/models lists them), or set a vault default-model."
            } else {
                ""
            };
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!("{} returned {status}: {detail}{hint}", self.spec.id),
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

/// Pure parse of one OpenAI Chat Completions SSE `data:` payload. Returns
/// `Some((maybe_text_delta, maybe_total_tokens))` for chunks we care about and
/// `None` for noise / unparseable lines. Identical schema to `openai_direct`'s
/// parser — kept local so this tier is self-contained and unit-testable.
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

    fn spec(id: &'static str) -> &'static ProviderSpec {
        PROVIDERS.iter().find(|p| p.id == id).expect("known id")
    }

    #[test]
    fn provider_table_is_the_thirteen_from_the_spec() {
        assert_eq!(PROVIDERS.len(), 13, "Group A spec table has 13 providers");
        let expected = [
            "groq", "together", "fireworks", "deepseek", "mistral", "xai",
            "perplexity", "openrouter", "dashscope", "moonshot", "cohere",
            "gemini-api", "llama-api",
        ];
        for id in expected {
            assert!(PROVIDERS.iter().any(|p| p.id == id), "missing provider {id}");
        }
    }

    #[test]
    fn provider_ids_are_unique() {
        let mut ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "provider ids must be unique (registry keys)");
    }

    #[test]
    fn every_spec_is_well_formed() {
        for p in PROVIDERS {
            assert!(p.base_url.starts_with("https://"), "{} base_url must be https", p.id);
            assert!(!p.label.is_empty(), "{} needs a label", p.id);
            assert!(!p.api_key_env.is_empty(), "{} needs an env var", p.id);
            assert!(!p.default_model.is_empty(), "{} needs a default model", p.id);
            // No localhost / homelab leakage in a hosted-API table.
            assert!(!p.base_url.contains("localhost"), "{} must not be localhost", p.id);
            assert!(!p.base_url.contains("127.0.0.1"), "{} must not be loopback", p.id);
            assert!(!p.base_url.contains("192.168."), "{} must not embed a LAN IP", p.id);
        }
    }

    #[test]
    fn endpoint_appends_chat_completions_once() {
        let groq = OpenAiCompatAgent::new(spec("groq"));
        assert_eq!(groq.endpoint(), "https://api.groq.com/openai/v1/chat/completions");
        // base_url with no trailing slash already; deepseek has no /v1 segment.
        let ds = OpenAiCompatAgent::new(spec("deepseek"));
        assert_eq!(ds.endpoint(), "https://api.deepseek.com/chat/completions");
    }

    #[test]
    fn unavailable_when_no_key() {
        // No vault entry and no env var for this provider in the test env, so
        // the descriptor must report available:false (routing skips it) rather
        // than erroring. Guard against an env var leaking in from the host.
        let p = spec("fireworks");
        if std::env::var(p.api_key_env).is_err() {
            let agent = OpenAiCompatAgent::new(p);
            assert!(!agent.descriptor().available, "no key → unavailable");
        }
    }

    #[test]
    fn descriptor_is_chat_only() {
        // Truthful-flags tripwire: these are plain Chat Completions endpoints.
        // They must advertise ONLY Chat + LongContext — never tool/code/shell/
        // vision capabilities the HTTP path does not implement.
        let caps = OpenAiCompatAgent::new(spec("groq")).descriptor().capabilities;
        assert!(caps.contains(&AgentCapability::Chat));
        assert!(caps.contains(&AgentCapability::LongContext));
        assert!(!caps.contains(&AgentCapability::CodeEdit));
        assert!(!caps.contains(&AgentCapability::ShellExec));
        assert!(!caps.contains(&AgentCapability::Vision));
        assert!(!caps.contains(&AgentCapability::WebSearch));
    }

    #[test]
    fn strips_provider_prefix_from_model() {
        let groq = OpenAiCompatAgent::new(spec("groq"));
        assert_eq!(
            groq.requested_model(Some("groq:llama-3.3-70b-versatile")),
            Some("llama-3.3-70b-versatile".to_string())
        );
        assert_eq!(
            groq.requested_model(Some("groq/llama-3.3-70b-versatile")),
            Some("llama-3.3-70b-versatile".to_string())
        );
        // A bare slug passes through untouched.
        assert_eq!(
            groq.requested_model(Some(" mixtral-8x7b ")),
            Some("mixtral-8x7b".to_string())
        );
    }

    #[test]
    fn openrouter_vendor_slash_model_is_preserved() {
        // OpenRouter ids are `vendor/model`; only the `openrouter/` routing
        // prefix may be stripped, never the meaningful vendor slash.
        let or = OpenAiCompatAgent::new(spec("openrouter"));
        assert_eq!(
            or.requested_model(Some("anthropic/claude-opus-4-8")),
            Some("anthropic/claude-opus-4-8".to_string())
        );
        // The `openrouter/` prefix (and only it) is stripped.
        assert_eq!(
            or.requested_model(Some("openrouter/anthropic/claude-opus-4-8")),
            Some("anthropic/claude-opus-4-8".to_string())
        );
    }

    #[test]
    fn blank_model_falls_back_to_spec_default() {
        let groq = OpenAiCompatAgent::new(spec("groq"));
        assert_eq!(groq.requested_model(None), None);
        assert_eq!(groq.requested_model(Some("   ")), None);
        // resolve_model uses default when no vault override is present.
        if keyvault::lookup_key_sync("groq", VAULT_MODEL_LABEL).is_err() {
            let req = ChatRequest {
                session_id: "t".into(),
                message: "hi".into(),
                project_root: None,
                history: vec![],
                model: None,
                reasoning_effort: None,
            };
            assert_eq!(groq.resolve_model(&req), "llama-3.3-70b-versatile");
        }
    }

    #[test]
    fn parses_content_delta() {
        let data = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"Hi"}}]}"#;
        assert_eq!(
            parse_chat_completion_event(data),
            Some((Some("Hi".to_string()), None))
        );
    }

    #[test]
    fn reads_total_tokens_from_usage_chunk() {
        let data = r#"{"id":"x","choices":[],"usage":{"total_tokens":99}}"#;
        assert_eq!(parse_chat_completion_event(data), Some((None, Some(99))));
    }

    #[test]
    fn tolerates_garbage_and_role_only() {
        assert_eq!(parse_chat_completion_event("not json"), None);
        assert_eq!(parse_chat_completion_event("[DONE]"), None);
        let role_only = r#"{"choices":[{"index":0,"delta":{"role":"assistant"}}]}"#;
        assert_eq!(parse_chat_completion_event(role_only), None);
    }
}
