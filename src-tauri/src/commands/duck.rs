//! Rubber-duck Socratic chat partner.
//!
//! `/duck <topic>` opens a back-and-forth dialog where the model is forbidden
//! from giving direct answers — it only asks one focused clarifying question
//! at a time. The transcript lives in the frontend; the backend is a thin
//! single-turn wrapper around the gateway streaming chat endpoint, mirroring
//! the prompt-shape used by [`super::session_summary`].
//!
//! `duck_question` accepts the current transcript (so we don't have to
//! persist it server-side) and returns the next `DuckTurn` the model
//! produced. A separate `save_duck_transcript` writes the whole dialog out to
//! `~/Documents/Cortex Brain/duck/<date>-<slug>.md`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Strict Socratic system prompt — no direct answers, one terse question per
/// turn. We trim the model output to a single line before returning so the
/// UI never has to deal with multi-paragraph "duck" bubbles.
const SYSTEM_PROMPT: &str = "You are a rubber duck — a Socratic thinking partner. \
NEVER give direct answers or solutions. \
Ask one focused clarifying question that helps the user reason through their problem. \
Be terse and curious. Return ONLY the question.";

/// 20s wall clock — single question, single short reply.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Cap the transcript we replay to the model so a 30-turn session can't blow
/// past context limits. We keep the *newest* turns because the duck only
/// needs short-term context to ask its next question.
const MAX_TRANSCRIPT_CHARS: usize = 12_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DuckTurn {
    /// "user" or "duck".
    pub role: String,
    pub content: String,
    pub ts_unix_ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct DuckQuestionArgs {
    pub topic: String,
    #[serde(default)]
    pub transcript: Vec<DuckTurn>,
}

#[tauri::command]
pub async fn duck_question(
    args: DuckQuestionArgs,
    state: State<'_, AppState>,
) -> Result<DuckTurn, String> {
    let topic = args.topic.trim();
    if topic.is_empty() {
        return Err("topic is empty — give the duck something to chew on".into());
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = build_user_prompt(topic, &args.transcript);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        // Slightly higher than the summarizer — we *want* a bit of variety so
        // the duck doesn't ask the same question every turn — but capped well
        // below 1.0 so it stays focused.
        temperature: Some(0.5),
    };

    let raw = run_with_timeout(client, req).await?;
    let cleaned = sanitize_question(&raw);
    if cleaned.is_empty() {
        return Err("duck returned an empty question".into());
    }

    Ok(DuckTurn {
        role: "duck".into(),
        content: cleaned,
        ts_unix_ms: chrono::Utc::now().timestamp_millis(),
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
            "duck timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

/// Render the topic + transcript so the model has just enough context to ask
/// the *next* question. Newer turns win when we have to truncate.
fn build_user_prompt(topic: &str, transcript: &[DuckTurn]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(transcript.len() + 2);
    let mut total = 0usize;
    for turn in transcript.iter().rev() {
        let role = if turn.role == "duck" { "Duck" } else { "User" };
        let entry = format!("{}: {}", role, turn.content.trim());
        if total + entry.len() > MAX_TRANSCRIPT_CHARS && !lines.is_empty() {
            lines.push("[…older turns truncated…]".to_string());
            break;
        }
        total += entry.len();
        lines.push(entry);
    }
    lines.reverse();
    let transcript_block = if lines.is_empty() {
        "(no prior turns)".to_string()
    } else {
        lines.join("\n")
    };
    format!(
        "Topic: {topic}\n\n--- TRANSCRIPT ---\n{transcript_block}\n--- END ---\n\n\
         Ask the next focused clarifying question. Return ONLY the question."
    )
}

/// Reduce the model output to a single-line question. Strips any "Here is …"
/// preamble, fences, leading list markers, and collapses internal newlines.
fn sanitize_question(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // Drop a single outer fence if the model wrapped its reply.
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim().to_string();
        }
    }
    // First non-empty line is the question; ignore anything after a blank.
    let first = s
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', '*', '-', '·', '>', ' '])
        .trim()
        .to_string();
    first
}

// ----- Save transcript to brain ------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SaveDuckArgs {
    pub topic: String,
    pub transcript: Vec<DuckTurn>,
}

#[derive(Debug, Serialize)]
pub struct SaveDuckResult {
    pub written_path: PathBuf,
    pub bytes: usize,
}

#[tauri::command]
pub async fn save_duck_transcript(args: SaveDuckArgs) -> Result<SaveDuckResult, String> {
    let brain_root = brain_dir().ok_or_else(|| "could not resolve ~/Documents".to_string())?;
    let dir = brain_root.join("duck");
    fs::create_dir_all(&dir).map_err(|e| format!("create duck dir failed: {e}"))?;

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let slug = slugify(&args.topic);
    let filename = format!("{date}-{slug}.md");
    let written_path = dir.join(&filename);

    if !written_path.starts_with(&dir) {
        return Err("refusing to write outside the brain vault".into());
    }

    let body = render_markdown(&args.topic, &args.transcript);
    let bytes = body.as_bytes().len();
    fs::write(&written_path, &body)
        .map_err(|e| format!("write {} failed: {e}", written_path.display()))?;

    Ok(SaveDuckResult { written_path, bytes })
}

fn render_markdown(topic: &str, transcript: &[DuckTurn]) -> String {
    let now_iso = chrono::Utc::now().to_rfc3339();
    let frontmatter = format!(
        "---\nkind: duck-transcript\ntopic: {}\ngenerated_at: {}\nturns: {}\n---\n\n",
        yaml_escape(topic),
        now_iso,
        transcript.len(),
    );
    let mut body = format!("{frontmatter}# Duck: {topic}\n\n");
    for turn in transcript {
        let label = if turn.role == "duck" { "**Duck**" } else { "**You**" };
        body.push_str(&format!("{label}\n\n{}\n\n", turn.content.trim()));
    }
    body
}

fn brain_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Documents").join("Cortex Brain"))
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
    if out.len() > 60 {
        out.truncate(60);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "duck".into()
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
    fn sanitize_strips_fence_and_markers() {
        assert_eq!(sanitize_question("- What's the goal?"), "What's the goal?");
        assert_eq!(sanitize_question("```\nWhy now?\n```"), "Why now?");
        assert_eq!(sanitize_question("   \n\nWhich step failed?\n\nmore"), "Which step failed?");
    }

    #[test]
    fn sanitize_returns_empty_on_blank() {
        assert_eq!(sanitize_question(""), "");
        assert_eq!(sanitize_question("   \n\n  "), "");
    }

    #[test]
    fn build_user_prompt_handles_no_transcript() {
        let p = build_user_prompt("auth bug", &[]);
        assert!(p.contains("Topic: auth bug"));
        assert!(p.contains("(no prior turns)"));
    }

    #[test]
    fn build_user_prompt_truncates_old_turns() {
        let big = "x".repeat(MAX_TRANSCRIPT_CHARS);
        let transcript = vec![
            DuckTurn { role: "user".into(), content: big.clone(), ts_unix_ms: 1 },
            DuckTurn { role: "duck".into(), content: "fresh?".into(), ts_unix_ms: 2 },
        ];
        let p = build_user_prompt("topic", &transcript);
        assert!(p.contains("Duck: fresh?"));
        assert!(p.contains("truncated"));
    }

    #[test]
    fn slugify_falls_back_when_empty() {
        assert_eq!(slugify(""), "duck");
        assert_eq!(slugify("!!!"), "duck");
        assert_eq!(slugify("Why is my SQL slow?"), "why-is-my-sql-slow");
    }

    #[test]
    fn render_markdown_has_frontmatter_and_turns() {
        let t = vec![
            DuckTurn { role: "user".into(), content: "stuck on regex".into(), ts_unix_ms: 1 },
            DuckTurn { role: "duck".into(), content: "what does it match?".into(), ts_unix_ms: 2 },
        ];
        let md = render_markdown("regex woes", &t);
        assert!(md.starts_with("---"));
        assert!(md.contains("# Duck: regex woes"));
        assert!(md.contains("**You**"));
        assert!(md.contains("**Duck**"));
    }
}
