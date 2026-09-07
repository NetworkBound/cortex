//! Import external AI chat history (Claude.ai / ChatGPT / generic exports) into
//! Cortex as resumable, searchable sessions — plus experimental live pullers.
//!
//! - [`parse`]   — tolerant parsers + auto-detect → [`parse::ImportedConversation`].
//! - [`pipeline`] — write conversations as Cortex sessions ([`pipeline::import_conversations`]).
//! - [`pull`]    — EXPERIMENTAL live pullers via session token (unofficial APIs).
//!
//! See `commands::chat_import` for the Tauri surface and `mobile_server` for the
//! HTTP endpoints.

pub mod parse;
pub mod pipeline;
pub mod pull;

pub use parse::{
    detect_format, parse_any, parse_chatgpt, parse_claude, parse_generic, Format,
    ImportedConversation, ImportedMessage, MAX_IMPORT_BYTES,
};
pub use pipeline::{import_conversations, ImportResult};
pub use pull::{pull_chatgpt, pull_claude};

/// Parse a raw export string (with optional explicit format) and import it into
/// `store`. Shared by the Tauri `import_chat_file` command and the
/// `POST /api/import/file` endpoint so both behave identically.
///
/// Enforces [`MAX_IMPORT_BYTES`]; returns a human-readable `Err` on oversize or
/// unparseable input.
pub async fn import_from_str(
    content: &str,
    format: Option<Format>,
    store: &crate::observability::tracing_store::TracingStore,
) -> Result<ImportResult, String> {
    if content.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "import too large: {} bytes (max {})",
            content.len(),
            MAX_IMPORT_BYTES
        ));
    }
    let json: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;
    let convs = parse_any(&json, format);
    if convs.is_empty() {
        return Err("no importable conversations found (unrecognized format?)".to_string());
    }
    Ok(import_conversations(convs, store).await)
}

/// Map a `"auto"|"claude"|"chatgpt"|"generic"` string to a [`Format`] option.
/// `"auto"` / unknown → `None` (auto-detect).
pub fn format_from_str(s: &str) -> Option<Format> {
    match s.to_ascii_lowercase().as_str() {
        "claude" => Some(Format::Claude),
        "chatgpt" => Some(Format::ChatGpt),
        "generic" => Some(Format::Generic),
        _ => None,
    }
}

/// EXPERIMENTAL: pull from a provider and import. Shared by the Tauri
/// `import_chat_pull` command and `POST /api/import/pull`.
///
/// `provider` is `"claude"` or `"chatgpt"`. `token` is the session credential
/// (never logged). See [`pull`] for the heavy ToS/fragility caveats.
pub async fn import_from_pull(
    provider: &str,
    token: &str,
    store: &crate::observability::tracing_store::TracingStore,
) -> Result<ImportResult, String> {
    let convs = match provider.to_ascii_lowercase().as_str() {
        "claude" => pull_claude(token).await?,
        "chatgpt" => pull_chatgpt(token).await?,
        other => return Err(format!("unknown provider: {other} (expected claude|chatgpt)")),
    };
    if convs.is_empty() {
        return Err("pull succeeded but returned no conversations".to_string());
    }
    Ok(import_conversations(convs, store).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::tracing_store::TracingStore;

    #[tokio::test]
    async fn import_from_str_end_to_end() {
        let store = TracingStore::in_memory();
        let raw = serde_json::json!([{
            "name": "E2E",
            "chat_messages": [
                { "sender": "human", "text": "hello" },
                { "sender": "assistant", "text": "hi" }
            ]
        }])
        .to_string();
        let res = import_from_str(&raw, None, &store).await.unwrap();
        assert_eq!(res.imported, 1);
        assert_eq!(store.recent_chat_sessions(10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn import_from_str_rejects_oversize_and_garbage() {
        let store = TracingStore::in_memory();
        assert!(import_from_str("not json", None, &store).await.is_err());
        let big = "x".repeat(MAX_IMPORT_BYTES + 1);
        assert!(import_from_str(&big, None, &store).await.is_err());
    }

    #[test]
    fn format_from_str_maps() {
        assert_eq!(format_from_str("claude"), Some(Format::Claude));
        assert_eq!(format_from_str("ChatGPT"), Some(Format::ChatGpt));
        assert_eq!(format_from_str("generic"), Some(Format::Generic));
        assert_eq!(format_from_str("auto"), None);
        assert_eq!(format_from_str("nonsense"), None);
    }

    #[tokio::test]
    async fn import_from_pull_unknown_provider() {
        let store = TracingStore::in_memory();
        assert!(import_from_pull("bogus", "tok", &store).await.is_err());
    }
}
