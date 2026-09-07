//! Tolerant parsers for external AI chat-history exports.
//!
//! Every supported export shape is normalized into a common
//! [`ImportedConversation`] / [`ImportedMessage`] model that the import
//! pipeline ([`super::pipeline`]) turns into resumable Cortex sessions.
//!
//! Design rule: **never panic, never fail the whole import on one bad record.**
//! A malformed conversation or message is skipped, not propagated as an error.
//! All field access is via `serde_json::Value` lookups with graceful fallbacks
//! rather than `#[derive(Deserialize)]` on a rigid struct, because these export
//! schemas drift and partial data is still worth importing.

use serde::{Deserialize, Serialize};

/// A normalized message inside an [`ImportedConversation`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedMessage {
    /// Cortex role: `"user"`, `"assistant"`, or `"system"`.
    pub role: String,
    /// Plain-text message content.
    pub content: String,
    /// Source timestamp in epoch **milliseconds**, or 0 when the export omits
    /// one. The pipeline enforces monotonicity within a conversation, so a
    /// missing/duplicate ts never reorders the transcript.
    pub ts: i64,
}

/// A normalized conversation, the unit the import pipeline persists as one
/// Cortex session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedConversation {
    pub title: String,
    /// Provenance label surfaced to the user: `"claude.ai"`, `"chatgpt"`,
    /// or `"generic"`.
    pub source: String,
    /// Conversation creation time in epoch **milliseconds** (0 if unknown).
    pub created_ts: i64,
    pub messages: Vec<ImportedMessage>,
}

/// Detected export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Claude,
    ChatGpt,
    Generic,
    Unknown,
}

/// Sanity ceiling: never accept an export larger than this (bytes). Guards the
/// mobile endpoint against a hostile/accidental multi-hundred-MB paste.
pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;

/// Auto-detect the export format from a parsed JSON value by structure.
///
/// Heuristics (first match wins), checking the first array element for arrays:
/// - a `chat_messages` array  → Claude.ai export
/// - a `mapping` object       → ChatGPT export
/// - a `messages` array       → Generic
pub fn detect_format(json: &serde_json::Value) -> Format {
    let probe = match json {
        serde_json::Value::Array(arr) => arr.first(),
        serde_json::Value::Object(_) => Some(json),
        _ => None,
    };
    let Some(obj) = probe else { return Format::Unknown };

    if obj.get("chat_messages").map(|v| v.is_array()).unwrap_or(false) {
        return Format::Claude;
    }
    if obj.get("mapping").map(|v| v.is_object()).unwrap_or(false) {
        return Format::ChatGpt;
    }
    if obj.get("messages").map(|v| v.is_array()).unwrap_or(false) {
        return Format::Generic;
    }
    Format::Unknown
}

/// Parse + dispatch by detected format. `format == None` means auto-detect.
pub fn parse_any(
    json: &serde_json::Value,
    format: Option<Format>,
) -> Vec<ImportedConversation> {
    let fmt = format.unwrap_or_else(|| detect_format(json));
    match fmt {
        Format::Claude => parse_claude(json),
        Format::ChatGpt => parse_chatgpt(json),
        Format::Generic => parse_generic(json),
        Format::Unknown => Vec::new(),
    }
}

/// Coerce the top level to a slice of conversation objects: either a JSON array
/// of objects or a single object treated as a one-element list.
fn top_level_items(json: &serde_json::Value) -> Vec<&serde_json::Value> {
    match json {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        serde_json::Value::Object(_) => vec![json],
        _ => Vec::new(),
    }
}

/// Parse an RFC3339 string or an epoch number (seconds or millis) into epoch
/// millis. Returns 0 when unparseable/absent so callers stay panic-free.
fn parse_ts(v: Option<&serde_json::Value>) -> i64 {
    match v {
        Some(serde_json::Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0),
        Some(serde_json::Value::Number(n)) => {
            // Heuristic: values below ~10^11 are epoch seconds (any plausible
            // chat date in seconds is < 1e11), so scale to millis.
            if let Some(f) = n.as_f64() {
                if f.abs() < 1e11 {
                    (f * 1000.0) as i64
                } else {
                    f as i64
                }
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// Pull text out of either a plain string field or a Claude `content` array of
/// `{type:"text", text}` blocks. Joins multiple text blocks with newlines.
fn extract_text(text_field: Option<&serde_json::Value>, content_field: Option<&serde_json::Value>) -> String {
    // Prefer the flat `text` if non-empty.
    if let Some(serde_json::Value::String(s)) = text_field {
        if !s.trim().is_empty() {
            return s.clone();
        }
    }
    // Otherwise walk the structured content blocks.
    if let Some(serde_json::Value::Array(blocks)) = content_field {
        let joined = blocks
            .iter()
            .filter_map(|b| {
                let is_text = b.get("type").and_then(|t| t.as_str()).map(|t| t == "text").unwrap_or(true);
                if !is_text {
                    return None;
                }
                b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.trim().is_empty() {
            return joined;
        }
    }
    // Last resort: the flat text even if it was whitespace-only.
    if let Some(serde_json::Value::String(s)) = text_field {
        return s.clone();
    }
    String::new()
}

// ───────────────────────────────────────────────────────────────────────────
// Claude.ai export
// ───────────────────────────────────────────────────────────────────────────

/// Parse a Claude.ai `conversations.json` export.
///
/// Shape: top-level array, each item `{ uuid, name, created_at, chat_messages:
/// [{ text|content, sender("human"|"assistant"), created_at }] }`. `sender`
/// maps human→user, assistant→assistant. Malformed items/messages are skipped.
pub fn parse_claude(json: &serde_json::Value) -> Vec<ImportedConversation> {
    let mut out = Vec::new();
    for item in top_level_items(json) {
        let Some(msgs_val) = item.get("chat_messages").and_then(|v| v.as_array()) else {
            continue;
        };
        let title = item
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Untitled Claude chat")
            .to_string();
        let created_ts = parse_ts(item.get("created_at"));

        let mut messages = Vec::new();
        for m in msgs_val {
            let sender = m.get("sender").and_then(|v| v.as_str()).unwrap_or("");
            let role = match sender {
                "human" => "user",
                "assistant" => "assistant",
                // Unknown sender: skip rather than guess.
                _ => continue,
            };
            let content = extract_text(m.get("text"), m.get("content"));
            if content.trim().is_empty() {
                continue;
            }
            let ts = parse_ts(m.get("created_at"));
            messages.push(ImportedMessage { role: role.to_string(), content, ts });
        }
        if messages.is_empty() {
            continue;
        }
        out.push(ImportedConversation {
            title,
            source: "claude.ai".to_string(),
            created_ts,
            messages,
        });
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// ChatGPT export
// ───────────────────────────────────────────────────────────────────────────

/// Parse a ChatGPT `conversations.json` export by linearizing each
/// conversation's `mapping` tree. See [`linearize_chatgpt`] for the traversal.
pub fn parse_chatgpt(json: &serde_json::Value) -> Vec<ImportedConversation> {
    let mut out = Vec::new();
    for item in top_level_items(json) {
        let Some(mapping) = item.get("mapping").and_then(|v| v.as_object()) else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Untitled ChatGPT chat")
            .to_string();
        let created_ts = parse_ts(item.get("create_time"));
        let current_node = item.get("current_node").and_then(|v| v.as_str());

        let messages = linearize_chatgpt(mapping, current_node);
        if messages.is_empty() {
            continue;
        }
        out.push(ImportedConversation {
            title,
            source: "chatgpt".to_string(),
            created_ts,
            messages,
        });
    }
    out
}

/// Linearize a ChatGPT `mapping` node tree into an ordered message list.
///
/// Strategy:
/// 1. If `current_node` is present, walk **up** the `parent` chain from it to
///    the root, then reverse — this is the exact conversation path the user saw
///    (handles regenerated/branched trees correctly).
/// 2. Otherwise walk **down** from the root (the node with `parent == null`)
///    following the first child at each step (depth-first first-child).
///
/// Only nodes whose message is `content_type == "text"` (or has joinable
/// `parts`), non-empty, and role ∈ {user, assistant, system} are kept. Tool /
/// system-bookkeeping / empty nodes are dropped.
pub fn linearize_chatgpt(
    mapping: &serde_json::Map<String, serde_json::Value>,
    current_node: Option<&str>,
) -> Vec<ImportedMessage> {
    // Build the ordered node-key path.
    let path: Vec<String> = if let Some(cur) = current_node.filter(|c| mapping.contains_key(*c)) {
        // Walk up parents to root, then reverse.
        let mut chain = Vec::new();
        let mut node_key = Some(cur.to_string());
        let mut guard = 0usize;
        while let Some(key) = node_key {
            if guard > mapping.len() + 1 {
                break; // cycle guard
            }
            guard += 1;
            let parent = mapping
                .get(&key)
                .and_then(|n| n.get("parent"))
                .and_then(|p| p.as_str())
                .map(|s| s.to_string());
            chain.push(key);
            node_key = parent.filter(|p| mapping.contains_key(p));
        }
        chain.reverse();
        chain
    } else {
        // Find the root: parent is null or points outside the map.
        let root = mapping.iter().find(|(_, n)| match n.get("parent") {
            None | Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(p)) => !mapping.contains_key(p),
            _ => true,
        });
        let mut chain = Vec::new();
        let mut node_key = root.map(|(k, _)| k.clone());
        let mut guard = 0usize;
        while let Some(key) = node_key {
            if guard > mapping.len() + 1 {
                break;
            }
            guard += 1;
            let next = mapping
                .get(&key)
                .and_then(|n| n.get("children"))
                .and_then(|c| c.as_array())
                .and_then(|c| c.first())
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            chain.push(key);
            node_key = next.filter(|c| mapping.contains_key(c));
        }
        chain
    };

    let mut out = Vec::new();
    for key in path {
        let Some(node) = mapping.get(&key) else { continue };
        let Some(message) = node.get("message") else { continue };
        if message.is_null() {
            continue;
        }
        let role = message
            .get("author")
            .and_then(|a| a.get("role"))
            .and_then(|r| r.as_str())
            .unwrap_or("");
        if role != "user" && role != "assistant" && role != "system" {
            continue; // drop tool nodes
        }
        let content = message.get("content");
        let content_type = content
            .and_then(|c| c.get("content_type"))
            .and_then(|t| t.as_str())
            .unwrap_or("text");
        if content_type != "text" {
            continue;
        }
        let text = content
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let ts = parse_ts(message.get("create_time"));
        out.push(ImportedMessage { role: role.to_string(), content: text.to_string(), ts });
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// Generic export
// ───────────────────────────────────────────────────────────────────────────

/// Parse the generic shape: either `[{title?, messages:[{role, content, ts?}]}]`
/// or a single `{title?, messages:[...]}`.
pub fn parse_generic(json: &serde_json::Value) -> Vec<ImportedConversation> {
    let mut out = Vec::new();
    for item in top_level_items(json) {
        let Some(msgs_val) = item.get("messages").and_then(|v| v.as_array()) else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Imported chat")
            .to_string();
        let created_ts = parse_ts(item.get("created_ts").or_else(|| item.get("created_at")));

        let mut messages = Vec::new();
        for m in msgs_val {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let role = match role {
                "user" | "human" => "user",
                "assistant" | "ai" | "bot" => "assistant",
                "system" => "system",
                _ => continue,
            };
            // Accept either a plain `content` string or Claude-style blocks.
            let content = extract_text(m.get("content"), m.get("content"));
            if content.trim().is_empty() {
                continue;
            }
            let ts = parse_ts(m.get("ts").or_else(|| m.get("timestamp")).or_else(|| m.get("created_at")));
            messages.push(ImportedMessage { role: role.to_string(), content, ts });
        }
        if messages.is_empty() {
            continue;
        }
        out.push(ImportedConversation {
            title,
            source: "generic".to_string(),
            created_ts,
            messages,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_claude_chatgpt_generic() {
        let claude = serde_json::json!([{ "chat_messages": [] }]);
        let chatgpt = serde_json::json!([{ "mapping": {} }]);
        let generic = serde_json::json!([{ "messages": [] }]);
        let single = serde_json::json!({ "messages": [] });
        let unknown = serde_json::json!([{ "foo": 1 }]);
        assert_eq!(detect_format(&claude), Format::Claude);
        assert_eq!(detect_format(&chatgpt), Format::ChatGpt);
        assert_eq!(detect_format(&generic), Format::Generic);
        assert_eq!(detect_format(&single), Format::Generic);
        assert_eq!(detect_format(&unknown), Format::Unknown);
    }

    #[test]
    fn claude_parses_both_text_shapes_and_skips_malformed() {
        let json = serde_json::json!([
            {
                "uuid": "abc",
                "name": "Hello chat",
                "created_at": "2026-01-01T00:00:00Z",
                "chat_messages": [
                    { "sender": "human", "text": "hi there", "created_at": "2026-01-01T00:00:01Z" },
                    { "sender": "assistant", "content": [{ "type": "text", "text": "hello back" }], "created_at": "2026-01-01T00:00:02Z" },
                    { "sender": "human", "text": "   " },          // empty → skipped
                    { "sender": "tool", "text": "noise" },          // unknown sender → skipped
                ]
            },
            { "uuid": "empty", "name": "nada", "chat_messages": [] }, // no real msgs → conv skipped
            { "not_a_conv": true }                                   // malformed → skipped
        ]);
        let convs = parse_claude(&json);
        assert_eq!(convs.len(), 1);
        let c = &convs[0];
        assert_eq!(c.title, "Hello chat");
        assert_eq!(c.source, "claude.ai");
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].role, "user");
        assert_eq!(c.messages[0].content, "hi there");
        assert_eq!(c.messages[1].role, "assistant");
        assert_eq!(c.messages[1].content, "hello back");
        assert!(c.created_ts > 0);
    }

    #[test]
    fn chatgpt_linearizes_via_current_node_path() {
        // root → u1 → a1 → u2(branch a) and u2b(branch b). current_node = a2 on
        // branch a, so we must follow the a-branch, not pick the first child.
        let json = serde_json::json!([{
            "title": "Branched chat",
            "create_time": 1_700_000_000.0,
            "current_node": "a2",
            "mapping": {
                "root": { "id": "root", "parent": null, "children": ["u1"], "message": null },
                "u1": { "id": "u1", "parent": "root", "children": ["a1"],
                        "message": { "author": { "role": "user" }, "create_time": 1_700_000_001.0,
                                     "content": { "content_type": "text", "parts": ["question one"] } } },
                "a1": { "id": "a1", "parent": "u1", "children": ["u2a", "u2b"],
                        "message": { "author": { "role": "assistant" }, "create_time": 1_700_000_002.0,
                                     "content": { "content_type": "text", "parts": ["answer one"] } } },
                "u2a": { "id": "u2a", "parent": "a1", "children": ["a2"],
                        "message": { "author": { "role": "user" }, "create_time": 1_700_000_003.0,
                                     "content": { "content_type": "text", "parts": ["follow up A"] } } },
                "a2": { "id": "a2", "parent": "u2a", "children": [],
                        "message": { "author": { "role": "assistant" }, "create_time": 1_700_000_004.0,
                                     "content": { "content_type": "text", "parts": ["final A"] } } },
                "u2b": { "id": "u2b", "parent": "a1", "children": [],
                        "message": { "author": { "role": "user" }, "create_time": 1_700_000_009.0,
                                     "content": { "content_type": "text", "parts": ["follow up B (not chosen)"] } } },
                "tool": { "id": "tool", "parent": "a1", "children": [],
                        "message": { "author": { "role": "tool" },
                                     "content": { "content_type": "text", "parts": ["tool junk"] } } }
            }
        }]);
        let convs = parse_chatgpt(&json);
        assert_eq!(convs.len(), 1);
        let c = &convs[0];
        let texts: Vec<&str> = c.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(texts, vec!["question one", "answer one", "follow up A", "final A"]);
        // Branch B and the tool node must not appear.
        assert!(!texts.iter().any(|t| t.contains("not chosen")));
        assert!(!texts.iter().any(|t| t.contains("tool junk")));
    }

    #[test]
    fn chatgpt_linearizes_via_root_when_no_current_node() {
        let json = serde_json::json!([{
            "title": "Linear chat",
            "mapping": {
                "root": { "parent": null, "children": ["m1"], "message": null },
                "m1": { "parent": "root", "children": ["m2"],
                        "message": { "author": { "role": "user" },
                                     "content": { "content_type": "text", "parts": ["one"] } } },
                "m2": { "parent": "m1", "children": [],
                        "message": { "author": { "role": "assistant" },
                                     "content": { "content_type": "text", "parts": ["two"] } } }
            }
        }]);
        let convs = parse_chatgpt(&json);
        assert_eq!(convs.len(), 1);
        let texts: Vec<&str> = convs[0].messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(texts, vec!["one", "two"]);
    }

    #[test]
    fn generic_array_and_single_object() {
        // ts given in epoch millis (>= 1e11) is preserved as-is; smaller values
        // are treated as epoch seconds and scaled (see `parse_ts`).
        let arr = serde_json::json!([
            { "title": "G1", "messages": [
                { "role": "user", "content": "u", "ts": 1_700_000_000_000i64 },
                { "role": "assistant", "content": "a" },
                { "role": "weird", "content": "skip me" }
            ]}
        ]);
        let single = serde_json::json!({
            "messages": [ { "role": "human", "content": "hey" } ]
        });
        let ca = parse_generic(&arr);
        assert_eq!(ca.len(), 1);
        assert_eq!(ca[0].title, "G1");
        assert_eq!(ca[0].messages.len(), 2);
        assert_eq!(ca[0].messages[0].ts, 1_700_000_000_000i64);

        let cs = parse_generic(&single);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].title, "Imported chat");
        assert_eq!(cs[0].messages[0].role, "user");
    }

    #[test]
    fn parse_any_autodetects() {
        let claude = serde_json::json!([{ "name": "x", "chat_messages": [
            { "sender": "human", "text": "hi" }
        ]}]);
        let convs = parse_any(&claude, None);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].source, "claude.ai");
    }

    #[test]
    fn no_panic_on_garbage() {
        for v in [
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!("a string"),
            serde_json::json!([1, 2, 3]),
            serde_json::json!([{ "mapping": "not an object" }]),
            serde_json::json!([{ "chat_messages": "nope" }]),
        ] {
            // Must never panic, just return empty.
            let _ = parse_any(&v, None);
            let _ = parse_claude(&v);
            let _ = parse_chatgpt(&v);
            let _ = parse_generic(&v);
            let _ = detect_format(&v);
        }
    }
}
