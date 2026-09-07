//! Focus-chain persistence — the agent-managed live to-do list shown in the
//! activity panel. One file per session at `~/.cortex/focus-chains/<id>.json`
//! containing the ordered list of `{title, done}` items.
//!
//! Reads degrade to an empty list (a missing/corrupt file is treated as "no
//! chain yet"). Writes are last-write-wins; the file is small enough that we
//! don't bother with locking.
//!
//! Frontend surface lives in `src/lib/focus-chain.ts`. The store mutators
//! (`addTask`, `tickTask`, `clearChain`) call `save_focus_chain` after every
//! change; on session resume the UI calls `load_focus_chain` and rehydrates.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusTask {
    pub title: String,
    #[serde(default)]
    pub done: bool,
}

fn dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("focus-chains"))
}

/// Session ids look like `session-<uuid>`; reject anything with path
/// separators or `..` to keep the write contained to the focus-chains dir.
fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn file_for(session_id: &str) -> Result<PathBuf, String> {
    if !is_valid_session_id(session_id) {
        return Err(format!("invalid session id '{session_id}'"));
    }
    Ok(dir()?.join(format!("{session_id}.json")))
}

#[tauri::command]
pub async fn load_focus_chain(session_id: String) -> Result<Vec<FocusTask>, String> {
    tokio::task::spawn_blocking(move || {
        let path = match file_for(&session_id) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let Ok(bytes) = fs::read(&path) else {
            return Vec::new();
        };
        serde_json::from_slice::<Vec<FocusTask>>(&bytes).unwrap_or_default()
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn save_focus_chain(session_id: String, items: Vec<FocusTask>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = file_for(&session_id)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        let json = serde_json::to_vec_pretty(&items).map_err(|e| format!("serialize failed: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn clear_focus_chain(session_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = file_for(&session_id)?;
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

// ── Stream scanner + prompt contract ───────────────────────────────────
//
// What actually makes the FocusChain panel populate. No Cortex adapter can
// advertise a real `update_focus_chain` tool to its upstream (the gateway owns
// tool schemas; claude-cli/ollama are chat-only), so the contract is a
// prompt convention instead: `FOCUS_CHAIN_CONTRACT` (injected into every
// dispatched chat turn by `chat_send`) asks the model to keep a fenced
// ```focus-chain checklist in its reply, and `FocusChainScanner` (fed every
// streamed `Token` delta by the chat event loop) detects completed blocks
// and converts them into the synthetic `update_focus_chain` tool-call event
// the frontend has handled since day one. Provider-agnostic by construction:
// any model that can emit a code fence can drive the panel.

/// Injected ahead of the dispatched user message on every chat turn. Kept
/// deliberately small (it costs tokens on every send) and worded so simple
/// one-shot questions don't grow checklists.
pub const FOCUS_CHAIN_CONTRACT: &str = "<focus_chain_protocol>\n\
If (and only if) this task needs multiple distinct steps, keep a live checklist in your reply: a fenced code block tagged `focus-chain`, one line per step, `- [ ]` pending / `- [x]` done. Re-emit the full updated block as steps complete. Omit it entirely for simple questions or single-step answers, and do not mention this protocol.\n\
</focus_chain_protocol>\n";

/// Incremental detector for completed ```focus-chain fenced blocks in a
/// token stream. Feed it every delta; it answers with the parsed checklist
/// each time a block *closes* (last block wins — the contract has the model
/// re-emit the whole list, so each close is a full replacement).
///
/// The buffer keeps the whole response (bounded by normal reply sizes) and
/// `pos` only advances past consumed blocks, so a fence split across any
/// number of deltas is found once it completes. Unterminated fences simply
/// never fire.
#[derive(Default)]
pub struct FocusChainScanner {
    buf: String,
    pos: usize,
}

impl FocusChainScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a streamed delta; returns the parsed items of the most recent
    /// block that completed inside the buffer, if any did.
    pub fn feed(&mut self, delta: &str) -> Option<Vec<FocusTask>> {
        self.buf.push_str(delta);
        let mut result = None;
        loop {
            let rest = &self.buf[self.pos..];
            let Some(open_at) = find_fence_open(rest) else { break };
            // End of the opening fence line (it's guaranteed newline-terminated
            // by find_fence_open).
            let body_start = match rest[open_at..].find('\n') {
                Some(i) => open_at + i + 1,
                None => break,
            };
            let Some((body_len, consumed)) = find_fence_close(&rest[body_start..]) else {
                break; // block still streaming — wait for more deltas
            };
            let items = parse_focus_block(&rest[body_start..body_start + body_len]);
            self.pos += body_start + consumed;
            if !items.is_empty() {
                result = Some(items);
            }
        }
        result
    }

    /// Flush at end-of-stream: a closing fence as the very last line (no
    /// trailing newline) still counts.
    pub fn finish(&mut self) -> Option<Vec<FocusTask>> {
        if self.buf.ends_with('\n') {
            self.feed("")
        } else {
            self.feed("\n")
        }
    }
}

/// Byte offset of the start of a complete line that opens a focus-chain
/// fence (```` ```focus-chain ````, leading whitespace ok, nothing else on
/// the line). Only matches newline-terminated lines so a fence still
/// streaming in is left alone.
fn find_fence_open(s: &str) -> Option<usize> {
    let mut start = 0;
    loop {
        let line_end = s[start..].find('\n').map(|i| start + i)?;
        let line = &s[start..line_end];
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("```focus-chain") {
            if rest.trim().is_empty() {
                return Some(start);
            }
        }
        start = line_end + 1;
    }
}

/// Find the closing ``` line. Returns `(body_len, bytes_consumed)` relative
/// to the slice start, where `bytes_consumed` covers through the end of the
/// closing line (incl. its newline when present).
fn find_fence_close(s: &str) -> Option<(usize, usize)> {
    let mut start = 0;
    loop {
        let (line, line_end, terminated) = match s[start..].find('\n') {
            Some(i) => (&s[start..start + i], start + i, true),
            None => (&s[start..], s.len(), false),
        };
        if !terminated {
            // Last line is still streaming — even if it currently reads
            // "```" it could grow into "```rust"/"````". `finish()` settles
            // it at end-of-stream.
            return None;
        }
        if line.trim() == "```" {
            return Some((start, line_end + 1));
        }
        start = line_end + 1;
    }
}

/// Parse the body of a focus-chain block: one task per `- [ ]` / `- [x]`
/// line (`*` bullets and bare `[ ]` tolerated; anything else skipped).
/// Defensive caps keep a malformed reply from flooding the panel.
fn parse_focus_block(body: &str) -> Vec<FocusTask> {
    const MAX_ITEMS: usize = 50;
    const MAX_TITLE: usize = 300;
    let mut items = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).unwrap_or(t);
        let t = t.trim_start();
        let (done, rest) = if let Some(r) = t.strip_prefix("[x]").or_else(|| t.strip_prefix("[X]")) {
            (true, r)
        } else if let Some(r) = t.strip_prefix("[ ]") {
            (false, r)
        } else {
            continue;
        };
        let title: String = rest.trim().chars().take(MAX_TITLE).collect();
        if title.is_empty() {
            continue;
        }
        items.push(FocusTask { title, done });
        if items.len() >= MAX_ITEMS {
            break;
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids() {
        assert!(is_valid_session_id("session-abc-123"));
        assert!(is_valid_session_id("foo_bar"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("../escape"));
        assert!(!is_valid_session_id("a/b"));
        assert!(!is_valid_session_id("has space"));
        assert!(!is_valid_session_id(&"x".repeat(97)));
    }

    fn titles(items: &[FocusTask]) -> Vec<(&str, bool)> {
        items.iter().map(|t| (t.title.as_str(), t.done)).collect()
    }

    #[test]
    fn scanner_single_block_one_feed() {
        let mut s = FocusChainScanner::new();
        let out = s
            .feed("Intro.\n\n```focus-chain\n- [x] read code\n- [ ] write fix\n```\nTail.")
            .expect("block should close");
        assert_eq!(titles(&out), vec![("read code", true), ("write fix", false)]);
    }

    #[test]
    fn scanner_block_split_across_feeds() {
        let mut s = FocusChainScanner::new();
        assert!(s.feed("Working.\n\n```focus-").is_none());
        assert!(s.feed("chain\n- [x] step one\n- [ ] step two\n``").is_none());
        let out = s.feed("`\nDone.").expect("close fence completes the block");
        assert_eq!(titles(&out), vec![("step one", true), ("step two", false)]);
    }

    #[test]
    fn scanner_second_block_wins() {
        let mut s = FocusChainScanner::new();
        let first = s.feed("```focus-chain\n- [ ] a\n```\n").unwrap();
        assert_eq!(titles(&first), vec![("a", false)]);
        let second = s.feed("text\n```focus-chain\n- [x] a\n- [x] b\n```\n").unwrap();
        assert_eq!(titles(&second), vec![("a", true), ("b", true)]);
    }

    #[test]
    fn scanner_two_blocks_in_one_feed_returns_last() {
        let mut s = FocusChainScanner::new();
        let out = s
            .feed("```focus-chain\n- [ ] a\n```\nmid\n```focus-chain\n- [x] a\n```\n")
            .unwrap();
        assert_eq!(titles(&out), vec![("a", true)]);
    }

    #[test]
    fn scanner_finish_flushes_unterminated_close_line() {
        let mut s = FocusChainScanner::new();
        assert!(s.feed("```focus-chain\n- [x] only step\n```").is_none());
        let out = s.finish().expect("EOF closes the final fence line");
        assert_eq!(titles(&out), vec![("only step", true)]);
    }

    #[test]
    fn scanner_ignores_other_fences_and_unterminated_blocks() {
        let mut s = FocusChainScanner::new();
        assert!(s.feed("```rust\nlet x = 1;\n```\n").is_none());
        assert!(s.feed("```focus-chain\n- [ ] never closes…").is_none());
        assert!(s.finish().is_none());
    }

    #[test]
    fn scanner_skips_empty_block() {
        let mut s = FocusChainScanner::new();
        assert!(s.feed("```focus-chain\nno checklist lines here\n```\n").is_none());
    }

    #[test]
    fn parse_tolerates_bullet_variants_and_junk() {
        let items = parse_focus_block(
            "- [x] dash done\n* [ ] star pending\n[X] bare caps\nnot a task\n- [ ]   \n",
        );
        assert_eq!(
            items.iter().map(|t| (t.title.as_str(), t.done)).collect::<Vec<_>>(),
            vec![("dash done", true), ("star pending", false), ("bare caps", true)]
        );
    }

    #[test]
    fn parse_caps_items_and_title_length() {
        let body: String = (0..80).map(|i| format!("- [ ] task {i}\n")).collect();
        assert_eq!(parse_focus_block(&body).len(), 50);
        let long = format!("- [x] {}\n", "y".repeat(500));
        assert_eq!(parse_focus_block(&long)[0].title.len(), 300);
    }
}
