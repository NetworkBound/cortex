//! AI-powered commit message suggester.
//!
//! Reads the staged diff (`git diff --cached`) from `project_root`, falling
//! back to the unstaged diff when nothing is staged. The diff is truncated to
//! 16 KiB and piped through the gateway — same client + streaming-collect pattern
//! as [`super::inline_completion`] — with a system prompt that asks for a
//! Conventional Commits-style message. The frontend `/commit-msg` slash
//! command copies the result to the clipboard.

use std::path::PathBuf;
use std::time::Duration;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Hard cap on the diff blob sent to the model. 16 KiB is enough to summarise
/// most reasonable commits without ballooning latency or context usage.
const DIFF_LIMIT_BYTES: usize = 16 * 1024;

/// Wall-clock cap on the whole gateway call. Keeps `/commit-msg` snappy even
/// when the gateway is stuck — a timeout surfaces as a user-facing error.
const TIMEOUT: Duration = Duration::from_secs(20);

const SYSTEM_PROMPT: &str = "You are a commit message generator. \
Given the diff below, write a Conventional Commits-style message \
(subject + optional 2-3 line body). \
Output ONLY the message, no fences, no preamble.";

#[tauri::command]
pub async fn suggest_commit_message(
    project_root: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let diff = read_diff(&root)?;
    if diff.trim().is_empty() {
        return Err("no changes to summarise (staged or unstaged)".into());
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = format!(
        "Write a Conventional Commits message for the following diff:\n\n```diff\n{diff}\n```",
    );

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.3),
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
        Err(_) => return Err("The gateway timed out generating commit message".into()),
    };

    let message = sanitize(&collected);
    if message.trim().is_empty() {
        return Err("The gateway returned an empty commit message".into());
    }
    Ok(message)
}

/// Pull the staged diff first; fall back to unstaged when index is empty. We
/// shell out to `git` directly to keep this independent of `crate::git`
/// (which is index-aware but doesn't expose a raw diff helper today).
fn read_diff(root: &PathBuf) -> Result<String, String> {
    let staged = run_diff(root, &["diff", "--cached", "--no-color"])?;
    let raw = if staged.trim().is_empty() {
        run_diff(root, &["diff", "--no-color"])?
    } else {
        staged
    };
    Ok(truncate(raw, DIFF_LIMIT_BYTES))
}

fn run_diff(root: &PathBuf, args: &[&str]) -> Result<String, String> {
    let output = crate::sys::no_window("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: spawn failed: {e}"))?;
    if !output.status.success() {
        // Not a repo / git missing — treat as no diff so callers can fall
        // back gracefully (matches `context::git_working_diff` semantics).
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
    s.push_str("\n[truncated — diff exceeded 16 KiB]");
    s
}

/// Strip common chat-model preambles and fenced code blocks. Reuses the
/// approach from `inline_completion::sanitize` so the two endpoints behave
/// identically when the model ignores the "no fences" instruction.
fn sanitize(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }
    // Drop a common "Here is …" preamble line if present.
    if let Some(rest) = s.strip_prefix("Here is") {
        if let Some(idx) = rest.find('\n') {
            s = rest[idx + 1..].trim_start().to_string();
        }
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_caps_long_diff() {
        let blob = "x".repeat(DIFF_LIMIT_BYTES + 500);
        let out = truncate(blob, DIFF_LIMIT_BYTES);
        assert!(out.contains("[truncated"));
        assert!(out.len() < DIFF_LIMIT_BYTES + 200);
    }

    #[test]
    fn truncate_leaves_short_diff_alone() {
        let blob = "diff --git a/x b/x\n+hi\n".to_string();
        let out = truncate(blob.clone(), DIFF_LIMIT_BYTES);
        assert_eq!(out, blob);
    }

    #[test]
    fn sanitize_strips_fenced_block() {
        let raw = "```\nfeat: add widget\n\nBody.\n```";
        assert_eq!(sanitize(raw), "feat: add widget\n\nBody.");
    }

    #[test]
    fn sanitize_passes_through_plain_message() {
        let raw = "fix: handle nil pointer\n\nDetails here.";
        assert_eq!(sanitize(raw), raw);
    }
}
