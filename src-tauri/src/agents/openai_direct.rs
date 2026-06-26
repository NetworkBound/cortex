//! Direct OpenAI adapter — streams chat straight from the OpenAI Chat
//! Completions API, bypassing the Cortex Gateway entirely.
//!
//! Only compiled in the `standalone` build variant (`--features standalone`).
//! Mirrors `anthropic_direct.rs`: descriptor → health_check → run, with `run`
//! streaming events over the `mpsc::Sender`.
//!
//! The API key is re-read from the encrypted key vault on **every** run (via
//! `keyvault::lookup_key_sync("openai", "api-key")`), so updating the key in
//! Settings → Providers takes effect immediately without an app restart.
//!
//! Model selection: an explicit `req.model` is sent verbatim (a bad slug
//! surfaces as the API's own 404 rather than being silently swapped). With no
//! explicit model, the default resolves per run: vault entry
//! `openai/default-model` → `CORTEX_OPENAI_MODEL` env → `DEFAULT_MODEL`.
//!
//! Wire protocol: `POST https://api.openai.com/v1/chat/completions` with a
//! `Bearer` auth header and `stream:true`. The response is an SSE stream of
//! `chat.completion.chunk` objects terminated by a `[DONE]` sentinel; we
//! translate `choices[].delta.content` into `Token`s.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest,
};
use crate::commands::keyvault;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const AGENT_ID: &str = "openai-direct";
const API_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_MODEL: &str = "gpt-4o";
/// Env override for the default model when no vault entry is set.
const MODEL_ENV: &str = "CORTEX_OPENAI_MODEL";
const MAX_TOKENS: u64 = 4096;
/// Key vault coordinates for the user's OpenAI API key.
const VAULT_PROVIDER: &str = "openai";
const VAULT_LABEL: &str = "api-key";
/// Optional vault entry overriding the default model (Settings → Providers →
/// provider `openai`, label `default-model`).
const VAULT_MODEL_LABEL: &str = "default-model";

pub struct OpenAIDirectAgent;

impl OpenAIDirectAgent {
    pub fn new() -> Self {
        Self
    }

    fn api_key() -> Option<String> {
        keyvault::lookup_key_sync(VAULT_PROVIDER, VAULT_LABEL)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Trimmed explicit model off the request, if any. Sent verbatim — the old
    /// `starts_with("gpt")` filter silently substituted the default for any
    /// other slug (including OpenAI's own non-gpt families like o-series),
    /// masking routing bugs behind the wrong model's output.
    fn requested_model(model: Option<&str>) -> Option<String> {
        model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
    }

    /// Default model, re-resolved per run like the API key: vault entry
    /// `openai/default-model` → `CORTEX_OPENAI_MODEL` env → built-in.
    fn default_model() -> String {
        Self::requested_model(
            keyvault::lookup_key_sync(VAULT_PROVIDER, VAULT_MODEL_LABEL)
                .ok()
                .as_deref(),
        )
        .or_else(|| Self::requested_model(std::env::var(MODEL_ENV).ok().as_deref()))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    }

    fn resolve_model(req: &ChatRequest) -> String {
        Self::requested_model(req.model.as_deref()).unwrap_or_else(Self::default_model)
    }
}

impl Default for OpenAIDirectAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentAdapter for OpenAIDirectAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: AGENT_ID.to_string(),
            label: "GPT (Direct)".to_string(),
            // Honest about the direct path: this adapter streams plain chat
            // (system message + history) straight from Chat Completions. It does
            // NOT send `tools`, so there's no tool use / code-edit / shell exec,
            // and it forwards content as text only, so no real vision. Those
            // capabilities are gateway-only — route through the gateway for them.
            description:
                "OpenAI Chat Completions API direct — streams chat (with your system message) from api.openai.com using your stored API key, no gateway. Chat only: tool use, code edits, and vision are gateway-only (route via the gateway)."
                    .to_string(),
            // Declare ONLY what the direct path actually does. Tool use (and the
            // CodeEdit/ShellExec that ride on it) and Vision are not wired here
            // yet, so the UI must not offer them — see the truthful-flags audit.
            capabilities: vec![
                AgentCapability::Chat,
                AgentCapability::LongContext,
            ],
            available: Self::api_key().is_some(),
        }
    }

    async fn health_check(&self) -> bool {
        Self::api_key().is_some()
    }

    async fn run(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let _ = tx
            .send(AgentEvent::Started { agent_id: AGENT_ID.into(), run_id: None })
            .await;

        let Some(api_key) = Self::api_key() else {
            let _ = tx
                .send(AgentEvent::Error {
                    message:
                        "No OpenAI API key set. Open Settings → Providers → OpenAI to add one."
                            .into(),
                })
                .await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        };

        let model = Self::resolve_model(&req);

        // messages = mapped history (role/content) then the new user turn.
        // Chat Completions takes the system prompt as a `role: "system"` entry
        // inline in the messages array (unlike Anthropic's top-level `system`
        // field), so any "system" history turn is passed through verbatim and
        // reaches the model as its system message — no special handling needed.
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
            .post(API_URL)
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
                        message: format!("openai request failed: {e} (check your network)"),
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
                " — API key invalid. Re-enter it in Settings → Providers → OpenAI."
            } else if status.as_u16() == 404 {
                " — model not recognized by the API. Pick a current OpenAI model, or set a vault entry openai/default-model."
            } else {
                ""
            };
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!("openai returned {status}: {detail}{hint}"),
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
                        .send(AgentEvent::Error { message: format!("openai stream error: {e}") })
                        .await;
                    break;
                }
            };
            if event.data == "[DONE]" {
                break;
            }
            if let Some((delta, tokens)) = parse_openai_event(&event.data) {
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

/// Pure parse of one OpenAI SSE `data:` payload. Returns
/// `Some((maybe_text_delta, maybe_total_tokens))` for chunks we care about and
/// `None` for noise / unparseable lines. Factored out so the streaming schema
/// is unit-testable without a live API.
///
/// We read text off `choices[0].delta.content` and the optional final token
/// total off `usage.total_tokens` (only present when `include_usage` is set,
/// on the terminal chunk whose `choices` array is empty).
fn parse_openai_event(data: &str) -> Option<(Option<String>, Option<u64>)> {
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

    #[test]
    fn parses_content_delta() {
        let data = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"Hi"}}]}"#;
        assert_eq!(parse_openai_event(data), Some((Some("Hi".to_string()), None)));
    }

    #[test]
    fn parses_role_only_first_chunk_as_noise() {
        // First chunk often carries only delta.role with no content.
        let data = r#"{"id":"x","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#;
        assert_eq!(parse_openai_event(data), None);
    }

    #[test]
    fn reads_total_tokens_from_usage_chunk() {
        let data = r#"{"id":"x","choices":[],"usage":{"total_tokens":99}}"#;
        assert_eq!(parse_openai_event(data), Some((None, Some(99))));
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(parse_openai_event("not json"), None);
        assert_eq!(parse_openai_event("[DONE]"), None);
    }

    #[test]
    fn explicit_model_passes_through_verbatim() {
        assert_eq!(
            OpenAIDirectAgent::requested_model(Some(" gpt-4o ")),
            Some("gpt-4o".to_string())
        );
        // Non-gpt slugs (o-series, typos) are no longer silently swapped for
        // the default — they go to the API as-is so mismatches surface.
        assert_eq!(
            OpenAIDirectAgent::requested_model(Some("o3-mini")),
            Some("o3-mini".to_string())
        );
    }

    #[test]
    fn blank_model_falls_back_to_default() {
        assert_eq!(OpenAIDirectAgent::requested_model(None), None);
        assert_eq!(OpenAIDirectAgent::requested_model(Some("   ")), None);
    }

    #[test]
    fn descriptor_reports_only_implemented_capabilities() {
        // Truthful-flags tripwire: the direct path is chat-only. It must NOT
        // advertise tool use (CodeEdit/ShellExec) or Vision until those are
        // actually wired through Chat Completions — otherwise the UI offers a
        // capability that silently no-ops on this adapter.
        let caps = OpenAIDirectAgent::new().descriptor().capabilities;
        assert!(caps.contains(&AgentCapability::Chat));
        assert!(caps.contains(&AgentCapability::LongContext));
        assert!(!caps.contains(&AgentCapability::CodeEdit));
        assert!(!caps.contains(&AgentCapability::ShellExec));
        assert!(!caps.contains(&AgentCapability::Vision));
    }
}
