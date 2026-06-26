//! AI-guided git staging.
//!
//! `/stage <intent>` asks the model to pick which files to `git add` based on
//! the working diff + the user's high-level intent (e.g. "just the snippets
//! backend changes, not the UI"). We collect `git status --porcelain -uall`
//! and `git diff` (unstaged) from `project_root`, cap each at 32 KiB, then
//! round-trip through the gateway — same client + streaming-collect pattern as
//! [`super::inline_completion`] / [`super::commit_suggest`].
//!
//! The model returns a JSON object `{ stage, skip, reason }`; we run `git add`
//! for each path it picked (silently skipping `add` failures so a typoed path
//! doesn't poison the whole batch).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Per-blob cap. 32 KiB is roomy enough for most working diffs without
/// blowing out the model's context window.
const BLOB_LIMIT_BYTES: usize = 32 * 1024;

/// Wall-clock cap on the whole gateway call.
const TIMEOUT: Duration = Duration::from_secs(30);

const SYSTEM_PROMPT: &str = "You are a smart git stager. \
Given the working diff and the user's intent below, return a JSON object: \
`{ stage: [<file paths>], skip: [<file paths>], reason: '<one line explanation>' }`. \
Be conservative — only stage files clearly matching the intent. \
Output ONLY the JSON object, no fences, no preamble.";

#[derive(Debug, Default, Deserialize)]
struct ModelPick {
    #[serde(default)]
    stage: Vec<String>,
    #[serde(default)]
    skip: Vec<String>,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct SmartStageReport {
    pub staged: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    pub reason: String,
}

#[tauri::command]
pub async fn smart_stage(
    project_root: String,
    intent: String,
    state: State<'_, AppState>,
) -> Result<SmartStageReport, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    if intent.trim().is_empty() {
        return Err("intent is empty — say what you want to stage".into());
    }

    let status = read_git(&root, &["status", "--porcelain", "-uall"]);
    let diff = read_git(&root, &["diff", "--no-color"]);
    if status.trim().is_empty() && diff.trim().is_empty() {
        return Ok(SmartStageReport {
            reason: "nothing to stage — working tree is clean".into(),
            ..Default::default()
        });
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = build_user_prompt(&intent, &status, &diff);

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
        Err(_) => return Err("The gateway timed out picking files to stage".into()),
    };

    let pick = parse_pick(&collected);
    let mut report = SmartStageReport {
        skipped: pick.skip.clone(),
        reason: if pick.reason.trim().is_empty() {
            "no reason given".into()
        } else {
            pick.reason
        },
        ..Default::default()
    };

    for path in pick.stage {
        match run_git_add(&root, &path) {
            Ok(()) => report.staged.push(path),
            Err(e) => report.errors.push(format!("{path}: {e}")),
        }
    }

    Ok(report)
}

fn build_user_prompt(intent: &str, status: &str, diff: &str) -> String {
    format!(
        "Intent: {intent}\n\
         --- git status --porcelain -uall ---\n{status}\n\
         --- git diff (unstaged) ---\n{diff}\n",
    )
}

/// Run a `git` subcommand and return stdout as UTF-8. Returns an empty string
/// on any failure (matches `commit_suggest::run_diff` semantics — callers
/// downgrade gracefully if the repo isn't a git checkout).
fn read_git(root: &PathBuf, args: &[&str]) -> String {
    let out = match crate::sys::no_window("git").args(args).current_dir(root).output() {
        Ok(o) => o,
        Err(_) => return String::new(),
    };
    if !out.status.success() {
        return String::new();
    }
    truncate(String::from_utf8_lossy(&out.stdout).into_owned(), BLOB_LIMIT_BYTES)
}

fn run_git_add(root: &PathBuf, path: &str) -> Result<(), String> {
    let out = crate::sys::no_window("git")
        .args(["add", "--", path])
        .current_dir(root)
        .output()
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

fn truncate(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[truncated]");
    s
}

/// Pull the first balanced `{...}` block out of the model's reply and parse
/// it as JSON. Falls back to a parse-failed sentinel so the frontend always
/// gets a structured response.
fn parse_pick(raw: &str) -> ModelPick {
    let trimmed = raw.trim();
    let candidate = extract_json_object(trimmed).unwrap_or(trimmed.to_string());
    serde_json::from_str::<ModelPick>(&candidate).unwrap_or_else(|_| ModelPick {
        reason: "parse failed".into(),
        ..Default::default()
    })
}

/// Find the first balanced `{...}` substring. We can't just `find('{')` …
/// `rfind('}')` because the model sometimes appends commentary that includes
/// extra braces — instead we walk the string and stop at the matching close.
fn extract_json_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pick_extracts_json_with_preamble() {
        let raw = "Sure! Here's the picks:\n{\"stage\":[\"a.rs\"],\"skip\":[\"b.ts\"],\"reason\":\"backend only\"}\nDone.";
        let p = parse_pick(raw);
        assert_eq!(p.stage, vec!["a.rs"]);
        assert_eq!(p.skip, vec!["b.ts"]);
        assert_eq!(p.reason, "backend only");
    }

    #[test]
    fn parse_pick_handles_malformed() {
        let p = parse_pick("not json at all");
        assert_eq!(p.reason, "parse failed");
        assert!(p.stage.is_empty());
    }

    #[test]
    fn parse_pick_ignores_braces_inside_strings() {
        let raw = r#"{"stage":[],"skip":[],"reason":"contains } char"}"#;
        let p = parse_pick(raw);
        assert_eq!(p.reason, "contains } char");
    }

    #[test]
    fn truncate_caps_long_blob() {
        let blob = "x".repeat(BLOB_LIMIT_BYTES + 100);
        let out = truncate(blob, BLOB_LIMIT_BYTES);
        assert!(out.ends_with("[truncated]"));
    }

    #[test]
    fn build_user_prompt_includes_all_sections() {
        let p = build_user_prompt("just docs", "M foo.md", "diff body");
        assert!(p.contains("Intent: just docs"));
        assert!(p.contains("M foo.md"));
        assert!(p.contains("diff body"));
    }
}
