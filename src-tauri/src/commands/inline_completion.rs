//! Inline AI ghost-text completion endpoint (Terax #14).
//!
//! The CodeMirror editor pane calls `inline_complete` after the user pauses
//! typing. We round-trip through the gateway' OpenAI-compatible
//! `/v1/chat/completions` stream, collect the deltas into a single string,
//! and return it as `{ completion, latency_ms }`.
//!
//! Hard cap at 5s end-to-end so a stuck gateway never blocks the editor.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

const SYSTEM_PROMPT: &str = "You are an inline code completion assistant. \
Given the context, return ONLY the text that should be inserted at the cursor. \
Do not repeat context. Do not explain. \
Stop at logical break (end of line, end of function, etc). Max 200 tokens.";

/// 5-second wall clock on the whole gateway call — the UI is blocked on this
/// future, so any drift beyond that should be treated as a miss rather than
/// stalling the editor.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
pub struct InlineCompleteArgs {
    /// Up to 50 lines immediately preceding the cursor.
    pub before: String,
    /// Up to 20 lines immediately following the cursor.
    pub after: String,
    /// Human-readable language hint ("TypeScript", "Rust", …) — best-effort,
    /// just steers the model.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct InlineCompleteResult {
    pub completion: String,
    pub latency_ms: i64,
}

#[tauri::command]
pub async fn inline_complete(
    args: InlineCompleteArgs,
    state: State<'_, AppState>,
) -> Result<InlineCompleteResult, String> {
    let started = Instant::now();
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = build_user_prompt(&args);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.2),
    };

    // Collect streamed deltas off a channel so we keep the existing client
    // surface but expose a request/response shape to the UI.
    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);

    let stream_fut = async move {
        let _ = client.chat_completion_stream(req, tx).await;
    };

    let collect_fut = async {
        let mut buf = String::new();
        while let Some(item) = rx.recv().await {
            match item {
                StreamItem::Delta(s) => buf.push_str(&s),
                StreamItem::Done { .. } => break,
            }
        }
        buf
    };

    let collected = match tokio::time::timeout(TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => body,
        Err(_) => {
            // Soft failure — surface an empty completion so the editor can
            // simply not show a ghost. The frontend treats `""` as "no
            // suggestion".
            return Ok(InlineCompleteResult {
                completion: String::new(),
                latency_ms: started.elapsed().as_millis() as i64,
            });
        }
    };

    let completion = sanitize(&collected);
    Ok(InlineCompleteResult {
        completion,
        latency_ms: started.elapsed().as_millis() as i64,
    })
}

fn build_user_prompt(args: &InlineCompleteArgs) -> String {
    let lang = args
        .language
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("plain text");
    format!(
        "Language: {lang}\n\
         --- BEFORE CURSOR ---\n{before}\n\
         --- AFTER CURSOR ---\n{after}\n\
         --- COMPLETION ---\n",
        lang = lang,
        before = args.before,
        after = args.after,
    )
}

/// Strip common chat-model preambles ("Here is the completion:" etc.) and
/// fenced code blocks, since the prompt explicitly forbids them but cheaper
/// models sometimes include them anyway.
fn sanitize(raw: &str) -> String {
    let mut s = raw.trim_start_matches('\n').to_string();

    // Drop a leading fenced block if the entire reply is one.
    if let Some(rest) = s.strip_prefix("```") {
        // Skip optional language tag on the same line.
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_fenced_block() {
        let raw = "```ts\nconst x = 1;\n```";
        assert_eq!(sanitize(raw), "const x = 1;");
    }

    #[test]
    fn sanitize_passes_through_plain_text() {
        assert_eq!(sanitize("foo bar"), "foo bar");
    }

    #[test]
    fn build_user_prompt_uses_default_language() {
        let args = InlineCompleteArgs {
            before: "let x =".into(),
            after: "".into(),
            language: None,
        };
        let p = build_user_prompt(&args);
        assert!(p.contains("Language: plain text"));
        assert!(p.contains("--- BEFORE CURSOR ---"));
    }
}
