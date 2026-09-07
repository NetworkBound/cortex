//! Local Ollama adapter — streams chat directly from an Ollama server.
//!
//! Bypasses the Cortex Gateway: POSTs to `<base_url>/api/chat` with
//! `stream:true` and translates Ollama's NDJSON chunks into Cortex
//! `AgentEvent`s. Mirrors the structure/style of `gateway_remote.rs`:
//! descriptor → health_check → run, with `run` streaming events over the
//! `mpsc::Sender`.
//!
//! Ollama's `/api/chat` returns newline-delimited JSON: each line is
//! `{"message":{"role","content"},"done":bool,...}` and the final line has
//! `"done":true` (carrying `eval_count`). We buffer raw bytes and split on
//! `\n` so a chunk that splits a JSON line mid-stream is tolerated.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest,
};
use futures::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;

const AGENT_ID: &str = "ollama";

/// Issue 008 — hard cap on chat rounds in one turn (1 initial + follow-ups
/// after tool calls), so a model that keeps requesting tools can't loop
/// forever. Without advertised MCP tools exactly one round runs, matching
/// the pre-tool behavior of this adapter.
const MAX_TOOL_ROUNDS: usize = 5;

/// The Ollama server on this machine. The Cookbook pulls models here when a
/// local server is running, while `ollama_base_url` may point at a remote
/// homelab box — discovery and routing below consider both.
pub const LOCAL_OLLAMA: &str = "http://127.0.0.1:11434";

/// Fetch one server's available tags (`/api/tags`), best-effort with a 3s
/// timeout. Returns an empty Vec on any failure (or an empty base).
pub async fn fetch_tags_at(base: &str) -> Vec<String> {
    if base.is_empty() {
        return Vec::new();
    }
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return Vec::new();
    };
    let Ok(resp) = client.get(format!("{base}/api/tags")).send().await else {
        return Vec::new();
    };
    let Ok(json) = resp.json::<Value>().await else {
        return Vec::new();
    };
    json.get("models")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Does a tag list serve `model`? Matches exactly, or via Ollama's implicit
/// `:latest` (a bare `llama3.2` request is served by the `llama3.2:latest` tag).
fn has_tag(tags: &[String], model: &str) -> bool {
    tags.iter()
        .any(|t| t == model || t.strip_suffix(":latest").is_some_and(|bare| bare == model))
}

pub struct OllamaAgent {
    base_url: String,
    default_model: String,
}

impl OllamaAgent {
    pub fn new(base_url: String, default_model: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            default_model,
        }
    }

    /// Resolve the model for a request: strip a leading `ollama:` / `ollama/`
    /// prefix off the per-call slug, else fall back to the configured default.
    fn resolve_model(&self, req: &ChatRequest) -> String {
        req.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| {
                m.strip_prefix("ollama:")
                    .or_else(|| m.strip_prefix("ollama/"))
                    .unwrap_or(m)
                    .to_string()
            })
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.default_model.clone())
    }

    /// All tags this adapter can serve: the configured server's plus the local
    /// server's (deduped) — Cookbook pulls land locally even when the config
    /// points at a remote box, and both are real chat targets.
    async fn fetch_tags(&self) -> Vec<String> {
        let mut tags = fetch_tags_at(&self.base_url).await;
        if self.base_url != LOCAL_OLLAMA {
            for t in fetch_tags_at(LOCAL_OLLAMA).await {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
        tags
    }

    /// Route a concrete tag to the server that actually has it: the configured
    /// base wins, the local server (where Cookbook pulls land) is the fallback.
    /// If neither lists the tag, return the configured base so the server's own
    /// "model not found" error surfaces against the expected endpoint.
    async fn resolve_base_for_model(&self, model: &str) -> String {
        if has_tag(&fetch_tags_at(&self.base_url).await, model) {
            return self.base_url.clone();
        }
        if self.base_url != LOCAL_OLLAMA && has_tag(&fetch_tags_at(LOCAL_OLLAMA).await, model) {
            return LOCAL_OLLAMA.to_string();
        }
        self.base_url.clone()
    }

    /// Resolve the best available local tag for this request by inspecting the
    /// message (+ last user history turn). Deterministic, tier-based. Falls back
    /// to the configured default if discovery fails or nothing matches.
    async fn auto_select_model(&self, req: &ChatRequest) -> String {
        let tags = self.fetch_tags().await;
        if tags.is_empty() {
            return self.default_model.clone();
        }

        // Never auto-pick an embedding model — they can't chat.
        let is_embed = |n: &str| n.to_lowercase().contains("embed");
        let candidates: Vec<&String> = tags.iter().filter(|n| !is_embed(n)).collect();
        if candidates.is_empty() {
            return self.default_model.clone();
        }
        let contains = |needle: &str| -> Option<String> {
            candidates
                .iter()
                .find(|n| n.to_lowercase().contains(needle))
                .map(|n| (*n).clone())
        };

        // Build the classification corpus: this message + the last user turn.
        let last_user = req
            .history
            .iter()
            .rev()
            .find(|t| t.role == "user")
            .map(|t| t.content.as_str())
            .unwrap_or("");
        let text = format!("{}\n{}", req.message, last_user);
        let lower = text.to_lowercase();
        let word_count = req.message.split_whitespace().count();

        // --- Tier 1: vision (image markers / explicit image reference) ---
        let has_image = lower.contains("![")
            || lower.contains("data:image")
            || lower.contains(".png")
            || lower.contains(".jpg")
            || lower.contains(".jpeg")
            || lower.contains("image")
            || lower.contains("screenshot");
        if has_image {
            if let Some(m) = contains("minicpm-v") {
                return m;
            }
        }

        // --- Tier 2: coding (code fences / language tokens / keywords) ---
        let is_coding = text.contains("```")
            || text.contains("fn ")
            || text.contains("def ")
            || text.contains("class ")
            || text.contains("import ")
            || ["compile", "bug", "refactor", "function", "code"]
                .iter()
                .any(|k| lower.contains(k));
        if is_coding {
            if let Some(m) = contains("coder") {
                return m;
            }
        }

        // --- Tier 3: complex / reasoning (long prompt or reasoning keywords) ---
        let is_complex = word_count > 60
            || [
                "reason", "analyze", "explain", "why", "prove", "derive", "plan", "architect",
            ]
            .iter()
            .any(|k| lower.contains(k));
        if is_complex {
            // Prefer a dedicated reasoning model, then a 32b, then the largest.
            if let Some(m) = contains("deepseek-r1") {
                return m;
            }
            if let Some(m) = contains(":32b") {
                return m;
            }
            if let Some(m) = largest_tag(&candidates) {
                return m;
            }
        }

        // --- Tier 4 (default): simple / short → prefer a fast/small model ---
        if let Some(m) = contains("homelab-fast") {
            return m;
        }
        if let Some(m) = contains("qwen2.5:14b") {
            return m;
        }
        // First non-embed model as a last resort.
        candidates
            .first()
            .map(|n| (*n).clone())
            .unwrap_or_else(|| self.default_model.clone())
    }
}

/// One parsed NDJSON line from `/api/chat`. `delta` is the streamed content
/// fragment; `tool_calls` carries any model-requested tool invocations
/// (issue 008 — present only when `tools` were advertised); `done` marks the
/// final record, whose `eval_count` is the turn's token total.
#[derive(Debug, Default, PartialEq)]
struct ChatLine {
    delta: Option<String>,
    tool_calls: Vec<Value>,
    done: bool,
    eval_count: Option<u64>,
}

/// Parse one line of the NDJSON chat stream. `None` for blank lines and
/// non-JSON noise (tolerated exactly like the pre-tool parser did).
fn parse_chat_line(line: &str) -> Option<ChatLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let json = serde_json::from_str::<Value>(line).ok()?;
    let msg = json.get("message");
    Some(ChatLine {
        delta: msg
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string),
        tool_calls: msg
            .and_then(|m| m.get("tool_calls"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        done: json.get("done").and_then(Value::as_bool).unwrap_or(false),
        eval_count: json.get("eval_count").and_then(Value::as_u64),
    })
}

/// Ollama sends tool-call `arguments` as a JSON object; some builds emit a
/// JSON-encoded string instead. Normalize to a JSON value either way so the
/// dispatcher always receives structured args (an unparseable string is
/// wrapped rather than dropped, so the gate chain still sees it).
fn normalize_tool_args(args: Option<&Value>) -> Value {
    match args {
        None | Some(Value::Null) => json!({}),
        Some(Value::String(s)) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({ "raw": s }))
        }
        Some(v) => v.clone(),
    }
}

/// Pick the "largest" tag by parsing a trailing `:<N>b` parameter-size hint
/// (e.g. `qwen2.5:32b` → 32). Tags without a size hint sort as 0.
fn largest_tag(candidates: &[&String]) -> Option<String> {
    candidates
        .iter()
        .max_by_key(|n| {
            n.to_lowercase()
                .rsplit_once(':')
                .and_then(|(_, suffix)| suffix.strip_suffix('b'))
                .and_then(|num| num.parse::<u32>().ok())
                .unwrap_or(0)
        })
        .map(|n| (*n).clone())
}

#[async_trait::async_trait]
impl AgentAdapter for OllamaAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: AGENT_ID.to_string(),
            label: "Ollama (local)".to_string(),
            description: format!(
                "Local Ollama server at {} — streams chat directly, bypassing the Cortex Gateway. Default model: {}",
                self.base_url, self.default_model,
            ),
            capabilities: vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::LongContext,
            ],
            available: !self.base_url.is_empty(),
        }
    }

    async fn health_check(&self) -> bool {
        if self.base_url.is_empty() {
            return false;
        }
        let client = match crate::tailscale::maybe_tailscale_proxy(
            reqwest::Client::builder().timeout(Duration::from_secs(3)),
        )
        .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        let configured_up = client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if configured_up {
            return true;
        }
        // A running local server can still serve chats (Cookbook pulls land
        // there), so it keeps the adapter healthy when the remote is down.
        self.base_url != LOCAL_OLLAMA
            && client
                .get(format!("{LOCAL_OLLAMA}/api/tags"))
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
        // Announce the run immediately so the UI shows activity.
        let _ = tx
            .send(AgentEvent::Started { agent_id: AGENT_ID.into(), run_id: None })
            .await;

        if self.base_url.is_empty() {
            let _ = tx
                .send(AgentEvent::Error {
                    message: "No Ollama base URL configured. Set OLLAMA_BASE_URL or the ollama_base_url config.".into(),
                })
                .await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        }

        let mut model = self.resolve_model(&req);

        // "auto" → pick the best local tag for this task. Best-effort: if tag
        // discovery fails we fall back to the configured default (done inside).
        if model == "auto" {
            model = self.auto_select_model(&req).await;
            let _ = tx
                .send(AgentEvent::Reasoning {
                    text: format!("Auto-selected local model: {model}"),
                })
                .await;
        }

        // messages = mapped history (role/content) then the new user turn.
        let mut messages: Vec<Value> = req
            .history
            .iter()
            .map(|t| json!({ "role": t.role, "content": t.content }))
            .collect();
        messages.push(json!({ "role": "user", "content": req.message }));

        // Issue 008 — MCP chat tools. Default-empty: a server only appears
        // here after the user flipped its "expose in chat" toggle in the MCP
        // panel AND it is currently connected, so the pre-existing single-
        // round behavior is untouched until someone opts in. Ollama's
        // /api/chat takes OpenAI-style function specs.
        let mcp_tools = crate::mcp::chat_tools::advertised_tools();
        let tool_defs: Vec<Value> = mcp_tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    },
                })
            })
            .collect();
        let mut send_tools = !tool_defs.is_empty();

        // Send to whichever server actually serves this tag (configured base
        // first, local Cookbook server as fallback).
        let base = self.resolve_base_for_model(&model).await;
        // Route through the embedded-Tailscale SOCKS5 proxy when enabled +
        // connected so a homelab Ollama resolves + tunnels over the tailnet.
        let client = crate::tailscale::maybe_tailscale_proxy(reqwest::Client::builder())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut total_tokens: Option<u64> = None;

        // One iteration per chat round: the first request, then one follow-up
        // per batch of executed tool calls. Without tools this runs exactly
        // once (`tool_calls` stays empty → break).
        for round in 0..MAX_TOOL_ROUNDS {
            let mut body = json!({
                "model": model,
                "messages": messages,
                "stream": true,
            });
            if send_tools {
                body["tools"] = Value::Array(tool_defs.clone());
            }

            let resp = match client
                .post(format!("{base}/api/chat"))
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: format!("ollama request failed: {e} (check OLLAMA_BASE_URL / that Ollama is running)"),
                        })
                        .await;
                    let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                    return Ok(());
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let detail = resp.text().await.unwrap_or_default();
                let detail = detail.trim().to_string();
                // A model without tool support rejects the `tools` field with
                // a 400 — degrade to a plain chat round instead of failing
                // the whole turn.
                if send_tools
                    && status.as_u16() == 400
                    && detail.contains("does not support tools")
                {
                    send_tools = false;
                    let _ = tx
                        .send(AgentEvent::Reasoning {
                            text: format!(
                                "Model '{model}' does not support tools — continuing without MCP tools."
                            ),
                        })
                        .await;
                    continue;
                }
                let _ = tx
                    .send(AgentEvent::Error {
                        message: format!("ollama returned {status}: {detail}"),
                    })
                    .await;
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                return Ok(());
            }

            // Stream raw bytes; buffer and split on '\n' so a JSON line split
            // across two chunks is reassembled before parsing.
            let mut stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut content = String::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            let mut stream_failed = false;

            while let Some(chunk) = stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx
                            .send(AgentEvent::Error { message: format!("ollama stream error: {e}") })
                            .await;
                        stream_failed = true;
                        break;
                    }
                };
                buf.extend_from_slice(&bytes);

                // Drain every complete line currently in the buffer.
                while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=nl).collect();
                    let line = &line[..line.len() - 1]; // drop the '\n'
                    let line = std::str::from_utf8(line).unwrap_or("");
                    let Some(parsed) = parse_chat_line(line) else {
                        continue; // tolerate any non-JSON noise
                    };
                    if let Some(delta) = parsed.delta {
                        if !delta.is_empty() {
                            content.push_str(&delta);
                            let _ = tx.send(AgentEvent::Token { delta }).await;
                        }
                    }
                    tool_calls.extend(parsed.tool_calls);
                    if parsed.done {
                        total_tokens = parsed.eval_count;
                    }
                }
            }

            // Flush a final, non-newline-terminated line. NDJSON does not
            // require a trailing '\n' on the last record, and that record is
            // exactly the {"done":true,"eval_count":...} object — without
            // this, total_tokens (and any trailing content/tool calls) from
            // such a stream would be dropped.
            if !buf.is_empty() {
                if let Ok(line) = std::str::from_utf8(&buf) {
                    if let Some(parsed) = parse_chat_line(line) {
                        if let Some(delta) = parsed.delta {
                            if !delta.is_empty() {
                                content.push_str(&delta);
                                let _ = tx.send(AgentEvent::Token { delta }).await;
                            }
                        }
                        tool_calls.extend(parsed.tool_calls);
                        if parsed.done {
                            total_tokens = parsed.eval_count;
                        }
                    }
                }
            }

            if stream_failed || tool_calls.is_empty() || !send_tools {
                break;
            }
            if round + 1 == MAX_TOOL_ROUNDS {
                let _ = tx
                    .send(AgentEvent::Error {
                        message: format!(
                            "MCP tool-call round limit reached ({MAX_TOOL_ROUNDS}) — ending the turn."
                        ),
                    })
                    .await;
                break;
            }

            // Feed the model's tool calls back: its assistant turn first, then
            // one `role:"tool"` message per call. EVERY call goes through the
            // gated dispatcher in commands::chat — chat exposure + per-tool
            // enablement, sandbox tier / Safe Mode / guardrails, and the
            // per-server trust tier (with its approval pause) — which also
            // emits the ToolCall/ToolResult events for Run Replay. A refusal
            // comes back as an ERROR message the model can read and adapt to.
            messages.push(json!({
                "role": "assistant",
                "content": content,
                "tool_calls": tool_calls,
            }));
            for call in &tool_calls {
                let function = call.get("function");
                let name = function
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let call_args = normalize_tool_args(function.and_then(|f| f.get("arguments")));
                let outcome = crate::commands::chat::dispatch_mcp_chat_tool(
                    req.project_root.as_deref(),
                    &name,
                    &call_args,
                    &tx,
                )
                .await;
                let tool_content = match outcome {
                    Ok(result) => result,
                    Err(reason) => format!("ERROR: {reason}"),
                };
                messages.push(json!({
                    "role": "tool",
                    "content": tool_content,
                    "tool_name": name,
                }));
            }
        }

        let _ = tx
            .send(AgentEvent::Done { total_tokens, run_id: None })
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn has_tag_matches_exact_and_implicit_latest() {
        let t = tags(&["llama3.2:1b", "mistral:latest"]);
        assert!(has_tag(&t, "llama3.2:1b"));
        // a bare name is served by the `:latest` tag (Ollama's own resolution)
        assert!(has_tag(&t, "mistral"));
        assert!(has_tag(&t, "mistral:latest"));
        // a bare name must NOT match a sized tag — `llama3.2` would resolve to
        // `llama3.2:latest` server-side, which this list does not have
        assert!(!has_tag(&t, "llama3.2"));
        assert!(!has_tag(&t, "qwen2.5:7b"));
    }

    #[test]
    fn largest_tag_prefers_biggest_param_hint() {
        let t = tags(&["qwen2.5:7b", "qwen2.5:32b", "mistral:latest"]);
        let refs: Vec<&String> = t.iter().collect();
        assert_eq!(largest_tag(&refs).as_deref(), Some("qwen2.5:32b"));
    }

    #[test]
    fn parse_chat_line_reads_content_done_and_noise() {
        // A plain content chunk.
        let l = parse_chat_line(r#"{"message":{"role":"assistant","content":"Hi"},"done":false}"#)
            .unwrap();
        assert_eq!(l.delta.as_deref(), Some("Hi"));
        assert!(l.tool_calls.is_empty());
        assert!(!l.done);
        // The final record carries eval_count.
        let l = parse_chat_line(r#"{"message":{"content":""},"done":true,"eval_count":42}"#)
            .unwrap();
        assert!(l.done);
        assert_eq!(l.eval_count, Some(42));
        // Blank lines and non-JSON noise are skipped, exactly as before.
        assert_eq!(parse_chat_line("   "), None);
        assert_eq!(parse_chat_line("not json"), None);
    }

    /// Issue 008: a streamed line carrying `message.tool_calls` surfaces them
    /// for the dispatch round.
    #[test]
    fn parse_chat_line_extracts_tool_calls() {
        let raw = r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"mcp__fs__read_file","arguments":{"path":"a.txt"}}}]},"done":false}"#;
        let l = parse_chat_line(raw).unwrap();
        assert_eq!(l.tool_calls.len(), 1);
        assert_eq!(
            l.tool_calls[0]["function"]["name"].as_str(),
            Some("mcp__fs__read_file")
        );
    }

    /// Issue 008: `arguments` normalizes to a JSON object whether Ollama sent
    /// an object, a JSON-encoded string, or nothing at all.
    #[test]
    fn normalize_tool_args_handles_object_string_and_missing() {
        let obj = serde_json::json!({ "path": "a.txt" });
        assert_eq!(normalize_tool_args(Some(&obj)), obj);
        let s = Value::String(r#"{"path":"a.txt"}"#.to_string());
        assert_eq!(normalize_tool_args(Some(&s)), obj);
        let bad = Value::String("not json".to_string());
        assert_eq!(
            normalize_tool_args(Some(&bad)),
            serde_json::json!({ "raw": "not json" })
        );
        assert_eq!(normalize_tool_args(None), serde_json::json!({}));
        assert_eq!(
            normalize_tool_args(Some(&Value::Null)),
            serde_json::json!({})
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Live two-server routing check (run manually:
    /// `cargo test --lib agents::ollama -- --ignored`). Requires the real
    /// homelab condition this fix targets: a local Ollama on 127.0.0.1:11434
    /// holding a Cookbook-pulled tag the configured remote does NOT have.
    /// Asserts the adapter (constructed with the remote base, as lib.rs does)
    /// discovers the local tag in its union and routes it to the local server.
    #[tokio::test]
    #[ignore]
    async fn live_routes_local_only_tag_to_local_server() {
        let remote = "http://192.0.2.10:11434";
        let local_tags = fetch_tags_at(LOCAL_OLLAMA).await;
        assert!(!local_tags.is_empty(), "local ollama must be running with ≥1 model");
        let remote_tags = fetch_tags_at(remote).await;
        let local_only = local_tags
            .iter()
            .find(|t| !remote_tags.contains(t))
            .expect("need a tag present locally but not on the remote");

        let agent = OllamaAgent::new(remote.to_string(), "qwen2.5:14b".to_string());
        let union = agent.fetch_tags().await;
        assert!(union.contains(local_only), "union discovery must include the local tag");
        assert_eq!(
            agent.resolve_base_for_model(local_only).await,
            LOCAL_OLLAMA,
            "a local-only tag must route to the local server"
        );
        // A remote tag (when the remote is up) must keep routing to the remote.
        if let Some(rt) = remote_tags.first() {
            assert_eq!(agent.resolve_base_for_model(rt).await, remote);
        }
    }

    /// Live full-chat check: stream an actual completion through the adapter
    /// for a local-only tag while configured against the remote base — the
    /// exact "Cookbook pull → Use in chat" hand-off path. Passes only if real
    /// tokens come back and no Error event fires.
    #[tokio::test]
    #[ignore]
    async fn live_chat_streams_from_local_only_tag() {
        let remote = "http://192.0.2.10:11434";
        let local_tags = fetch_tags_at(LOCAL_OLLAMA).await;
        let remote_tags = fetch_tags_at(remote).await;
        let local_only = local_tags
            .iter()
            .find(|t| !remote_tags.contains(t) && !t.contains("embed"))
            .expect("need a chat-capable tag present locally but not on the remote");

        let agent = OllamaAgent::new(remote.to_string(), "qwen2.5:14b".to_string());
        let (tx, mut rx) = mpsc::channel(64);
        let req = ChatRequest {
            session_id: "live-routing-test".to_string(),
            message: "Reply with the single word OK".to_string(),
            project_root: None,
            history: Vec::new(),
            model: Some(format!("ollama:{local_only}")),
            reasoning_effort: None,
        };
        // Drain concurrently — run() streams into the bounded channel and would
        // deadlock if nothing received until it returned.
        let runner = tokio::spawn(async move { agent.run(req, tx).await });
        let mut text = String::new();
        let mut errors: Vec<String> = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Token { delta } => text.push_str(&delta),
                AgentEvent::Error { message } => errors.push(message),
                _ => {}
            }
        }
        runner.await.expect("join").expect("run must not error");
        assert!(errors.is_empty(), "chat errored: {errors:?}");
        assert!(!text.trim().is_empty(), "expected streamed tokens from the local model");
    }
}
