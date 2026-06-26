//! AI session summarizer.
//!
//! Loads the full message history for a session via `TracingStore`, asks
//! the gateway for a structured summary (1-sentence headline + 3-5 bullets + open
//! questions / next steps), and optionally writes it to the Cortex Brain
//! vault under `~/Documents/Cortex Brain/sessions/<session_id>-summary.md`.
//!
//! Mirrors the chat-completion pattern in `inline_completion.rs` (system +
//! user message, streamed deltas collected into a single string) and the
//! path-safety / frontmatter pattern in `brain_import.rs` (sanitized slug,
//! anchored to the brain vault root).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::observability::tracing_store::TracingStore;

const SYSTEM_PROMPT: &str = "You are a session summarizer. Given the chat below, produce: \
(a) a 1-sentence headline, \
(b) 3-5 bullets of decisions/changes/artifacts, \
(c) any open questions or next steps. \
Return ONLY the summary, no preamble.";

/// 30s wall clock — summarisation is a single-shot ask, not interactive, so
/// we give it more headroom than the 5s inline-completion cap.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Cap how much chat history we feed the model so a 10k-message session
/// doesn't blow past context limits. Newest messages win because they're the
/// ones the user most likely wants summarised.
const MAX_HISTORY_CHARS: usize = 24_000;

#[derive(Debug, Deserialize)]
pub struct SummarizeArgs {
    pub session_id: String,
    #[serde(default)]
    pub save_to_brain: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub headline: String,
    pub body: String,
    pub generated_unix_ms: i64,
    pub saved_path: Option<PathBuf>,
}

#[tauri::command]
pub async fn summarize_session(
    args: SummarizeArgs,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<SessionSummary, String> {
    let messages = store
        .load_session_messages(&args.session_id)
        .map_err(|e| format!("load session messages failed: {e}"))?;

    if messages.is_empty() {
        return Err("session has no messages to summarize".into());
    }

    let transcript = build_transcript(&messages);
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: transcript },
        ],
        stream: true,
        temperature: Some(0.3),
    };

    let raw = run_with_timeout(client, req).await?;
    let cleaned = raw.trim().to_string();
    if cleaned.is_empty() {
        return Err("summarizer returned an empty response".into());
    }

    let (headline, body) = split_headline(&cleaned);
    let generated_unix_ms = chrono::Utc::now().timestamp_millis();

    let saved_path = if args.save_to_brain {
        match write_to_brain(&args.session_id, &headline, &body, generated_unix_ms) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("save_to_brain failed: {e}");
                None
            }
        }
    } else {
        None
    };

    Ok(SessionSummary {
        session_id: args.session_id,
        headline,
        body,
        generated_unix_ms,
        saved_path,
    })
}

async fn run_with_timeout(
    client: GatewayClient,
    req: ChatCompletionRequest,
) -> Result<String, String> {
    let started = Instant::now();
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

    match tokio::time::timeout(TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err(format!(
            "summarizer timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

/// Render the stored messages as a plain transcript the LLM can read. Older
/// messages are dropped first if the total exceeds [`MAX_HISTORY_CHARS`].
fn build_transcript(messages: &[crate::observability::tracing_store::StoredMessage]) -> String {
    // Format newest-first while measuring, then reverse so the prompt reads
    // chronologically. This way we keep the most recent turns when truncating.
    let mut lines: Vec<String> = Vec::with_capacity(messages.len());
    let mut total = 0usize;
    for msg in messages.iter().rev() {
        let role = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            other => other,
        };
        let entry = format!("{}: {}", role, msg.content.trim());
        if total + entry.len() > MAX_HISTORY_CHARS && !lines.is_empty() {
            lines.push("[…older messages truncated…]".to_string());
            break;
        }
        total += entry.len();
        lines.push(entry);
    }
    lines.reverse();
    lines.join("\n\n")
}

/// Split the model's reply into a headline + body. We treat the first
/// non-empty line as the headline and the rest as the body. If the model
/// returned everything on one line, the body falls back to the whole reply.
fn split_headline(raw: &str) -> (String, String) {
    let mut lines = raw.lines();
    let headline = lines
        .by_ref()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        // Strip common headline markers that the model sometimes adds despite
        // the prompt asking for plain text.
        .trim_start_matches(['#', '*', '-', '·', ' '])
        .trim()
        .to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    let body = if body.is_empty() { raw.trim().to_string() } else { body };
    (headline, body)
}

fn brain_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Documents").join("Cortex Brain"))
}

/// Write the summary into `~/Documents/Cortex Brain/sessions/<session_id>-summary.md`
/// with YAML frontmatter. The filename is anchored to the brain vault root and
/// the session_id is slug-sanitised so callers can't escape via traversal.
fn write_to_brain(
    session_id: &str,
    headline: &str,
    body: &str,
    generated_unix_ms: i64,
) -> Result<PathBuf, String> {
    let brain_root = brain_dir().ok_or_else(|| "could not resolve ~/Documents".to_string())?;
    let dir = brain_root.join("sessions");
    fs::create_dir_all(&dir).map_err(|e| format!("create sessions dir failed: {e}"))?;

    let slug = slugify(session_id);
    let filename = format!("{slug}-summary.md");
    let path = dir.join(&filename);

    // Path-safety: enforce that the resolved path still lives under the
    // sessions dir. Belt-and-braces — slugify already strips traversal chars.
    if !path.starts_with(&dir) {
        return Err("refusing to write outside the brain vault".into());
    }

    let iso = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(generated_unix_ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    let frontmatter = format!(
        "---\nkind: session-summary\nsession_id: {sid}\ngenerated_at: {iso}\nheadline: {hl}\n---\n\n",
        sid = yaml_escape(session_id),
        iso = iso,
        hl = yaml_escape(headline),
    );
    let doc = format!("{frontmatter}# {headline}\n\n{body}\n");
    fs::write(&path, doc).map_err(|e| format!("write {} failed: {e}", path.display()))?;
    Ok(path)
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 80 {
        out.truncate(80);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "session".into()
    } else {
        out
    }
}

fn yaml_escape(input: &str) -> String {
    let cleaned = input.replace(['\n', '\r'], " ").trim().to_string();
    if cleaned.is_empty() {
        return "unknown".into();
    }
    if cleaned.chars().any(|c| matches!(c, ':' | '#' | '"' | '\'' | '{' | '}' | '[' | ']' | ',' | '&' | '*' | '!' | '|' | '>' | '%' | '@' | '`')) {
        format!("\"{}\"", cleaned.replace('"', "\\\""))
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_headline_takes_first_line() {
        let raw = "Migration finished cleanly.\n\n- Step 1\n- Step 2";
        let (h, b) = split_headline(raw);
        assert_eq!(h, "Migration finished cleanly.");
        assert!(b.contains("Step 1"));
    }

    #[test]
    fn split_headline_strips_markdown_prefix() {
        let (h, _) = split_headline("# Refactor complete\n\nbody");
        assert_eq!(h, "Refactor complete");
    }

    #[test]
    fn split_headline_falls_back_when_single_line() {
        let (h, b) = split_headline("Just one line");
        assert_eq!(h, "Just one line");
        assert_eq!(b, "Just one line");
    }

    #[test]
    fn slugify_strips_traversal() {
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("session-abc/123"), "session-abc-123");
    }

    #[test]
    fn slugify_falls_back_when_empty() {
        assert_eq!(slugify(""), "session");
        assert_eq!(slugify("!!!"), "session");
    }
}
