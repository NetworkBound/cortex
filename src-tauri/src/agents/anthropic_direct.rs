//! Direct Anthropic adapter — streams chat straight from the Anthropic
//! Messages API, bypassing the Cortex Gateway entirely.
//!
//! Only compiled in the `standalone` build variant (`--features standalone`).
//! Mirrors the structure/style of `ollama.rs` and `gateway_remote.rs`:
//! descriptor → health_check → run, with `run` streaming events over the
//! `mpsc::Sender`.
//!
//! The API key is re-read from the encrypted key vault on **every** run (via
//! `keyvault::lookup_key_sync("anthropic", "api-key")`), so updating the key
//! in Settings → Providers takes effect immediately without an app restart.
//!
//! Model selection: an explicit `req.model` is sent verbatim (a bad slug
//! surfaces as the API's own 404 rather than being silently swapped). With no
//! explicit model, the default resolves per run: vault entry
//! `anthropic/default-model` → `CORTEX_ANTHROPIC_MODEL` env → `DEFAULT_MODEL`.
//!
//! Wire protocol: `POST https://api.anthropic.com/v1/messages` with
//! `x-api-key` + `anthropic-version` headers and `stream:true`. The response
//! is an SSE stream of typed events; we translate `content_block_delta` /
//! `text_delta` events into `Token`s and read final usage off `message_delta`
//! / `message_stop`.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest, ChatTurn,
};
use crate::commands::keyvault;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const AGENT_ID: &str = "anthropic-direct";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-opus-4-8";
/// Env override for the default model when no vault entry is set.
const MODEL_ENV: &str = "CORTEX_ANTHROPIC_MODEL";
const MAX_TOKENS: u64 = 4096;
/// Key vault coordinates for the user's Anthropic API key.
const VAULT_PROVIDER: &str = "anthropic";
const VAULT_LABEL: &str = "api-key";
/// Optional vault entry overriding the default model (Settings → Providers →
/// provider `anthropic`, label `default-model`).
const VAULT_MODEL_LABEL: &str = "default-model";

pub struct AnthropicDirectAgent;

impl AnthropicDirectAgent {
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
    /// `starts_with("claude")` filter silently substituted the default for any
    /// other slug, masking routing bugs behind the wrong model's output.
    fn requested_model(model: Option<&str>) -> Option<String> {
        model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
    }

    /// Default model, re-resolved per run like the API key: vault entry
    /// `anthropic/default-model` → `CORTEX_ANTHROPIC_MODEL` env → built-in.
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

impl Default for AnthropicDirectAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentAdapter for AnthropicDirectAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: AGENT_ID.to_string(),
            label: "Claude (Direct)".to_string(),
            // Honest about the direct path: this adapter streams plain chat
            // (system prompt + history) straight from the Messages API. It does
            // NOT send `tools`, so there's no tool use / code-edit / shell exec,
            // and it forwards content as text only, so no real vision. Those
            // capabilities are gateway-only — route through the gateway for them.
            description:
                "Anthropic Messages API direct — streams chat (with your system prompt) from api.anthropic.com using your stored API key, no gateway. Chat only: tool use, code edits, and vision are gateway-only (route via the gateway)."
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
        // Non-destructive: presence of a key is the readiness signal. We do not
        // burn tokens probing the API on every health poll.
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
                        "No Anthropic API key set. Open Settings → Providers → Anthropic to add one."
                            .into(),
                })
                .await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        };

        let model = Self::resolve_model(&req);

        // Anthropic only accepts "user"/"assistant" roles in the `messages`
        // array — the system prompt goes in a top-level `system` field. Any
        // "system" history turn is therefore lifted out and concatenated into
        // that field (rather than dropped, which silently lost the user's
        // instructions before this fix), and the rest become the messages.
        let system = collect_system(&req.history);
        let mut messages: Vec<Value> = req
            .history
            .iter()
            .filter(|t| t.role == "user" || t.role == "assistant")
            .map(|t| json!({ "role": t.role, "content": t.content }))
            .collect();
        messages.push(json!({ "role": "user", "content": req.message }));

        let mut body = json!({
            "model": model,
            "max_tokens": MAX_TOKENS,
            "messages": messages,
            "stream": true,
        });
        // Only attach `system` when there's something to send — an empty/blank
        // system field is pointless and an empty-string `system` is a 400.
        if let Some(system) = system {
            body["system"] = Value::String(system);
        }

        let client = reqwest::Client::new();
        let resp = match client
            .post(API_URL)
            .header("x-api-key", &api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Error {
                        message: format!("anthropic request failed: {e} (check your network)"),
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
                " — API key invalid. Re-enter it in Settings → Providers → Anthropic."
            } else if status.as_u16() == 404 {
                " — model not recognized by the API. Pick a current Claude model (e.g. claude-opus-4-8 or claude-sonnet-4-6), or set a vault entry anthropic/default-model."
            } else {
                ""
            };
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!("anthropic returned {status}: {detail}{hint}"),
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
                        .send(AgentEvent::Error { message: format!("anthropic stream error: {e}") })
                        .await;
                    break;
                }
            };
            if let Some((delta, tokens)) = parse_anthropic_event(&event.data) {
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

/// Lift every `system`-role turn out of the chat history and join them into a
/// single string for the Messages API's top-level `system` parameter (Anthropic
/// rejects "system" inside the `messages` array). Blank turns are dropped;
/// returns `None` when there is no non-empty system text so the caller can omit
/// the field entirely (an empty `system` is a 400).
fn collect_system(history: &[ChatTurn]) -> Option<String> {
    let joined = history
        .iter()
        .filter(|t| t.role == "system")
        .map(|t| t.content.trim())
        .filter(|c| !c.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Pure parse of one Anthropic SSE `data:` payload. Returns
/// `Some((maybe_text_delta, maybe_output_tokens))` for events we care about and
/// `None` for noise / unparseable lines. Factored out so the streaming schema
/// is unit-testable without a live API.
///
/// Events of interest:
///   - `content_block_delta` with `delta.type == "text_delta"` → text chunk
///   - `message_delta` carrying `usage.output_tokens` → final token count
fn parse_anthropic_event(data: &str) -> Option<(Option<String>, Option<u64>)> {
    let json: Value = serde_json::from_str(data).ok()?;
    let kind = json.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "content_block_delta" => {
            let delta = json.get("delta")?;
            if delta.get("type").and_then(Value::as_str) == Some("text_delta") {
                let text = delta.get("text").and_then(Value::as_str)?.to_string();
                return Some((Some(text), None));
            }
            None
        }
        "message_delta" => {
            let tokens = json
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64);
            tokens.map(|t| (None, Some(t)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta() {
        let data =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#;
        assert_eq!(parse_anthropic_event(data), Some((Some("Hi".to_string()), None)));
    }

    #[test]
    fn ignores_non_text_block_delta() {
        let data =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}"#;
        assert_eq!(parse_anthropic_event(data), None);
    }

    #[test]
    fn reads_output_tokens_from_message_delta() {
        let data =
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#;
        assert_eq!(parse_anthropic_event(data), Some((None, Some(42))));
    }

    #[test]
    fn tolerates_unknown_and_garbage() {
        assert_eq!(parse_anthropic_event(r#"{"type":"ping"}"#), None);
        assert_eq!(parse_anthropic_event("not json"), None);
        assert_eq!(parse_anthropic_event("[DONE]"), None);
    }

    #[test]
    fn explicit_model_passes_through_verbatim() {
        assert_eq!(
            AnthropicDirectAgent::requested_model(Some(" claude-sonnet-4-6 ")),
            Some("claude-sonnet-4-6".to_string())
        );
        // Non-claude slugs are no longer silently swapped for the default —
        // they go to the API as-is so the mismatch surfaces as a 404.
        assert_eq!(
            AnthropicDirectAgent::requested_model(Some("llama3:8b")),
            Some("llama3:8b".to_string())
        );
    }

    #[test]
    fn blank_model_falls_back_to_default() {
        assert_eq!(AnthropicDirectAgent::requested_model(None), None);
        assert_eq!(AnthropicDirectAgent::requested_model(Some("   ")), None);
    }

    fn turn(role: &str, content: &str) -> ChatTurn {
        ChatTurn { role: role.into(), content: content.into(), agent: None }
    }

    #[test]
    fn collect_system_lifts_and_joins_system_turns() {
        let history = vec![
            turn("system", "You are a terse assistant."),
            turn("user", "hi"),
            turn("assistant", "hello"),
            turn("system", "  Always cite sources.  "),
        ];
        assert_eq!(
            collect_system(&history),
            Some("You are a terse assistant.\n\nAlways cite sources.".to_string())
        );
    }

    #[test]
    fn collect_system_none_when_absent_or_blank() {
        assert_eq!(collect_system(&[turn("user", "hi")]), None);
        // Blank/whitespace-only system turns must not produce an empty `system`
        // field (Anthropic 400s on `system: ""`).
        assert_eq!(collect_system(&[turn("system", "   ")]), None);
        assert_eq!(collect_system(&[]), None);
    }

    #[test]
    fn descriptor_reports_only_implemented_capabilities() {
        // Truthful-flags tripwire: the direct path is chat-only. It must NOT
        // advertise tool use (CodeEdit/ShellExec) or Vision until those are
        // actually wired through the Messages API — otherwise the UI offers a
        // capability that silently no-ops on this adapter.
        let caps = AnthropicDirectAgent::new().descriptor().capabilities;
        assert!(caps.contains(&AgentCapability::Chat));
        assert!(caps.contains(&AgentCapability::LongContext));
        assert!(!caps.contains(&AgentCapability::CodeEdit));
        assert!(!caps.contains(&AgentCapability::ShellExec));
        assert!(!caps.contains(&AgentCapability::Vision));
    }

    #[test]
    fn default_model_is_a_current_alias() {
        // Tripwire: the previous default (claude-3-5-sonnet-20241022) was
        // retired by Anthropic on 2025-10-28 and 404s. Date-suffixed pins rot;
        // keep the default on a bare current alias.
        assert!(DEFAULT_MODEL.starts_with("claude-"));
        assert!(!DEFAULT_MODEL.contains("2024"));
        assert!(!DEFAULT_MODEL.contains("3-5"));
    }
}
