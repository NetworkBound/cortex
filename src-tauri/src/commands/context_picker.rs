//! Smart-context auto-picker.
//!
//! Backs the `/suggest-context` (alias `/ctx`) slash and the "🎯 Suggest
//! context" button in the composer. Given the user's draft message and the
//! active project root, asks the gateway which `@`-tokens the user should attach
//! before sending.
//!
//! Candidate context is gathered locally:
//!   - File list via `projects::list_files` (capped at 1000 entries).
//!   - Recent memory entries via `memory::sources::default_sources`
//!     walked through `walk_markdown` (top 50).
//!   - Recent traces via the `TracingStore` (top 20).
//!
//! Same streaming-collect + timeout pattern as `ask_router` / `commit_suggest`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::memory::sources;
use crate::observability::tracing_store::TracingStore;
use crate::projects::list_files;

/// Wall-clock cap on the gateway call. The response is a small JSON array,
/// so 5s is comfortable while still keeping the UI snappy.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Hard cap on the candidate-context block we ship to the model. 8 KiB fits
/// roughly: top 80 files (~50 chars each), 50 memory titles, and 20 trace
/// summaries — enough breadth without ballooning latency.
const INPUT_LIMIT_BYTES: usize = 8 * 1024;

/// Cap on the user's draft message — anything past this is unlikely to add
/// signal to the picker.
const MESSAGE_LIMIT_BYTES: usize = 4 * 1024;

/// Cap on how many files we enumerate before slicing for the prompt.
const FILE_SCAN_CAP: usize = 1000;

/// How many memory entries we send to the model.
const MEMORY_PROMPT_CAP: usize = 50;

/// How many recent traces we send.
const TRACE_PROMPT_CAP: usize = 20;

/// How many file paths we send. Files are the bulk of candidates; keep this
/// generous but still cropped so the prompt fits the byte budget.
const FILE_PROMPT_CAP: usize = 80;

/// Truncate `s` to at most `max_bytes`, backing up to the nearest UTF-8 char
/// boundary so we never slice through a multi-byte codepoint. Slicing a
/// `&str` at an arbitrary byte offset panics; user drafts and file contents
/// routinely contain emoji/CJK, so the fixed-offset slices this replaced could
/// crash the command thread.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

const SYSTEM_PROMPT: &str = "You are the smart-context picker for Cortex, a \
desktop AI chat app. The user is about to send a message and would benefit \
from attaching the most relevant `@`-tokens (files, memory entries, recent \
traces, the working diff, or compile problems). Inspect the user's draft and \
the candidate context below, then recommend the top 5-8 attachments with \
confidence >= 0.5. Output ONLY a JSON array. Each element MUST have these \
exact keys: {\"kind\": \"file\"|\"memory\"|\"recent\"|\"diff\"|\"problems\", \
\"value\": \"<path or id; empty string for diff/problems>\", \
\"reason\": \"<one short sentence>\", \"confidence\": <0.0-1.0>}. Use exactly \
the file paths and memory paths shown — never invent. Do not wrap the JSON \
in code fences or add any other text.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSuggestion {
    /// One of: `file`, `memory`, `recent`, `diff`, `problems`.
    pub kind: String,
    /// Path / id payload. May be empty for `diff` and `problems`.
    pub value: String,
    /// One-line rationale shown beside the chip.
    pub reason: String,
    /// Model-reported confidence in `[0.0, 1.0]`.
    pub confidence: f32,
}

#[derive(Debug, Serialize)]
pub struct ContextSuggestions {
    pub suggestions: Vec<ContextSuggestion>,
}

#[tauri::command]
pub async fn suggest_context(
    message: String,
    project_root: Option<String>,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<ContextSuggestions, String> {
    let draft = message.trim();
    if draft.is_empty() {
        return Err("suggest_context: empty message".into());
    }
    let draft = truncate_on_char_boundary(draft, MESSAGE_LIMIT_BYTES);

    let root_path = project_root.as_deref().map(PathBuf::from);
    let candidate_block = build_candidate_block(root_path.as_deref(), &store);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = format!(
        "USER DRAFT MESSAGE:\n{draft}\n\n--- CANDIDATE CONTEXT ---\n{candidate_block}--- END ---\n\n\
         Respond with the JSON array only.",
    );

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.2),
    };

    let raw = run_with_timeout(client, req).await?;
    let suggestions = parse_suggestions(&raw)?;
    Ok(ContextSuggestions { suggestions })
}

/// Build the `--- CANDIDATE CONTEXT ---` block. We trickle each section in
/// (files, memory, traces) and stop once we hit the byte budget so we don't
/// silently drop the most recent traces just because there were many files.
fn build_candidate_block(project_root: Option<&std::path::Path>, store: &TracingStore) -> String {
    let mut buf = String::with_capacity(2048);

    if let Some(root) = project_root {
        if root.is_dir() {
            let entries = list_files(root, FILE_SCAN_CAP);
            let files: Vec<String> = entries
                .into_iter()
                .filter(|e| !e.is_dir)
                .take(FILE_PROMPT_CAP)
                .map(|e| {
                    e.path
                        .strip_prefix(root)
                        .unwrap_or(&e.path)
                        .display()
                        .to_string()
                })
                .collect();
            if !files.is_empty() {
                push_section(&mut buf, "Files (relative to project root):", &files);
            }
        }
    }

    let memory_titles = collect_memory_titles(project_root);
    if !memory_titles.is_empty() {
        push_section(&mut buf, "Memory entries (path — first line):", &memory_titles);
    }

    let traces = store.recent_traces(TRACE_PROMPT_CAP).unwrap_or_default();
    if !traces.is_empty() {
        let lines: Vec<String> = traces
            .into_iter()
            .map(|t| {
                let span_label = t
                    .spans
                    .first()
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "(no span)".into());
                format!("{} — session {} — {}", t.trace_id, t.session_id, span_label)
            })
            .collect();
        push_section(&mut buf, "Recent traces (id — session — first span):", &lines);
    }

    if buf.is_empty() {
        buf.push_str("(no candidate context available)\n");
    }

    buf
}

/// Walk the active-project + Obsidian-vault memory roots and return at most
/// `MEMORY_PROMPT_CAP` `"<relative path> — <first non-empty line>"` strings.
fn collect_memory_titles(project_root: Option<&std::path::Path>) -> Vec<String> {
    // Note: we deliberately don't read the obsidian vault from `AppState` here
    // because `suggest_context` already runs on a `State<AppState>` borrow and
    // `default_sources` will accept `None` to skip the vault.
    let srcs = sources::default_sources(project_root, None);
    let mut out: Vec<String> = Vec::new();
    for src in &srcs {
        for p in sources::walk_markdown(src) {
            if out.len() >= MEMORY_PROMPT_CAP {
                return out;
            }
            let rel = p.display().to_string();
            let first_line = std::fs::read_to_string(&p)
                .ok()
                .and_then(|body| {
                    body.lines()
                        .map(|l| l.trim().to_string())
                        .find(|l| !l.is_empty())
                })
                .unwrap_or_default();
            let first_line = if first_line.len() > 120 {
                format!("{}…", truncate_on_char_boundary(&first_line, 120))
            } else {
                first_line
            };
            out.push(format!("{rel} — {first_line}"));
        }
    }
    out
}

/// Append a labelled section to `buf`, halting early once we cross the byte
/// budget so later sections still get a chance to land at least a few lines.
fn push_section(buf: &mut String, label: &str, items: &[String]) {
    buf.push_str(label);
    buf.push('\n');
    for item in items {
        if buf.len() >= INPUT_LIMIT_BYTES {
            buf.push_str("…[truncated]\n");
            return;
        }
        buf.push_str("- ");
        buf.push_str(item);
        buf.push('\n');
    }
    buf.push('\n');
}

async fn run_with_timeout(
    client: GatewayClient,
    req: ChatCompletionRequest,
) -> Result<String, String> {
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
        Err(_) => Err("suggest_context: The gateway timed out".into()),
    }
}

/// Strip code fences, locate the JSON array, parse + normalise. Same defensive
/// approach as `ask_router::parse_router_json`.
fn parse_suggestions(raw: &str) -> Result<Vec<ContextSuggestion>, String> {
    let stripped = strip_fences(raw);
    let blob = extract_json_array(&stripped).ok_or_else(|| {
        format!("suggest_context: no JSON array in model output (raw={raw})")
    })?;
    let parsed: Vec<RawSuggestion> = serde_json::from_str(blob)
        .map_err(|e| format!("suggest_context: invalid JSON: {e} (raw={raw})"))?;

    let mut out: Vec<ContextSuggestion> = Vec::new();
    for r in parsed {
        let kind = r.kind.unwrap_or_default().trim().to_lowercase();
        if !is_valid_kind(&kind) {
            continue;
        }
        let value = r.value.unwrap_or_default().trim().to_string();
        // `diff` / `problems` don't need a payload; everything else does.
        if value.is_empty() && kind != "diff" && kind != "problems" {
            continue;
        }
        let confidence = r.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
        if confidence < 0.5 {
            continue;
        }
        let reason = r
            .reason
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no reason provided)".into());
        out.push(ContextSuggestion { kind, value, reason, confidence });
        if out.len() >= 8 {
            break;
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct RawSuggestion {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

fn is_valid_kind(k: &str) -> bool {
    matches!(k, "file" | "memory" | "recent" | "diff" | "problems")
}

fn strip_fences(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }
    s.trim().to_string()
}

fn extract_json_array(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_suggestions_filters_low_confidence() {
        let raw = r#"[
            {"kind":"file","value":"src/lib.rs","reason":"core","confidence":0.9},
            {"kind":"file","value":"README.md","reason":"meh","confidence":0.3}
        ]"#;
        let out = parse_suggestions(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, "src/lib.rs");
    }

    #[test]
    fn parse_suggestions_drops_unknown_kind() {
        let raw = r#"[{"kind":"teleport","value":"x","reason":"r","confidence":0.99}]"#;
        let out = parse_suggestions(raw).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn parse_suggestions_accepts_empty_value_for_diff() {
        let raw = r#"[{"kind":"diff","value":"","reason":"working changes","confidence":0.8}]"#;
        let out = parse_suggestions(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "diff");
    }

    #[test]
    fn parse_suggestions_handles_code_fences() {
        let raw = "```json\n[{\"kind\":\"problems\",\"value\":\"\",\"reason\":\"errors present\",\"confidence\":0.7}]\n```";
        let out = parse_suggestions(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "problems");
    }

    #[test]
    fn parse_suggestions_caps_at_eight() {
        let entries: Vec<String> = (0..15)
            .map(|i| {
                format!(
                    "{{\"kind\":\"file\",\"value\":\"f{i}.rs\",\"reason\":\"r\",\"confidence\":0.9}}"
                )
            })
            .collect();
        let raw = format!("[{}]", entries.join(","));
        let out = parse_suggestions(&raw).unwrap();
        assert_eq!(out.len(), 8);
    }

    #[test]
    fn truncate_on_char_boundary_never_splits_codepoint() {
        // A string of 2-byte chars; cutting at an odd byte limit must back up
        // to the previous boundary rather than panic (the bug this guards).
        let s = "ééééé"; // 5 × 2 bytes = 10 bytes
        let out = truncate_on_char_boundary(s, 5); // 5 lands mid-codepoint
        assert!(out.len() <= 5);
        assert!(s.is_char_boundary(out.len()));
        assert_eq!(out, "éé"); // backed up to 4 bytes
        // Emoji (4-byte) at a tight limit.
        let emoji = "😀abc";
        assert_eq!(truncate_on_char_boundary(emoji, 2), "");
        // No truncation when under the cap.
        assert_eq!(truncate_on_char_boundary("hi", 99), "hi");
    }
}
