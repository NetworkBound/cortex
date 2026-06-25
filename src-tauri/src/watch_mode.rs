//! Aider-style "watch mode" file watcher.
//!
//! When enabled, this watches a project root directory for saves to source
//! files (`.ts/.tsx/.js/.jsx/.rs/.py/.go/.md`). If a saved file contains an
//! `// AI!` (or `# AI!`) marker comment, the watcher emits a
//! `watch-mode-trigger` Tauri event so the frontend can auto-send a chat
//! message to the AI asking it to act on the comment.
//!
//! # Wiring (NOT done in this commit — apply manually)
//!
//! 1. `src-tauri/src/lib.rs` add at the top level (alongside the other
//!    `pub mod` declarations):
//!    ```ignore
//!    pub mod watch_mode;
//!    ```
//!
//! 2. `src-tauri/src/commands/mod.rs` add:
//!    ```ignore
//!    pub mod watch_mode;
//!    ```
//!
//! 3. `src-tauri/src/lib.rs` register the commands in `invoke_handler!`:
//!    ```ignore
//!    commands::watch_mode::start_watch_mode,
//!    commands::watch_mode::stop_watch_mode,
//!    commands::watch_mode::is_watch_mode_active,
//!    ```
//!
//! # Design
//!
//! - Uses the existing `notify = "7"` dependency (no new deps).
//! - The notify watcher runs in its own thread (notify owns it). Events are
//!   forwarded onto a `tokio::mpsc` channel and processed by a tokio task so
//!   we can do async work (debounce, file reads, emits) without blocking the
//!   filesystem callback.
//! - Per-file 500ms debounce: when an event lands, we record the timestamp;
//!   the task waits 500ms and only fires if no newer event arrived for that
//!   path.
//! - We only re-fire for a file when its content hash actually changed AND
//!   the file contains `AI!` markers.
//! - A `oneshot` channel is used as a shutdown signal so `stop()` is clean.
//!
//! # Stub for `cargo check`
//!
//! This module compiles standalone and is referenced only via the commands
//! module — but for `cargo check` to typecheck it during development you must
//! temporarily add `pub mod watch_mode;` to `lib.rs`. The header comment
//! above explains the permanent wiring.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use notify::{
    event::{CreateKind, ModifyKind},
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

/// Watched file extensions and the comment-marker style we expect inside them.
fn marker_style_for(path: &Path) -> Option<MarkerStyle> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "ts" | "tsx" | "js" | "jsx" | "rs" | "go" => Some(MarkerStyle::Slash),
        "py" | "md" => Some(MarkerStyle::Hash),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug)]
enum MarkerStyle {
    /// `// AI!` or `/* AI! */`
    Slash,
    /// `# AI!`
    Hash,
}

/// Payload emitted to the frontend on each detected marker.
#[derive(Debug, Clone, Serialize)]
struct WatchTriggerPayload {
    path: String,
    line: usize,
    marker: String,
    context: String,
    ts: u128,
}

/// Handle returned from [`start_watch`]. Drop or call [`WatchHandle::stop`] to
/// terminate the watcher.
pub struct WatchHandle {
    stop_tx: Option<oneshot::Sender<()>>,
}

impl WatchHandle {
    /// Signal the background task to shut down. Idempotent.
    pub fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Returns `true` if `line` looks like an `AI!` marker comment.
///
/// Matches (with optional surrounding whitespace):
/// - `// AI!`     (optionally followed by text)
/// - `/* AI! */`  (or `/* AI! ... */`)
/// - `# AI!`      (optionally followed by text)
pub fn looks_like_ai_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    // // AI!  or  //AI!  (we accept zero-or-more spaces between `//` and `AI!`)
    if let Some(rest) = trimmed.strip_prefix("//") {
        let rest = rest.trim_start();
        if rest.starts_with("AI!") {
            return true;
        }
    }
    // /* AI! ... */  or  /* AI!*/
    if let Some(rest) = trimmed.strip_prefix("/*") {
        let rest = rest.trim_start();
        if rest.starts_with("AI!") {
            return true;
        }
    }
    // # AI!
    if let Some(rest) = trimmed.strip_prefix('#') {
        let rest = rest.trim_start();
        if rest.starts_with("AI!") {
            return true;
        }
    }
    false
}

/// Start watching `project_root` recursively. Returns a [`WatchHandle`] that
/// terminates the watcher on `stop()` or drop.
pub fn start_watch(project_root: PathBuf, app: AppHandle) -> Result<WatchHandle> {
    if !project_root.is_dir() {
        anyhow::bail!(
            "watch_mode: project_root is not a directory: {}",
            project_root.display()
        );
    }

    // notify -> tokio bridge. The notify watcher runs on its own thread and
    // forwards events into this async channel.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<PathBuf>();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else {
                return;
            };
            if !is_interesting_kind(&event.kind) {
                return;
            }
            for path in event.paths {
                if marker_style_for(&path).is_some() {
                    let _ = event_tx.send(path);
                }
            }
        })
        .context("watch_mode: failed to create notify watcher")?;

    watcher
        .watch(&project_root, RecursiveMode::Recursive)
        .with_context(|| {
            format!(
                "watch_mode: failed to start watching {}",
                project_root.display()
            )
        })?;

    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

    // Per-path debounce + content hash state lives inside the task.
    let debounce = Duration::from_millis(500);
    // Keep watcher owned by the task so it lives for the watcher's lifetime.
    let watcher_owner = Arc::new(Mutex::new(Some(watcher)));
    let watcher_owner_for_task = watcher_owner.clone();

    tokio::spawn(async move {
        // last-seen event timestamps per path (used to coalesce bursts)
        let mut pending: HashMap<PathBuf, tokio::time::Instant> = HashMap::new();
        // last-emitted content hash per path so we don't re-fire on no-op saves
        let mut last_hash: HashMap<PathBuf, u64> = HashMap::new();

        loop {
            // Compute next wake time based on the soonest pending debounce.
            let next_deadline = pending.values().min().copied();

            let sleep_fut: tokio::time::Sleep = match next_deadline {
                Some(d) => tokio::time::sleep_until(d + debounce),
                None => tokio::time::sleep(Duration::from_secs(3600)),
            };
            tokio::pin!(sleep_fut);

            tokio::select! {
                _ = &mut stop_rx => {
                    tracing::info!("watch_mode: stop signal received");
                    break;
                }
                maybe_path = event_rx.recv() => {
                    match maybe_path {
                        Some(path) => {
                            pending.insert(path, tokio::time::Instant::now());
                        }
                        None => {
                            // Channel closed — watcher dropped on the notify side.
                            tracing::warn!("watch_mode: event channel closed");
                            break;
                        }
                    }
                }
                _ = &mut sleep_fut => {
                    // Drain any path whose debounce window has fully elapsed.
                    let now = tokio::time::Instant::now();
                    let ready: Vec<PathBuf> = pending
                        .iter()
                        .filter(|(_, t)| now.duration_since(**t) >= debounce)
                        .map(|(p, _)| p.clone())
                        .collect();
                    for path in ready {
                        pending.remove(&path);
                        if let Err(e) = process_path(&path, &app, &mut last_hash) {
                            tracing::warn!(
                                "watch_mode: failed to process {}: {e:#}",
                                path.display()
                            );
                        }
                    }
                }
            }
        }

        // Drop the watcher explicitly so the OS-level watch is released.
        drop(watcher_owner_for_task.lock().take());
    });

    // Keep the original Arc alive on the returning side via the closure;
    // the task above takes the only meaningful clone. The returned handle
    // doesn't carry the watcher because we want the *task* to own its
    // lifecycle (so dropping the handle ends the task, which drops the
    // watcher).
    let _ = watcher_owner; // silence unused — task owns the clone.

    Ok(WatchHandle {
        stop_tx: Some(stop_tx),
    })
}

fn is_interesting_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(CreateKind::File)
            | EventKind::Create(CreateKind::Any)
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Any)
            // Some editors do atomic-replace saves which surface as Name(_).
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn process_path(
    path: &Path,
    app: &AppHandle,
    last_hash: &mut HashMap<PathBuf, u64>,
) -> Result<()> {
    let style = match marker_style_for(path) {
        Some(s) => s,
        None => return Ok(()),
    };

    // Read the file. If it disappeared (rename-mid-save races), skip silently.
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(anyhow::Error::from(e)),
    };

    // Skip non-UTF8 / binary blobs.
    let contents = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    // Fast reject: if the file doesn't contain "AI!" at all, drop it.
    if !contents.contains("AI!") {
        // Still update hash so a later edit that *adds* AI! re-fires once.
        last_hash.insert(path.to_path_buf(), hash_content(contents));
        return Ok(());
    }

    let new_hash = hash_content(contents);
    if last_hash.get(path) == Some(&new_hash) {
        return Ok(());
    }
    last_hash.insert(path.to_path_buf(), new_hash);

    let style_str = match style {
        MarkerStyle::Slash => "//",
        MarkerStyle::Hash => "#",
    };

    let lines: Vec<&str> = contents.lines().collect();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    for (idx, line) in lines.iter().enumerate() {
        if !looks_like_ai_marker(line) {
            continue;
        }
        // 8 lines centered on the match (4 above, the match, 3 below)
        let start = idx.saturating_sub(4);
        let end = (idx + 4).min(lines.len().saturating_sub(1));
        let excerpt = lines[start..=end].join("\n");

        let payload = WatchTriggerPayload {
            path: path.display().to_string(),
            line: idx + 1, // 1-indexed for humans
            marker: style_str.to_string(),
            context: excerpt,
            ts,
        };

        if let Err(e) = app.emit("watch-mode-trigger", &payload) {
            tracing::warn!("watch_mode: failed to emit trigger event: {e}");
        } else {
            tracing::info!(
                "watch_mode: AI! marker at {}:{}",
                payload.path,
                payload.line
            );
        }
    }

    Ok(())
}

/// FNV-1a 64-bit content hash. Cheap, no extra deps, good enough for the
/// "did the file actually change?" check.
fn hash_content(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_slash_variants() {
        assert!(looks_like_ai_marker("// AI!"));
        assert!(looks_like_ai_marker("    // AI! please refactor"));
        assert!(looks_like_ai_marker("//AI!"));
        assert!(looks_like_ai_marker("/* AI! rename this */"));
        assert!(looks_like_ai_marker("  /*   AI! */"));
    }

    #[test]
    fn marker_hash_variants() {
        assert!(looks_like_ai_marker("# AI!"));
        assert!(looks_like_ai_marker("    # AI! fix this"));
        assert!(looks_like_ai_marker("#AI!"));
    }

    #[test]
    fn non_markers() {
        assert!(!looks_like_ai_marker("// hello"));
        assert!(!looks_like_ai_marker("# regular comment"));
        assert!(!looks_like_ai_marker("println!(\"AI!\")")); // string literal, not a comment
        assert!(!looks_like_ai_marker(""));
    }

    #[test]
    fn ext_routing() {
        assert!(matches!(
            marker_style_for(Path::new("foo.rs")),
            Some(MarkerStyle::Slash)
        ));
        assert!(matches!(
            marker_style_for(Path::new("foo.py")),
            Some(MarkerStyle::Hash)
        ));
        assert!(matches!(
            marker_style_for(Path::new("foo.md")),
            Some(MarkerStyle::Hash)
        ));
        assert!(marker_style_for(Path::new("foo.bin")).is_none());
        assert!(marker_style_for(Path::new("noext")).is_none());
    }
}
