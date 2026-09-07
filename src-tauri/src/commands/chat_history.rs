//! Tauri commands for reading Claude Code chat history (`.jsonl` files
//! under `~/.claude/projects/*/`). Powers the MemoryExplorer's "Chats"
//! source.

use crate::memory::chat_history::{
    list_chats, read_chat, search_chats, ChatSearchHit, ChatSummary, ChatTranscript,
};
use std::path::{Path, PathBuf};

/// Resolve the canonical `~/.claude/projects` directory that chat transcripts
/// must live under. Returns `None` if the home directory can't be determined.
fn chats_root() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let root = home.join(".claude").join("projects");
    // Canonicalize so symlinks/`..` in the base itself are resolved.
    root.canonicalize().ok().or(Some(root))
}

/// Confine a caller-supplied transcript path to `~/.claude/projects`, rejecting
/// any path that escapes the directory (via `..`, symlinks, or absolute paths
/// elsewhere). Returns the canonicalized, confined path on success.
fn confine_chat_path(path: &Path) -> Result<PathBuf, String> {
    let root = chats_root().ok_or_else(|| "could not resolve chat history directory".to_string())?;
    // canonicalize resolves `..` and symlinks and requires the file to exist,
    // which prevents traversal to arbitrary locations on disk.
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("invalid chat path: {e}"))?;
    if !resolved.starts_with(&root) {
        return Err("chat path is outside the allowed chat history directory".to_string());
    }
    Ok(resolved)
}

#[tauri::command]
pub async fn list_claude_chats() -> Result<Vec<ChatSummary>, String> {
    Ok(list_chats())
}

#[tauri::command]
pub async fn get_claude_chat(path: String, max_turns: Option<usize>) -> Result<ChatTranscript, String> {
    let cap = max_turns.unwrap_or(500);
    let confined = confine_chat_path(&PathBuf::from(path))?;
    read_chat(&confined, cap).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_claude_chats(query: String, limit: Option<usize>) -> Result<Vec<ChatSearchHit>, String> {
    let limit = limit.unwrap_or(40);
    Ok(search_chats(&query, limit))
}
