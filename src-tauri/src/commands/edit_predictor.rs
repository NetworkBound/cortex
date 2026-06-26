//! Zed-style edit predictor (next-edit suggestion).
//!
//! After the user makes a meaningful edit (rename, signature tweak, etc.) the
//! frontend ships the changed line's before/after plus the current file body
//! to `predict_next_edit`. We round-trip through the gateway' chat-completion
//! stream and ask the model to spot OTHER spots in the file that should likely
//! receive the same edit (e.g. additional call sites for a renamed function).
//!
//! Distinct from `inline_complete`:
//!   - `inline_complete` predicts at the CURRENT cursor on idle
//!   - `predict_next_edit` predicts ELSEWHERE in the file, triggered by an
//!     actual edit the user just made.
//!
//! Output is a list of `EditSuggestion { line, original, suggested,
//! confidence, reason }`. We filter to `confidence >= 0.7` and cap at 5.
//!
//! Hard wall-clock of 5s; on timeout / parse failure we return an empty list
//! so the editor simply doesn't show any ghost.
//!
//! NOTE: the gateway streaming client emits OpenAI-style chat deltas. The
//! model is prompted to emit a single JSON array; we parse that array out of
//! the collected text (tolerating an optional ```json fenced block).

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

const SYSTEM_PROMPT: &str = "You are an edit-predictor. \
Given a single edit the user just made and the full file body, \
identify OTHER places in the SAME file that should likely receive the same edit \
(e.g. additional call sites of a renamed identifier, parallel signature tweaks). \
Return ONLY a JSON array of objects with the shape \
{\"line\": <1-indexed line number>, \"original\": <existing line text>, \"suggested\": <proposed line text>, \"confidence\": <0..1>, \"reason\": <short string>}. \
Include AT MOST 5 entries, only those with confidence >= 0.7. \
Do not include the line the user just edited. \
If nothing matches, return [].";

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SUGGESTIONS: usize = 5;
const MIN_CONFIDENCE: f32 = 0.7;

#[derive(Debug, Deserialize)]
pub struct RecentEdit {
    /// 1-indexed line the user just edited.
    pub line: u32,
    /// Line text BEFORE the edit.
    pub before: String,
    /// Line text AFTER the edit.
    pub after: String,
}

#[derive(Debug, Deserialize)]
pub struct PredictNextEditArgs {
    /// File path — informational; lets the prompt steer the model on language.
    #[serde(default)]
    pub path: Option<String>,
    pub recent_edit: RecentEdit,
    /// Full file body POST-edit. We trim to a sane cap before shipping.
    pub file_body: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EditSuggestion {
    pub line: u32,
    pub original: String,
    pub suggested: String,
    pub confidence: f32,
    #[serde(default)]
    pub reason: String,
}

#[tauri::command]
pub async fn predict_next_edit(
    args: PredictNextEditArgs,
    state: State<'_, AppState>,
) -> Result<Vec<EditSuggestion>, String> {
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
        temperature: Some(0.1),
    };

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
            tracing::debug!(
                "predict_next_edit: timed out after {}ms",
                started.elapsed().as_millis()
            );
            return Ok(Vec::new());
        }
    };

    let suggestions = parse_suggestions(&collected)
        .into_iter()
        .filter(|s| s.confidence >= MIN_CONFIDENCE)
        .filter(|s| s.line != args.recent_edit.line)
        .take(MAX_SUGGESTIONS)
        .collect();
    Ok(suggestions)
}

fn build_user_prompt(args: &PredictNextEditArgs) -> String {
    // Cap the body we ship so a giant file doesn't blow the prompt window.
    // Trim from the start, keep the tail — for most refactors the edit lives
    // in the upper portion of the file and the call sites trail after; but
    // either way 16k chars is enough context for the model to find peers.
    const MAX_BODY: usize = 16_000;
    let body = if args.file_body.len() > MAX_BODY {
        tail_on_char_boundary(&args.file_body, MAX_BODY)
    } else {
        args.file_body.as_str()
    };
    let lang_hint = args
        .path
        .as_deref()
        .and_then(|p| p.rsplit('.').next())
        .unwrap_or("");
    format!(
        "Path: {path}\nLanguage hint: {lang}\n\
         --- USER JUST EDITED LINE {line} ---\n\
         BEFORE: {before}\n\
         AFTER:  {after}\n\
         --- FILE BODY (current) ---\n{body}\n\
         --- RESPONSE (JSON array only) ---\n",
        path = args.path.as_deref().unwrap_or(""),
        lang = lang_hint,
        line = args.recent_edit.line,
        before = args.recent_edit.before,
        after = args.recent_edit.after,
        body = body,
    )
}

/// Return the last `max_bytes`-ish bytes of `s` without splitting a UTF-8
/// codepoint. We aim to keep `max_bytes` of the tail, then walk the start
/// index forward until it lands on a char boundary (dropping at most 3 bytes),
/// so slicing can never panic mid-codepoint.
fn tail_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Extract the JSON array from a model reply. Tolerates a leading "Here is
/// the JSON:" preamble and an optional ```json fenced block.
fn parse_suggestions(raw: &str) -> Vec<EditSuggestion> {
    let trimmed = raw.trim();
    // Try whole-string parse first.
    if let Ok(v) = serde_json::from_str::<Vec<EditSuggestion>>(trimmed) {
        return v;
    }
    // Fenced block?
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            let inner = after_lang[..end].trim();
            if let Ok(v) = serde_json::from_str::<Vec<EditSuggestion>>(inner) {
                return v;
            }
        }
    }
    // Fall back: locate the outermost `[...]` slice.
    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if end > start {
            let slice = &trimmed[start..=end];
            if let Ok(v) = serde_json::from_str::<Vec<EditSuggestion>>(slice) {
                return v;
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_json_array() {
        let raw = r#"[{"line":42,"original":"foo","suggested":"bar","confidence":0.9,"reason":"rename"}]"#;
        let v = parse_suggestions(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].line, 42);
        assert_eq!(v[0].suggested, "bar");
    }

    #[test]
    fn parse_fenced_json() {
        let raw = "```json\n[{\"line\":7,\"original\":\"a\",\"suggested\":\"b\",\"confidence\":0.8,\"reason\":\"\"}]\n```";
        let v = parse_suggestions(raw);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].confidence, 0.8);
    }

    #[test]
    fn parse_with_preamble() {
        let raw = "Here is the JSON:\n[{\"line\":3,\"original\":\"x\",\"suggested\":\"y\",\"confidence\":0.75,\"reason\":\"\"}]";
        let v = parse_suggestions(raw);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn parse_garbage_returns_empty() {
        assert!(parse_suggestions("not json").is_empty());
    }

    #[test]
    fn tail_on_char_boundary_never_panics_mid_utf8() {
        // A string of multibyte chars; pick a max_bytes that would land
        // mid-codepoint under a naive slice.
        let s = "é".repeat(10_000); // each 'é' is 2 bytes
        let tail = tail_on_char_boundary(&s, 16_001);
        // Result is valid UTF-8 (didn't panic) and bounded by the cap.
        assert!(tail.len() <= 16_001);
        assert!(s.ends_with(tail));
        // Short strings pass through untouched.
        assert_eq!(tail_on_char_boundary("hi", 16_000), "hi");
    }

    #[test]
    fn build_user_prompt_handles_large_multibyte_body() {
        let args = PredictNextEditArgs {
            path: Some("src/foo.rs".into()),
            recent_edit: RecentEdit {
                line: 1,
                before: "a".into(),
                after: "b".into(),
            },
            // Over the 16k cap, all multibyte so the trim point is mid-char.
            file_body: "é".repeat(20_000),
        };
        // Must not panic.
        let _ = build_user_prompt(&args);
    }

    #[test]
    fn build_user_prompt_includes_edit() {
        let args = PredictNextEditArgs {
            path: Some("src/foo.ts".into()),
            recent_edit: RecentEdit {
                line: 12,
                before: "getUserName()".into(),
                after: "userName()".into(),
            },
            file_body: "line1\nline2\n".into(),
        };
        let p = build_user_prompt(&args);
        assert!(p.contains("LINE 12"));
        assert!(p.contains("getUserName()"));
        assert!(p.contains("userName()"));
        assert!(p.contains("Language hint: ts"));
    }
}
