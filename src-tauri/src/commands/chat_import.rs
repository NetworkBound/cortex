//! Tauri commands for importing external AI chat history into Cortex.
//!
//! Thin wrappers over [`crate::chat_import`]: file import (detect + parse +
//! persist) and the EXPERIMENTAL live pull. Both return an
//! [`crate::chat_import::ImportResult`] carrying the created `session_ids` so the
//! frontend can refresh the Recent-chats list.

use tauri::State;

use crate::chat_import::{self, ImportResult};
use crate::observability::tracing_store::TracingStore;

/// Import a chat-history export file from disk.
///
/// Reads `path`, auto-detects the format (Claude.ai / ChatGPT / generic), parses
/// tolerantly, and writes each conversation as a resumable + searchable Cortex
/// session. Idempotent: re-importing the same file skips already-present
/// conversations.
#[tauri::command]
pub async fn import_chat_file(
    path: String,
    store: State<'_, TracingStore>,
) -> Result<ImportResult, String> {
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read {path}: {e}"))?;
    chat_import::import_from_str(&content, None, &store).await
}

/// EXPERIMENTAL: pull chat history live from a provider via a session token and
/// import it. `provider` is `"claude"` or `"chatgpt"`; `token` is the user's
/// session credential (never logged).
///
/// These use unofficial, fragile, ToS-gray endpoints — see
/// [`crate::chat_import::pull`] for the full caveats.
#[tauri::command]
pub async fn import_chat_pull(
    provider: String,
    token: String,
    store: State<'_, TracingStore>,
) -> Result<ImportResult, String> {
    chat_import::import_from_pull(&provider, &token, &store).await
}
