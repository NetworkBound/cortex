//! Reads Claude Code chat-session `.jsonl` files from `~/.claude/projects/*/`
//! and the global `~/.claude/history.jsonl`. Each chat is parsed into a
//! compact `ChatSummary` (for listings) or a `ChatTranscript` (full message
//! list, for the detail view).
//!
//! The on-disk format uses one JSON object per line. We only surface
//! human-readable user/assistant turns; metadata events (file snapshots,
//! permission-mode toggles, deferred-tool deltas, etc.) are skipped.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize)]
pub struct ChatSummary {
    pub file_path: String,
    pub session_id: String,
    pub project: Option<String>,
    pub project_root: Option<String>,
    pub first_message: Option<String>,
    pub message_count: usize,
    pub modified_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
    pub ts_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatTranscript {
    pub file_path: String,
    pub session_id: String,
    pub project_root: Option<String>,
    pub turns: Vec<ChatTurn>,
}

#[derive(Debug, Deserialize)]
struct Row {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<RowMessage>,
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RowMessage {
    role: Option<String>,
    content: Option<serde_json::Value>,
}

fn ts_to_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn content_to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            // anthropic-style content blocks: [{type:"text", text:"…"}, ...]
            let mut out = String::new();
            for block in arr {
                if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                    if !out.is_empty() { out.push('\n'); }
                    out.push_str(t);
                } else if let Some(t) = block.get("content").and_then(|x| x.as_str()) {
                    if !out.is_empty() { out.push('\n'); }
                    out.push_str(t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn project_label_from_dir(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_string_lossy().to_string();
    // Claude Code encodes project paths as `-home-foo-bar` — turn back into
    // `/home/foo/bar` for display.
    if name.starts_with('-') {
        Some(name.replace('-', "/"))
    } else {
        Some(name)
    }
}

fn modified_ms(p: &Path) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// List every `*.jsonl` chat file under `~/.claude/projects/*/` plus the
/// global `~/.claude/history.jsonl`, parsed into `ChatSummary` rows sorted
/// newest first.
pub fn list_chats() -> Vec<ChatSummary> {
    // Scan every reachable home. On Windows that includes the WSL UNC
    // paths so cortex.exe sees `.jsonl` chats Claude Code wrote on the
    // WSL side. user's screen showed "Chats: 1" because the actual
    // chats live under `\\wsl.localhost\Ubuntu\home\user\.claude\`.
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(h) = dirs::home_dir() { homes.push(h); }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME")
            .ok()
            .map(|u| u.to_lowercase())
            .unwrap_or_else(|| "user".to_string());
        for distro in ["Ubuntu", "Ubuntu-24.04", "Ubuntu-22.04", "Debian"] {
            let p = PathBuf::from(format!("\\\\wsl.localhost\\{distro}\\home\\{user}"));
            if p.exists() && !homes.iter().any(|h| h == &p) {
                homes.push(p);
            }
        }
    }

    let mut out: Vec<ChatSummary> = Vec::new();
    let mut seen_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for home in &homes {
        let projects = home.join(".claude").join("projects");
        if projects.exists() {
            for project_entry in fs::read_dir(&projects).into_iter().flatten().flatten() {
                let project_dir = project_entry.path();
                if !project_dir.is_dir() { continue; }
                let project_label = project_label_from_dir(&project_dir);
                for entry in fs::read_dir(&project_dir).into_iter().flatten().flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("jsonl") { continue; }
                    if !seen_paths.insert(p.clone()) { continue; }
                    if let Some(summary) = summarize_file(&p, project_label.clone()) {
                        out.push(summary);
                    }
                }
            }
        }

        let history = home.join(".claude").join("history.jsonl");
        if history.exists() && seen_paths.insert(history.clone()) {
            if let Some(summary) = summarize_file(&history, Some("(global history)".into())) {
                out.push(summary);
            }
        }
    }

    out.sort_by(|a, b| b.modified_unix_ms.cmp(&a.modified_unix_ms));
    out
}

fn summarize_file(path: &Path, project_label: Option<String>) -> Option<ChatSummary> {
    let f = fs::File::open(path).ok()?;
    let reader = BufReader::new(f);
    let mut count = 0usize;
    let mut first_msg: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut project_root: Option<String> = None;

    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() { continue; }
        let Ok(row) = serde_json::from_str::<Row>(&line) else { continue };
        if session_id.is_none() {
            session_id = row.session_id.clone();
        }
        if project_root.is_none() {
            project_root = row.cwd.clone();
        }
        let Some(k) = row.kind.as_deref() else { continue };
        if k != "user" && k != "assistant" { continue; }
        let Some(msg) = row.message else { continue };
        let role = msg.role.as_deref().unwrap_or("");
        if role != "user" && role != "assistant" { continue; }
        count += 1;
        if first_msg.is_none() && role == "user" {
            if let Some(c) = msg.content.as_ref() {
                let text = content_to_text(c);
                let trimmed: String = text.trim().chars().take(160).collect();
                if !trimmed.is_empty() {
                    first_msg = Some(trimmed);
                }
            }
        }
    }

    let session_id = session_id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    Some(ChatSummary {
        file_path: path.display().to_string(),
        session_id,
        project: project_label,
        project_root,
        first_message: first_msg,
        message_count: count,
        modified_unix_ms: modified_ms(path),
    })
}

/// Parse a single `.jsonl` chat into its user/assistant turns. Caps content
/// length per turn to keep the response sane for the UI.
pub fn read_chat(path: &Path, max_turns: usize) -> std::io::Result<ChatTranscript> {
    let f = fs::File::open(path)?;
    let reader = BufReader::new(f);
    let mut turns = Vec::new();
    let mut session_id: Option<String> = None;
    let mut project_root: Option<String> = None;

    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() { continue; }
        let Ok(row) = serde_json::from_str::<Row>(&line) else { continue };
        if session_id.is_none() {
            session_id = row.session_id.clone();
        }
        if project_root.is_none() {
            project_root = row.cwd.clone();
        }
        let Some(k) = row.kind.as_deref() else { continue };
        if k != "user" && k != "assistant" { continue; }
        let Some(msg) = row.message else { continue };
        let role = msg.role.clone().unwrap_or_else(|| k.to_string());
        if role != "user" && role != "assistant" { continue; }
        let content = msg.content.as_ref().map(content_to_text).unwrap_or_default();
        if content.trim().is_empty() { continue; }
        // Cap per-turn content to 8 KB to keep transcripts streamable.
        let content: String = content.chars().take(8000).collect();
        let ts_unix_ms = row.timestamp.as_deref().and_then(ts_to_ms);
        turns.push(ChatTurn { role, content, ts_unix_ms });
        if turns.len() >= max_turns { break; }
    }

    Ok(ChatTranscript {
        file_path: path.display().to_string(),
        session_id: session_id.unwrap_or_default(),
        project_root,
        turns,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSearchHit {
    pub file_path: String,
    pub session_id: String,
    pub project: Option<String>,
    pub role: String,
    pub snippet: String,
    pub modified_unix_ms: i64,
}

/// Clamp a byte index down to the nearest char boundary of `s` (`<= idx`).
/// `str::floor_char_boundary` is still unstable, so we provide a small local
/// equivalent. The returned index is always a valid boundary for slicing.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn search_chats(query: &str, limit: usize) -> Vec<ChatSearchHit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() { return vec![] }
    let mut hits: Vec<ChatSearchHit> = Vec::new();
    let summaries = list_chats();
    for s in &summaries {
        if hits.len() >= limit { break }
        let path = PathBuf::from(&s.file_path);
        let Ok(transcript) = read_chat(&path, 2000) else { continue };
        for t in transcript.turns {
            // Search on a lowercased copy, but compute the snippet on that same
            // lowercased string so byte offsets always refer to the string we slice.
            let body_lc = t.content.to_lowercase();
            if let Some(pos) = body_lc.find(&q) {
                // `pos` and the window edges are byte offsets that may fall inside a
                // multi-byte UTF-8 char; clamp every offset to a char boundary of
                // `body_lc` before slicing to avoid a panic.
                let start = floor_char_boundary(&body_lc, pos.saturating_sub(60));
                let end = floor_char_boundary(&body_lc, (pos + q.len() + 120).min(body_lc.len()));
                let snippet: String = body_lc[start..end].replace('\n', " ");
                hits.push(ChatSearchHit {
                    file_path: s.file_path.clone(),
                    session_id: s.session_id.clone(),
                    project: s.project.clone(),
                    role: t.role.clone(),
                    snippet,
                    modified_unix_ms: s.modified_unix_ms,
                });
                if hits.len() >= limit { break }
            }
        }
    }
    hits
}
