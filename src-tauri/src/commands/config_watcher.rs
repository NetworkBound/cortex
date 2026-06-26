//! Hot-reload watcher for `~/.cortex/*.json` (+ a few yaml/md siblings).
//!
//! Watches the user-global Cortex config directory and emits a
//! `config-changed` window event whenever a known config file is created,
//! modified, or deleted. Future consumers (snippets panel, trust matrix,
//! theme picker, …) can subscribe via `subscribeConfigChanges` in
//! `src/lib/config-watcher.ts` and refresh their cached state — this commit
//! only ships the event stream, no consumers are wired yet.
//!
//! Implementation mirrors `repo_map::watcher` (notify -> tokio bridge,
//! 500ms per-path debounce) but limited to a single non-recursive top-level
//! dir plus a small allow-list of recursive subdirs (`tools/`, `roles/`,
//! `skills/`, `focus-chains/`, `workflows/`, `teams/`). Anything outside
//! the allow-list is dropped before emit so a noisy editor swap-file can't
//! flood the channel.
//!
//! IMPORTANT: spawned from the Tauri `setup` hook, which runs OUTSIDE a
//! Tokio reactor — uses `tauri::async_runtime::spawn` (see preview.rs note).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use notify::{
    event::{CreateKind, ModifyKind, RemoveKind},
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

/// Debounce window for collapsing rapid filesystem bursts.
const DEBOUNCE_MS: u64 = 500;

/// Top-level files in `~/.cortex/` that we surface. Exact-match basenames.
const TOP_LEVEL_FILES: &[&str] = &[
    "snippets.json",
    "agent-instructions.json",
    "agents-instructions.json",
    "auto-approve.json",
    "trust-matrix.json",
    "webhooks.json",
    "themes.json",
];

/// Subdirs inside `~/.cortex/` whose contents we watch recursively. Files
/// within are filtered by extension below.
const SUB_DIRS: &[&str] = &[
    "tools",
    "roles",
    "skills",
    "focus-chains",
    "workflows",
    "teams",
];

/// File extensions accepted inside the recursive sub-dirs. `SKILL.md` lives
/// under `skills/<name>/SKILL.md` so we additionally allow `.md` for the
/// `skills` subtree.
const ALLOWED_EXTS: &[&str] = &["json", "yaml", "yml", "md"];

/// Payload emitted on the `config-changed` window event. Mirrors
/// `ConfigChangedEvent` in `src/lib/config-watcher.ts`.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigChangedEvent {
    /// One of "created", "modified", "deleted".
    pub kind: String,
    /// Absolute path of the changed config file.
    pub path: String,
    /// Unix epoch milliseconds.
    pub ts: i64,
}

/// Status surface returned by the `config_watcher_status` command.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigWatcherStatus {
    pub active: bool,
    pub watched_paths: Vec<PathBuf>,
}

/// Internal handle to the running watcher.
struct WatcherHandle {
    stop_tx: oneshot::Sender<()>,
    watched_paths: Vec<PathBuf>,
}

static WATCHER: OnceCell<Arc<Mutex<Option<WatcherHandle>>>> = OnceCell::new();

fn slot() -> Arc<Mutex<Option<WatcherHandle>>> {
    WATCHER
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Compute `~/.cortex/`. Errors when no home dir is available.
fn cortex_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("config_watcher: no home dir")?;
    Ok(home.join(".cortex"))
}

/// Decide whether a changed path is one of the files we care about.
fn is_watched_path(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let comps: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    if comps.is_empty() {
        return false;
    }

    // Top-level file?
    if comps.len() == 1 {
        let name = comps[0].to_string_lossy();
        return TOP_LEVEL_FILES.iter().any(|n| *n == name.as_ref());
    }

    // Inside a known sub-dir?
    let first = comps[0].to_string_lossy();
    if !SUB_DIRS.iter().any(|d| *d == first.as_ref()) {
        return false;
    }

    // Filter by extension. `.md` only counts under the `skills` subtree to
    // avoid stray README/CHANGELOG churn outside skills.
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
    else {
        return false;
    };
    if ext == "md" {
        return first == "skills";
    }
    ALLOWED_EXTS.iter().any(|e| *e == ext.as_str()) && ext != "md"
}

fn classify_kind(kind: &EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Create(CreateKind::File)
        | EventKind::Create(CreateKind::Any)
        | EventKind::Create(CreateKind::Folder) => Some("created"),
        EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Any)
        | EventKind::Modify(ModifyKind::Name(_)) => Some("modified"),
        EventKind::Remove(RemoveKind::File)
        | EventKind::Remove(RemoveKind::Folder)
        | EventKind::Remove(RemoveKind::Any) => Some("deleted"),
        _ => None,
    }
}

/// Start the config watcher. Idempotent — replaces any running instance.
/// Auto-creates `~/.cortex/` if it doesn't exist yet so a fresh install
/// still gets a live event stream once the user lands a config file.
pub fn start(app: AppHandle) -> Result<()> {
    // Replace any existing instance first.
    stop();

    let root = cortex_dir()?;
    if !root.exists() {
        std::fs::create_dir_all(&root).with_context(|| {
            format!("config_watcher: failed to create {}", root.display())
        })?;
    }
    if !root.is_dir() {
        anyhow::bail!(
            "config_watcher: {} exists but is not a directory",
            root.display()
        );
    }

    let root_for_filter = root.clone();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<(String, PathBuf)>();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else {
                return;
            };
            let Some(kind) = classify_kind(&event.kind) else {
                return;
            };
            for path in event.paths {
                if !is_watched_path(&path, &root_for_filter) {
                    continue;
                }
                let _ = event_tx.send((kind.to_string(), path));
            }
        })
        .context("config_watcher: failed to create notify watcher")?;

    // Watch the top-level dir non-recursively for the flat files, then
    // recursively descend each known sub-dir if it exists. Missing sub-dirs
    // are tolerated — they'll start firing as soon as someone creates them.
    let mut watched_paths: Vec<PathBuf> = Vec::new();
    watcher
        .watch(&root, RecursiveMode::NonRecursive)
        .with_context(|| format!("config_watcher: failed to watch {}", root.display()))?;
    watched_paths.push(root.clone());

    for sub in SUB_DIRS {
        let p = root.join(sub);
        if p.is_dir() {
            if let Err(e) = watcher.watch(&p, RecursiveMode::Recursive) {
                tracing::warn!(
                    "config_watcher: failed to watch {}: {e}",
                    p.display()
                );
            } else {
                watched_paths.push(p);
            }
        }
    }

    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

    // The notify watcher must outlive the task. Wrap in an Arc<Mutex> so the
    // task can drop it cleanly on stop.
    let watcher_holder = Arc::new(Mutex::new(Some(watcher)));
    let watcher_holder_task = watcher_holder.clone();

    // CRITICAL: setup hook runs OUTSIDE a Tokio reactor. Use Tauri's runtime
    // wrapper — bare `tokio::spawn` panics with "no reactor running"
    // (see preview.rs lines 112-117 for the same fix).
    tauri::async_runtime::spawn(async move {
        let mut pending: HashMap<PathBuf, (String, tokio::time::Instant)> = HashMap::new();
        let debounce = Duration::from_millis(DEBOUNCE_MS);

        loop {
            let next_deadline = pending.values().map(|(_, t)| *t).min();
            let sleep_fut: tokio::time::Sleep = match next_deadline {
                Some(d) => tokio::time::sleep_until(d + debounce),
                None => tokio::time::sleep(Duration::from_secs(3600)),
            };
            tokio::pin!(sleep_fut);

            tokio::select! {
                _ = &mut stop_rx => {
                    tracing::info!("config_watcher: stop signal received");
                    break;
                }
                maybe = event_rx.recv() => {
                    match maybe {
                        Some((kind, path)) => {
                            pending.insert(path, (kind, tokio::time::Instant::now()));
                        }
                        None => {
                            tracing::warn!("config_watcher: event channel closed");
                            break;
                        }
                    }
                }
                _ = &mut sleep_fut => {
                    let now = tokio::time::Instant::now();
                    let ready: Vec<(PathBuf, String)> = pending
                        .iter()
                        .filter(|(_, (_, t))| now.duration_since(*t) >= debounce)
                        .map(|(p, (k, _))| (p.clone(), k.clone()))
                        .collect();
                    for (path, kind) in ready {
                        pending.remove(&path);
                        let ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        let payload = ConfigChangedEvent {
                            kind,
                            path: path.display().to_string(),
                            ts,
                        };
                        if let Err(e) = app.emit("config-changed", &payload) {
                            tracing::warn!("config_watcher: emit failed: {e}");
                        }
                    }
                }
            }
        }

        // Release the OS-level watch handles.
        drop(watcher_holder_task.lock().take());
    });

    {
        let cell = slot();
        let mut g = cell.lock();
        *g = Some(WatcherHandle {
            stop_tx,
            watched_paths,
        });
    }
    // Drop the local Arc — the task owns the surviving clone.
    let _ = watcher_holder;

    Ok(())
}

/// Stop the watcher if running. Returns `true` if we actually stopped one.
pub fn stop() -> bool {
    let cell = slot();
    let mut g = cell.lock();
    if let Some(handle) = g.take() {
        let _ = handle.stop_tx.send(());
        true
    } else {
        false
    }
}

/// Snapshot of the watcher's current state. Used by the frontend dev tab to
/// confirm the event stream is alive.
fn snapshot_status() -> ConfigWatcherStatus {
    let cell = slot();
    let g = cell.lock();
    match g.as_ref() {
        Some(h) => ConfigWatcherStatus {
            active: true,
            watched_paths: h.watched_paths.clone(),
        },
        None => ConfigWatcherStatus {
            active: false,
            watched_paths: Vec::new(),
        },
    }
}

// ────────────────────────────── tauri commands ─────────────────────────────

/// Stop the config watcher. Returns `true` if a watcher was actually stopped.
#[tauri::command]
pub async fn stop_config_watcher() -> Result<bool, String> {
    Ok(stop())
}

/// Snapshot of the current watcher state.
#[tauri::command]
pub async fn config_watcher_status() -> Result<ConfigWatcherStatus, String> {
    Ok(snapshot_status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watches_top_level_known_files() {
        let root = PathBuf::from("/home/x/.cortex");
        assert!(is_watched_path(
            &root.join("snippets.json"),
            &root
        ));
        assert!(is_watched_path(
            &root.join("trust-matrix.json"),
            &root
        ));
        assert!(is_watched_path(
            &root.join("agent-instructions.json"),
            &root
        ));
    }

    #[test]
    fn ignores_unknown_top_level_files() {
        let root = PathBuf::from("/home/x/.cortex");
        assert!(!is_watched_path(&root.join("random.json"), &root));
        assert!(!is_watched_path(&root.join("notes.txt"), &root));
    }

    #[test]
    fn watches_subdir_files_by_extension() {
        let root = PathBuf::from("/home/x/.cortex");
        assert!(is_watched_path(&root.join("tools/web.json"), &root));
        assert!(is_watched_path(&root.join("roles/dev.yaml"), &root));
        assert!(is_watched_path(&root.join("workflows/ship.yml"), &root));
        assert!(is_watched_path(
            &root.join("skills/my-skill/SKILL.md"),
            &root
        ));
        assert!(is_watched_path(
            &root.join("focus-chains/main.json"),
            &root
        ));
        assert!(is_watched_path(&root.join("teams/core.json"), &root));
    }

    #[test]
    fn rejects_md_outside_skills() {
        let root = PathBuf::from("/home/x/.cortex");
        assert!(!is_watched_path(&root.join("tools/README.md"), &root));
        assert!(!is_watched_path(&root.join("roles/notes.md"), &root));
    }

    #[test]
    fn rejects_unrelated_subdirs() {
        let root = PathBuf::from("/home/x/.cortex");
        assert!(!is_watched_path(&root.join("random/foo.json"), &root));
        assert!(!is_watched_path(&root.join("cache/x.json"), &root));
    }

    #[test]
    fn rejects_paths_outside_root() {
        let root = PathBuf::from("/home/x/.cortex");
        assert!(!is_watched_path(
            &PathBuf::from("/home/x/elsewhere/snippets.json"),
            &root
        ));
    }

    #[test]
    fn classify_known_kinds() {
        assert_eq!(
            classify_kind(&EventKind::Create(CreateKind::File)),
            Some("created")
        );
        assert_eq!(
            classify_kind(&EventKind::Modify(ModifyKind::Any)),
            Some("modified")
        );
        assert_eq!(
            classify_kind(&EventKind::Remove(RemoveKind::File)),
            Some("deleted")
        );
        assert_eq!(
            classify_kind(&EventKind::Access(notify::event::AccessKind::Any)),
            None
        );
    }
}
