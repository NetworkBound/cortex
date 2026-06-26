//! Claude-Code-style background monitors.
//!
//! Tails arbitrary commands (e.g. `npm test --watch`, `tail -F error.log`)
//! defined per-project in `<project_root>/.cortex/monitors/monitors.json` and
//! pipes their stdout/stderr line-by-line into a Tauri `monitor-line` event so
//! the frontend can surface them as synthetic chat messages.
//!
//! # Wiring (mirror watch_mode.rs)
//!
//! 1. `src-tauri/src/lib.rs`: `pub mod monitors;`
//! 2. `src-tauri/src/commands/mod.rs`: `pub mod monitors;`
//! 3. Register the three commands in `tauri::generate_handler!`:
//!    - `commands::monitors::start_monitors`
//!    - `commands::monitors::stop_monitors`
//!    - `commands::monitors::list_monitors`
//! 4. Hook `stop_all` into the Tauri `RunEvent::ExitRequested` / `Exit` so
//!    child processes are reaped on app shutdown.
//!
//! # Design
//!
//! - Each spawned process gets two tokio tasks (stdout / stderr) that read
//!   `BufReader::lines()` and forward each line to the main event channel.
//! - A process-wide registry (`OnceCell<Mutex<HashMap<String, ChildHandle>>>`)
//!   keys child handles by monitor name so `stop` can look them up by name.
//! - Rate limiting: each monitor allows up to 10 lines / 100ms (i.e. 100/sec)
//!   per stream — extra lines in that window are silently dropped and a single
//!   `[rate-limited]` notice is queued instead. Cheap, no extra deps.
//! - Shutdown is cooperative: `stop_all()` calls `child.kill()` and the
//!   forwarding tasks exit when the pipes close.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};

/// User-facing severity tag attached to each line. Frontend uses this to
/// colour-code the synthetic chat message.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MonitorLevel {
    Info,
    Warn,
    Error,
}

impl Default for MonitorLevel {
    fn default() -> Self {
        MonitorLevel::Info
    }
}

/// One row from `.cortex/monitors/monitors.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub level: MonitorLevel,
}

/// Payload emitted on every (non-rate-limited) line. Mirrors the frontend
/// `MonitorLinePayload` in `src/lib/monitors.ts`.
#[derive(Debug, Clone, Serialize)]
struct MonitorLinePayload {
    name: String,
    line: String,
    level: MonitorLevel,
    ts: u128,
}

/// Live handle for one spawned monitor. Killing the child causes its forward
/// tasks to drain remaining buffered lines and exit naturally.
struct ChildHandle {
    child: Child,
    forwarders: Vec<JoinHandle<()>>,
}

impl ChildHandle {
    async fn shutdown(mut self) {
        // Best-effort kill — the child may have already exited.
        let _ = self.child.kill().await;
        for f in self.forwarders {
            f.abort();
        }
    }
}

/// Process-wide registry. Keyed by monitor `name`.
static REGISTRY: OnceCell<Arc<Mutex<HashMap<String, ChildHandle>>>> = OnceCell::new();

fn registry() -> Arc<Mutex<HashMap<String, ChildHandle>>> {
    REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Absolute path to the monitors config for a project.
pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("monitors").join("monitors.json")
}

/// Parse `monitors.json` for the given project. Returns an empty vec if the
/// file is missing — only outright JSON errors propagate.
pub fn load_specs(project_root: &Path) -> Result<Vec<MonitorSpec>> {
    let path = config_path(project_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("monitors: failed to read {}", path.display()))?;
    let specs: Vec<MonitorSpec> = serde_json::from_str(&raw)
        .with_context(|| format!("monitors: failed to parse {}", path.display()))?;
    Ok(specs)
}

/// Start every monitor in `<project_root>/.cortex/monitors/monitors.json`.
/// Any previously running monitors are stopped first so this is safe to call
/// repeatedly. Returns the list of monitor names that were successfully
/// started.
pub async fn start_all(project_root: &Path, app: AppHandle) -> Result<Vec<String>> {
    stop_all().await;

    let specs = load_specs(project_root)?;
    if specs.is_empty() {
        return Ok(Vec::new());
    }

    let mut started = Vec::with_capacity(specs.len());
    for spec in specs {
        match spawn_one(&spec, project_root, app.clone()).await {
            Ok(handle) => {
                let name = spec.name.clone();
                registry().lock().insert(name.clone(), handle);
                started.push(name);
            }
            Err(e) => {
                tracing::warn!("monitors: failed to spawn {:?}: {e:#}", spec.name);
            }
        }
    }
    Ok(started)
}

/// Kill every running monitor. Idempotent.
pub async fn stop_all() {
    let handles: Vec<ChildHandle> = {
        let reg = registry();
        let mut map = reg.lock();
        map.drain().map(|(_, v)| v).collect()
    };
    for h in handles {
        h.shutdown().await;
    }
}

/// Spawn one monitor and wire its stdout/stderr to the Tauri event bus. The
/// returned [`ChildHandle`] owns the running process plus its forwarding tasks.
async fn spawn_one(spec: &MonitorSpec, cwd: &Path, app: AppHandle) -> Result<ChildHandle> {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("monitors: failed to spawn {:?}", spec.command))?;

    let stdout = child
        .stdout
        .take()
        .context("monitors: child stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("monitors: child stderr was not piped")?;

    // Both streams feed the same mpsc so we keep a single rate limiter per
    // monitor. The bounded buffer (256) backpressures away pathological output
    // bursts without blocking the child for long.
    let (tx, rx) = mpsc::channel::<(String, MonitorLevel)>(256);
    let stdout_tx = tx.clone();
    let stderr_tx = tx;
    let default_level = spec.level;

    let stdout_task: JoinHandle<()> = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if stdout_tx.send((line, default_level)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!("monitors: stdout read error: {e}");
                    break;
                }
            }
        }
    });

    let stderr_task: JoinHandle<()> = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    // stderr is bumped up one severity step from the spec's
                    // default — stderr from a `tail -F` warn monitor is
                    // surfaced as `error`.
                    let lvl = match default_level {
                        MonitorLevel::Info => MonitorLevel::Warn,
                        _ => MonitorLevel::Error,
                    };
                    if stderr_tx.send((line, lvl)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!("monitors: stderr read error: {e}");
                    break;
                }
            }
        }
    });

    let emitter_task = spawn_emitter(spec.name.clone(), app, rx);

    Ok(ChildHandle {
        child,
        forwarders: vec![stdout_task, stderr_task, emitter_task],
    })
}

/// Drains the per-monitor channel and emits `monitor-line` events. Applies a
/// 100-line-per-100ms hard cap; extra lines in that window are dropped and a
/// single `[rate-limited]` notice is emitted instead.
fn spawn_emitter(
    name: String,
    app: AppHandle,
    mut rx: mpsc::Receiver<(String, MonitorLevel)>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let window = Duration::from_millis(100);
        let max_per_window: u32 = 10; // 10 / 100ms ⇒ 100 lines/sec
        let mut window_start = tokio::time::Instant::now();
        let mut count_in_window: u32 = 0;
        let mut suppressed_in_window: u32 = 0;

        while let Some((line, level)) = rx.recv().await {
            let now = tokio::time::Instant::now();
            if now.duration_since(window_start) >= window {
                // Flush a single rate-limit notice for the previous window if
                // we dropped anything.
                if suppressed_in_window > 0 {
                    emit_line(
                        &app,
                        &name,
                        format!("[rate-limited: dropped {suppressed_in_window} line(s)]"),
                        MonitorLevel::Warn,
                    );
                }
                window_start = now;
                count_in_window = 0;
                suppressed_in_window = 0;
            }

            if count_in_window >= max_per_window {
                suppressed_in_window = suppressed_in_window.saturating_add(1);
                continue;
            }
            count_in_window += 1;
            emit_line(&app, &name, line, level);
        }

        // Channel closed — flush any pending rate-limit notice.
        if suppressed_in_window > 0 {
            emit_line(
                &app,
                &name,
                format!("[rate-limited: dropped {suppressed_in_window} line(s)]"),
                MonitorLevel::Warn,
            );
        }
    })
}

fn emit_line(app: &AppHandle, name: &str, line: String, level: MonitorLevel) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let payload = MonitorLinePayload {
        name: name.to_string(),
        line,
        level,
        ts,
    };
    if let Err(e) = app.emit("monitor-line", &payload) {
        tracing::warn!("monitors: emit failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_minimal_spec() {
        let json = r#"[{"name":"t","command":"echo","args":["hi"]}]"#;
        let specs: Vec<MonitorSpec> = serde_json::from_str(json).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "t");
        assert_eq!(specs[0].level, MonitorLevel::Info);
    }

    #[test]
    fn parses_full_spec() {
        let json = r#"[{"name":"err","command":"tail","args":["-F","x"],"level":"error"}]"#;
        let specs: Vec<MonitorSpec> = serde_json::from_str(json).unwrap();
        assert_eq!(specs[0].level, MonitorLevel::Error);
    }

    #[test]
    fn load_specs_missing_is_empty() {
        let tmp = TempDir::new().unwrap();
        let specs = load_specs(tmp.path()).unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn load_specs_reads_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".cortex").join("monitors");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("monitors.json"),
            r#"[{"name":"tests","command":"npm","args":["test"]}]"#,
        )
        .unwrap();
        let specs = load_specs(tmp.path()).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, "npm");
    }
}
