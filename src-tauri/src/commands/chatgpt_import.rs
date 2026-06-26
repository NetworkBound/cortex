//! ChatGPT export importer.
//!
//! OpenAI's chat.openai.com data export ships a single `conversations.json`
//! file: a JSON array where every element is one conversation, and each
//! conversation has a `mapping` tree of message nodes. We flatten each
//! conversation into a Claude-Code-style `.jsonl` file under
//! `~/.claude/projects/chatgpt-import/<conv-id>.jsonl` so the existing
//! `list_chats()` / `getClaudeChat()` plumbing surfaces them with zero
//! schema-side changes.
//!
//! Why under `.claude/projects`: it's the only place the chat-history
//! scanner already looks, so importing is a no-op once the files exist —
//! they appear in the right-tab Chats list grouped by "chatgpt-import"
//! and clicking a row resumes them through the same `cortex:chat-replay`
//! path as native Claude sessions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Conversation {
    id: Option<String>,
    title: Option<String>,
    create_time: Option<f64>,
    mapping: HashMap<String, Node>,
}

#[derive(Deserialize)]
struct Node {
    id: Option<String>,
    parent: Option<String>,
    children: Option<Vec<String>>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    author: Option<Author>,
    create_time: Option<f64>,
    content: Option<Content>,
}

#[derive(Deserialize)]
struct Author {
    role: Option<String>,
}

#[derive(Deserialize)]
struct Content {
    content_type: Option<String>,
    parts: Option<Vec<serde_json::Value>>,
}

#[derive(Serialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub out_dir: String,
}

/// Import an OpenAI `conversations.json` export.
///
/// Behaviour:
/// - Reads the file at `path` (must be the literal `conversations.json` from
///   the OpenAI data export ZIP).
/// - Writes one `.jsonl` per conversation under
///   `~/.claude/projects/chatgpt-import/<safe-id>.jsonl`.
/// - Skips conversations that already have a file (idempotent re-import).
/// - Returns counts so the UI can show "imported N, skipped M".
#[tauri::command]
pub async fn import_chatgpt_export(path: String) -> Result<ImportResult, String> {
    tokio::task::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        if !p.is_file() {
            return Err(format!("not a file: {}", p.display()));
        }
        let raw = fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
        // Top level may be either an array of conversations OR a single object.
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", p.display()))?;
        let convs: Vec<Conversation> = match value {
            serde_json::Value::Array(arr) => arr
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect(),
            serde_json::Value::Object(_) => {
                vec![serde_json::from_value(value).map_err(|e| format!("single-conv parse: {e}"))?]
            }
            other => return Err(format!("unexpected top-level type: {other:?}")),
        };

        let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
        let out_dir = home.join(".claude").join("projects").join("chatgpt-import");
        fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;

        let mut imported = 0usize;
        let mut skipped = 0usize;
        for conv in convs {
            let id = conv.id.clone().unwrap_or_else(|| format!("conv-{}", imported));
            let safe_id: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
            let out_path = out_dir.join(format!("{safe_id}.jsonl"));
            if out_path.exists() {
                skipped += 1;
                continue;
            }
            let messages = flatten(&conv);
            if messages.is_empty() {
                skipped += 1;
                continue;
            }
            let mut file = fs::File::create(&out_path)
                .map_err(|e| format!("create {}: {e}", out_path.display()))?;
            for (role, text, ts_ms) in messages {
                let row = serde_json::json!({
                    "type": role,
                    "sessionId": safe_id,
                    "timestamp": ms_to_iso(ts_ms),
                    "message": { "role": role, "content": text }
                });
                writeln!(file, "{}", row).map_err(|e| format!("write {}: {e}", out_path.display()))?;
            }
            imported += 1;
        }

        Ok(ImportResult {
            imported,
            skipped,
            out_dir: out_dir.display().to_string(),
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Walk a conversation's message tree in chronological order and emit
/// `(role, content, ts_ms)` for every user/assistant turn that has non-empty
/// text. System/tool/internal turns are skipped — they don't belong in a
/// resume transcript and would just noise up the chat replay.
fn flatten(conv: &Conversation) -> Vec<(String, String, i64)> {
    // Establish a deterministic traversal order by walking the mapping tree
    // along its parent/child links (the actual conversation thread), rather
    // than iterating the HashMap (whose order is randomized per run). This
    // ordering is the tiebreaker for messages that share a timestamp or whose
    // `create_time` is missing (ts == 0) — without it those rows would land in
    // arbitrary order on every import.
    let order = node_order(conv);

    // (seq, role, text, ts_ms). `seq` is the tree-traversal index.
    let mut rows: Vec<(usize, String, String, i64)> = Vec::new();
    for (node_key, node) in conv.mapping.iter() {
        let Some(msg) = node.message.as_ref() else { continue };
        let role = msg.author.as_ref().and_then(|a| a.role.clone()).unwrap_or_default();
        if role != "user" && role != "assistant" { continue; }
        let Some(content) = msg.content.as_ref() else { continue };
        if content.content_type.as_deref().unwrap_or("text") != "text" { continue; }
        let Some(parts) = content.parts.as_ref() else { continue };
        let text: String = parts
            .iter()
            .filter_map(|p| match p {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = text.trim();
        if text.is_empty() { continue; }
        let ts_ms = msg
            .create_time
            .map(|t| (t * 1000.0) as i64)
            .or_else(|| conv.create_time.map(|t| (t * 1000.0) as i64))
            .unwrap_or(0);
        // `node_order` is keyed by the mapping's own key, so look up by that.
        // Fall back to usize::MAX (sorts last, but still deterministic) only if
        // somehow absent. Touch the ids so they remain part of the parsed model.
        let _ = (msg.id.as_ref(), node.id.as_ref(), conv.title.as_ref());
        let seq = order.get(node_key).copied().unwrap_or(usize::MAX);
        rows.push((seq, role, text.to_string(), ts_ms));
    }
    // Stable sort by timestamp, with the tree-traversal sequence as a
    // deterministic secondary key so equal/zero timestamps keep a defined order.
    rows.sort_by(|a, b| a.3.cmp(&b.3).then(a.0.cmp(&b.0)));
    rows.into_iter().map(|(_, role, text, ts)| (role, text, ts)).collect()
}

/// Build a map from node-id to its position in a depth-first walk of the
/// conversation mapping tree, following `children` links from the root. This
/// yields the natural thread order; any node not reachable from the root is
/// appended afterwards in sorted-key order so the result stays deterministic.
fn node_order(conv: &Conversation) -> HashMap<String, usize> {
    let mut order: HashMap<String, usize> = HashMap::new();
    let mut seq = 0usize;

    // The root is the node whose `parent` is absent (or points outside the map).
    let mut roots: Vec<&String> = conv
        .mapping
        .iter()
        .filter(|(_, n)| match n.parent.as_ref() {
            None => true,
            Some(p) => !conv.mapping.contains_key(p),
        })
        .map(|(k, _)| k)
        .collect();
    roots.sort();

    let mut stack: Vec<&String> = roots.into_iter().rev().collect();
    while let Some(key) = stack.pop() {
        if order.contains_key(key) {
            continue;
        }
        order.insert(key.clone(), seq);
        seq += 1;
        if let Some(node) = conv.mapping.get(key) {
            if let Some(children) = node.children.as_ref() {
                // Push children in reverse so they pop in declared order.
                for child in children.iter().rev() {
                    if conv.mapping.contains_key(child) {
                        stack.push(child);
                    }
                }
            }
        }
    }

    // Any nodes unreachable from a root (malformed export): append in sorted
    // key order so they still get a deterministic sequence.
    let mut leftovers: Vec<&String> =
        conv.mapping.keys().filter(|k| !order.contains_key(*k)).collect();
    leftovers.sort();
    for key in leftovers {
        order.insert(key.clone(), seq);
        seq += 1;
    }

    order
}

fn ms_to_iso(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".into())
}
