//! AI changelog generator.
//!
//! Reads recent git commits from `project_root` (defaults to "2 weeks ago"),
//! caps the blob at 32 KiB, and asks the gateway for a Keep-a-Changelog-style
//! markdown document grouped by Added/Changed/Fixed/Deprecated/Removed/
//! Security. Mirrors the streaming-collect + timeout pattern from
//! [`super::doc_gen`] / [`super::commit_suggest`].
//!
//! The user-facing flow is `/changelog [since]` → portal modal. The frontend
//! handles "Copy as markdown" and "Save to CHANGELOG.md" itself (via the
//! existing `save_file_text` command); this backend is read-only.

use serde::Serialize;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Hard cap on the commit blob we ship to the model. 32 KiB keeps two weeks
/// of typical activity well under context limits without hitting the
/// per-request gateway timeout.
const COMMIT_LIMIT_BYTES: usize = 32 * 1024;

/// 45s wall clock — changelog generation tends to be heavier than commit-msg
/// suggestion because the model has to bucket every entry into Keep-a-
/// Changelog sections.
const TIMEOUT: Duration = Duration::from_secs(45);

/// How many commits to ask `git log` for. Bounded so a noisy repo doesn't
/// blow the 32 KiB cap before truncation kicks in.
const MAX_COMMITS: usize = 50;

const SYSTEM_PROMPT: &str = "You are a changelog generator. Given these commits, \
produce a Keep-a-Changelog-style markdown changelog grouped by Added/Changed/Fixed/\
Deprecated/Removed/Security. Return ONLY the markdown.";

#[derive(Debug, Serialize, Clone)]
pub struct ChangelogResult {
    pub since: String,
    pub markdown: String,
    pub commit_count: usize,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn generate_changelog(
    project_root: String,
    since: Option<String>,
    state: State<'_, AppState>,
) -> Result<ChangelogResult, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    if !root.join(".git").exists() {
        return Err(format!("not a git repo: {project_root}"));
    }

    let since_arg = since
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "2 weeks ago".to_string());

    let (raw_commits, commit_count) = read_git_log(&root, &since_arg)?;
    if commit_count == 0 {
        return Err(format!("no commits in range (since={since_arg})"));
    }

    let body = truncate(raw_commits, COMMIT_LIMIT_BYTES);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = build_user_prompt(&since_arg, commit_count, &body);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.2),
    };

    let raw_out = run_with_timeout(client, req).await?;
    let markdown = sanitize(&raw_out);
    if markdown.trim().is_empty() {
        return Err("The gateway returned an empty changelog".into());
    }

    Ok(ChangelogResult {
        since: since_arg,
        markdown,
        commit_count,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
}

/// Shell out to `git log` with our pinned format. Returns `(body, count)`.
fn read_git_log(root: &PathBuf, since: &str) -> Result<(String, usize), String> {
    let max_arg = format!("-{}", MAX_COMMITS);
    let since_arg = format!("--since={since}");
    let out = crate::sys::no_window("git")
        .args([
            "log",
            "--pretty=format:%H|%h|%s|%b",
            "--no-color",
            max_arg.as_str(),
            since_arg.as_str(),
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git log failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git log exited non-zero: {err}"));
    }
    let body = String::from_utf8_lossy(&out.stdout).to_string();
    // Each `%H|...|%b` block may itself contain newlines (commit body). We
    // count by counting full SHA prefixes (40 hex chars at the start of a
    // line) rather than `\n` — overestimates would inflate `commit_count`.
    let count = body
        .lines()
        .filter(|l| {
            l.len() >= 41
                && l.as_bytes()[40] == b'|'
                && l[..40].chars().all(|c| c.is_ascii_hexdigit())
        })
        .count();
    Ok((body, count))
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
            "changelog generator timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

fn build_user_prompt(since: &str, count: usize, body: &str) -> String {
    format!(
        "Since: {since}\nCommit count: {count}\n\n\
         --- COMMITS (format: full_sha|short_sha|subject|body) ---\n{body}\n--- END COMMITS ---\n\n\
         Return ONLY a Keep-a-Changelog markdown document. Use H2 (`## Added`, \
         `## Changed`, …) for the six standard sections. Omit empty sections."
    )
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
    s.push_str("\n[truncated — commit log exceeded 32 KiB]");
    s
}

/// Strip outer fenced ``` blocks and "Here is …" preamble the model may emit
/// despite the prompt. Conservative — only peels one wrapper.
fn sanitize(raw: &str) -> String {
    let mut s = raw.trim_end_matches('\n').to_string();
    let trimmed = s.trim_start();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }
    if let Some(rest) = s.strip_prefix("Here is") {
        if let Some(idx) = rest.find('\n') {
            s = rest[idx + 1..].trim_start().to_string();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_caps_long_blobs() {
        let blob = "x".repeat(COMMIT_LIMIT_BYTES + 200);
        let out = truncate(blob, COMMIT_LIMIT_BYTES);
        assert!(out.contains("[truncated"));
        assert!(out.len() < COMMIT_LIMIT_BYTES + 100);
    }

    #[test]
    fn sanitize_strips_fenced_block() {
        let raw = "```markdown\n## Added\n- thing\n```";
        assert_eq!(sanitize(raw), "## Added\n- thing");
    }

    #[test]
    fn sanitize_strips_here_is_preamble() {
        let raw = "Here is the changelog:\n## Added\n- thing";
        assert_eq!(sanitize(raw), "## Added\n- thing");
    }

    #[test]
    fn sanitize_passes_through_plain_md() {
        let raw = "## Added\n- thing\n";
        assert_eq!(sanitize(raw), "## Added\n- thing");
    }

    #[test]
    fn build_user_prompt_includes_metadata() {
        let p = build_user_prompt("2 weeks ago", 3, "abc|abc|fix|");
        assert!(p.contains("Since: 2 weeks ago"));
        assert!(p.contains("Commit count: 3"));
        assert!(p.contains("abc|abc|fix|"));
    }
}
