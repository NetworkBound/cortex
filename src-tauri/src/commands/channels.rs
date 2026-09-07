//! Multi-agent channels — Open WebUI-style persistent rooms where multiple
//! AI agents and the user coexist. `@role-name` mentions in a posted message
//! summon the matching `~/.cortex/roles/<name>.yaml` persona via the gateway and
//! append the response as a new channel message.
//!
//! Storage: one JSON file per channel at `~/.cortex/channels/<id>.json`.
//! Schema is intentionally minimal — channels are conversational, not
//! transactional, so read-modify-write with no locking is fine for the file
//! sizes we're dealing with.
//!
//! Gateway plumbing reuses the simple streaming-collect pattern from
//! `commit_suggest`: one `chat_completion_stream` per mentioned role, message
//! list = system prompt (from role) + the user's post.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, State};
use tokio::sync::mpsc;

use crate::agents::roles::{self};
use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Wall-clock cap on a single agent response. Keeps the UI honest when an
/// upstream is slow — beyond this we surface an error message in-channel
/// rather than blocking the panel forever.
const AGENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Hard cap on transcript length we'll keep on disk per channel. Old messages
/// are dropped (FIFO) once we cross this so the JSON files stay small.
const MAX_MESSAGES: usize = 500;

/// Per-agent progress emitted from `post_message` while a turn is in flight so
/// the panel can stream replies into the transcript instead of a frozen
/// "Sending…". Channel-scoped event name (`channels:progress:<channel_id>`)
/// mirrors the `batch:progress:<run_id>` pattern used elsewhere. Purely
/// additive — the command still returns the full appended message list.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelProgress {
    /// The `@role-name` this update is about.
    pub role: String,
    /// `"start"` | `"delta"` | `"done"` | `"error"`.
    pub status: String,
    /// `delta`: the incremental token chunk. `done`/`error`: the final,
    /// authoritative reply text. `start`: `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

fn emit_progress(app: &tauri::AppHandle, channel_id: &str, progress: ChannelProgress) {
    let _ = app.emit(&format!("channels:progress:{channel_id}"), progress);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberSpec {
    /// `"user"` or `"agent_role"`.
    pub kind: String,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: String,
    pub author_kind: String,
    pub author_id: String,
    pub content: String,
    pub ts: i64,
    #[serde(default)]
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub members: Vec<MemberSpec>,
    #[serde(default)]
    pub messages: Vec<ChannelMessage>,
    #[serde(default)]
    pub created_unix_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn channels_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("channels"))
}

/// `id`s are user-typed (via the channel name slug), so guard against
/// path-traversal. Letters / digits / `-` / `_` / `.` only.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out = format!("channel-{}", now_ms());
    }
    out.truncate(48);
    out
}

fn channel_path(id: &str) -> Result<PathBuf, String> {
    if !is_safe_id(id) {
        return Err(format!("invalid channel id '{id}'"));
    }
    Ok(channels_dir()?.join(format!("{id}.json")))
}

fn load_channel(id: &str) -> Result<Channel, String> {
    let path = channel_path(id)?;
    let bytes = fs::read(&path).map_err(|e| format!("read failed: {e}"))?;
    serde_json::from_slice::<Channel>(&bytes).map_err(|e| format!("parse failed: {e}"))
}

fn save_channel(channel: &Channel) -> Result<(), String> {
    let path = channel_path(&channel.id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json =
        serde_json::to_vec_pretty(channel).map_err(|e| format!("serialize failed: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("write failed: {e}"))
}

/// Extract `@role-name` mentions. We match a leading `@` followed by the same
/// safe-id characters as channel ids — roles live at `~/.cortex/roles/*.yaml`
/// and share that charset.
fn extract_mentions(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            // skip if preceded by an alnum char (email-ish) — only match
            // mentions at start-of-token.
            let prev_ok = i == 0 || {
                let p = bytes[i - 1];
                !p.is_ascii_alphanumeric() && p != b'_'
            };
            if prev_ok {
                let mut j = i + 1;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.' {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if j > i + 1 {
                    let name = &content[i + 1..j];
                    if !out.iter().any(|n| n == name) {
                        out.push(name.to_string());
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Render the last `n` messages as `author: content` lines — the compact room
/// context handed to each summoned role so multi-agent turns read coherently.
fn transcript_tail(messages: &[ChannelMessage], n: usize) -> String {
    let skip = messages.len().saturating_sub(n);
    let mut out = String::new();
    for m in &messages[skip..] {
        out.push_str(&m.author_id);
        out.push_str(": ");
        out.push_str(&m.content);
        out.push('\n');
    }
    out
}

#[tauri::command]
pub async fn list_channels() -> Result<Vec<Channel>, String> {
    let dir = channels_dir()?;
    let read = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        if let Ok(channel) = serde_json::from_slice::<Channel>(&bytes) {
            out.push(channel);
        }
    }
    out.sort_by(|a, b| b.created_unix_ms.cmp(&a.created_unix_ms));
    Ok(out)
}

#[tauri::command]
pub async fn get_channel(id: String) -> Result<Channel, String> {
    load_channel(&id)
}

#[tauri::command]
pub async fn create_channel(
    name: String,
    description: Option<String>,
    members: Vec<MemberSpec>,
) -> Result<Channel, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name is required".into());
    }
    let id = slugify(trimmed);
    if !is_safe_id(&id) {
        return Err("could not derive a safe channel id".into());
    }
    // Refuse to overwrite an existing channel; the UI surfaces this as "name
    // already taken" and lets the user tweak it.
    if channel_path(&id)?.exists() {
        return Err(format!("channel '{id}' already exists"));
    }
    let channel = Channel {
        id: id.clone(),
        name: trimmed.to_string(),
        description: description.unwrap_or_default(),
        members,
        messages: Vec::new(),
        created_unix_ms: now_ms(),
    };
    save_channel(&channel)?;
    Ok(channel)
}

#[tauri::command]
pub async fn delete_channel(id: String) -> Result<(), String> {
    let path = channel_path(&id)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("delete failed: {e}"))?;
    }
    Ok(())
}

/// Append the user's message, then for each `@role-name` mention summon the
/// matching role via the gateway and append its response. Returns the full set of
/// messages appended this call (user + each agent reply) so the UI can splice
/// them into the transcript without re-fetching the whole channel.
#[tauri::command]
pub async fn post_message(
    channel_id: String,
    content: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ChannelMessage>, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("content is required".into());
    }

    let mut channel = load_channel(&channel_id)?;
    let mentions = extract_mentions(trimmed);

    let user_msg = ChannelMessage {
        id: ulid::Ulid::new().to_string(),
        author_kind: "user".into(),
        author_id: "user".into(),
        content: trimmed.to_string(),
        ts: now_ms(),
        mentions: mentions.clone(),
    };
    let mut appended: Vec<ChannelMessage> = vec![user_msg.clone()];
    channel.messages.push(user_msg);

    // Snapshot gateway config once — we'll reuse the same client for every
    // mention this turn.
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();

    for role_name in &mentions {
        // Tell the panel this agent is now in flight so it can paint a spinner.
        emit_progress(
            &app,
            &channel_id,
            ChannelProgress {
                role: role_name.clone(),
                status: "start".into(),
                text: None,
            },
        );

        let Some(role) = roles::get_role(role_name) else {
            let content = format!("⚠️ no role named '{role_name}' under ~/.cortex/roles/");
            emit_progress(
                &app,
                &channel_id,
                ChannelProgress {
                    role: role_name.clone(),
                    status: "error".into(),
                    text: Some(content.clone()),
                },
            );
            let note = ChannelMessage {
                id: ulid::Ulid::new().to_string(),
                author_kind: "system".into(),
                author_id: "system".into(),
                content,
                ts: now_ms(),
                mentions: Vec::new(),
            };
            appended.push(note.clone());
            channel.messages.push(note);
            continue;
        };

        let system_prompt = role.system_prompt.unwrap_or_default();
        let model = role.model.unwrap_or_else(|| cfg.gateway_model.clone());

        // Build a compact transcript so the role has context for the room:
        // last ~20 messages, prefixed with the author name so multi-agent
        // turns read coherently.
        let transcript = transcript_tail(&channel.messages, 20);
        let user_prompt = format!(
            "You are participating in the channel '{}'. Recent transcript:\n\n{}\n\nReply as '{}'.",
            channel.name, transcript, role_name
        );

        let req = ChatCompletionRequest {
            model,
            messages: vec![
                ChatMessage { role: "system".into(), content: system_prompt },
                ChatMessage { role: "user".into(), content: user_prompt },
            ],
            stream: true,
            temperature: Some(0.7),
        };

        let client = GatewayClient::new(cfg.gateway_base_url.clone(), api_key.clone());
        let (tx, mut rx) = mpsc::channel::<StreamItem>(64);
        let stream_fut = async move {
            let _ = client.chat_completion_stream(req, tx).await;
        };
        let collect_fut = async {
            let mut buf = String::new();
            while let Some(item) = rx.recv().await {
                match item {
                    StreamItem::Delta(s) => {
                        // Forward each token chunk so the panel can grow the
                        // reply bubble live (mirrors chat's `agent-event:<id>`
                        // token stream, scoped to this channel's event name).
                        emit_progress(
                            &app,
                            &channel_id,
                            ChannelProgress {
                                role: role_name.clone(),
                                status: "delta".into(),
                                text: Some(s.clone()),
                            },
                        );
                        buf.push_str(&s);
                    }
                    StreamItem::Done { .. } => break,
                }
            }
            buf
        };

        let (body, timed_out) = match tokio::time::timeout(AGENT_TIMEOUT, async {
            let (_, body) = tokio::join!(stream_fut, collect_fut);
            body
        })
        .await
        {
            Ok(b) => (b.trim().to_string(), false),
            Err(_) => (
                format!("⚠️ {role_name} timed out after {}s", AGENT_TIMEOUT.as_secs()),
                true,
            ),
        };

        let is_empty = body.is_empty();
        let final_body = if is_empty {
            format!("⚠️ {role_name} returned an empty response")
        } else {
            body
        };

        // `done` for a real reply, `error` for timeout / empty so the panel can
        // flag the row distinctly. Final text rides along either way.
        emit_progress(
            &app,
            &channel_id,
            ChannelProgress {
                role: role_name.clone(),
                status: if timed_out || is_empty { "error".into() } else { "done".into() },
                text: Some(final_body.clone()),
            },
        );

        let reply = ChannelMessage {
            id: ulid::Ulid::new().to_string(),
            author_kind: "agent_role".into(),
            author_id: role_name.clone(),
            content: final_body,
            ts: now_ms(),
            mentions: Vec::new(),
        };
        appended.push(reply.clone());
        channel.messages.push(reply);
    }

    // FIFO-trim so we don't grow channel JSON unboundedly. Never drain the
    // messages just appended this call (user message + agent replies): a single
    // post can add more than MAX_MESSAGES rows, and dropping them would discard
    // the very content we're returning. Only the older, pre-existing messages
    // are eligible for trimming.
    if channel.messages.len() > MAX_MESSAGES {
        let older = channel.messages.len().saturating_sub(appended.len());
        let drop = (channel.messages.len() - MAX_MESSAGES).min(older);
        if drop > 0 {
            channel.messages.drain(0..drop);
        }
    }
    save_channel(&channel)?;
    Ok(appended)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Eng Standup"), "eng-standup");
        assert_eq!(slugify("  --weird??  name  "), "weird-name");
        assert!(slugify("").starts_with("channel-"));
    }

    #[test]
    fn extract_mentions_finds_roles() {
        let m = extract_mentions("hey @code-reviewer please review this with @test-writer");
        assert_eq!(m, vec!["code-reviewer", "test-writer"]);
    }

    #[test]
    fn extract_mentions_ignores_email_like() {
        let m = extract_mentions("contact me at foo@bar.com or ping @docs-writer");
        assert_eq!(m, vec!["docs-writer"]);
    }

    #[test]
    fn extract_mentions_dedupes() {
        let m = extract_mentions("@a then @a again");
        assert_eq!(m, vec!["a"]);
    }

    fn msg(author: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: String::new(),
            author_kind: "user".into(),
            author_id: author.into(),
            content: content.into(),
            ts: 0,
            mentions: Vec::new(),
        }
    }

    #[test]
    fn transcript_tail_formats_author_lines() {
        let msgs = vec![msg("user", "hello"), msg("code-reviewer", "hi back")];
        assert_eq!(
            transcript_tail(&msgs, 20),
            "user: hello\ncode-reviewer: hi back\n"
        );
    }

    #[test]
    fn transcript_tail_keeps_only_last_n() {
        let msgs: Vec<ChannelMessage> =
            (0..5).map(|i| msg("u", &format!("m{i}"))).collect();
        assert_eq!(transcript_tail(&msgs, 2), "u: m3\nu: m4\n");
    }

    #[test]
    fn transcript_tail_empty_is_empty() {
        assert_eq!(transcript_tail(&[], 20), "");
    }

    #[test]
    fn is_safe_id_rejects_traversal() {
        assert!(!is_safe_id("../etc"));
        assert!(!is_safe_id(""));
        assert!(is_safe_id("eng-standup"));
    }
}
