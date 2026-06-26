//! Linux-native end-to-end test bridge.
//!
//! The Windows audit path (`scripts/e2e-setup.md`) drives `cortex.exe` through
//! `tauri-driver` + `msedgedriver` against WebView2. On Linux the webview is
//! WebKitGTK, which speaks neither the W3C WebDriver dialect msedgedriver
//! expects nor full CDP — so there was *no* way to verify a Linux build
//! headlessly. That gap is exactly how the Fedora "black screen" shipped
//! unnoticed (the web process aborted on EGL init and nobody could tell from a
//! script).
//!
//! This bridge closes it without a browser driver: when the app is launched
//! with `CORTEX_E2E=1`, the renderer (`src/lib/e2e-probe.ts`) periodically
//! hands the backend a JSON snapshot of its own live state — DOM mounted, theme
//! applied, gateway reachable, console errors — and we persist it to
//! `~/.cortex/e2e/snapshot.json`. A headless runner (`scripts/e2e-linux.mjs`)
//! launches the app, polls that file, and asserts on it.
//!
//! The key insight: the snapshot can only ever be written if the renderer's JS
//! actually ran, which on WebKitGTK means the web process is alive and
//! painting. A black-screen build writes *nothing* — the absence of a fresh
//! snapshot is itself the failure signal.
//!
//! FS access is mediated here (per the renderer's narrow `fs:scope`
//! capability), so the renderer can't touch `~/.cortex` directly — it goes
//! through `e2e_write_snapshot`.
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn e2e_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("e2e"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
pub struct E2eConfig {
    /// True when launched with `CORTEX_E2E=1` (or `=true`). The renderer only
    /// runs the probe loop when this is set, so production overhead is zero.
    pub enabled: bool,
    pub app_version: String,
    /// Whether the process is running as an AppImage (mirrors selfupdate's gate).
    pub is_appimage: bool,
}

/// True when launched with `CORTEX_E2E=1` (or `=true|yes|on`). Gates the
/// snapshot loop AND the fixture commands below — none of them may run in a
/// normal session. `pub(crate)` so other modules (routines' deterministic
/// fake-LLM markers) can share the exact same gate.
pub(crate) fn e2e_enabled() -> bool {
    std::env::var("CORTEX_E2E")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}

/// Renderer asks at boot whether E2E mode is armed.
#[tauri::command]
pub async fn e2e_config() -> Result<E2eConfig, String> {
    let enabled = e2e_enabled();
    let is_appimage = std::env::var("APPIMAGE")
        .ok()
        .filter(|p| !p.is_empty())
        .is_some();
    Ok(E2eConfig {
        enabled,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        is_appimage,
    })
}

/// Persist a renderer snapshot to `~/.cortex/e2e/snapshot.json` (atomic).
///
/// The renderer owns the payload shape (see `e2e-probe.ts`); we wrap it with
/// server-stamped fields the renderer can't trust itself for (`received_at`,
/// `pid`, `app_version`) so the runner can tell a fresh snapshot from a stale
/// one left over from a previous run.
#[tauri::command]
pub async fn e2e_write_snapshot(payload: serde_json::Value) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let dir = e2e_dir()?;
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;

        let envelope = serde_json::json!({
            "received_at": now_ms(),
            "pid": std::process::id(),
            "app_version": env!("CARGO_PKG_VERSION"),
            "snapshot": payload,
        });
        let json = serde_json::to_vec_pretty(&envelope)
            .map_err(|e| format!("serialize failed: {e}"))?;

        // Atomic: write to a temp sibling then rename, so a polling reader never
        // observes a half-written file.
        let dest = dir.join("snapshot.json");
        let tmp = dir.join(format!("snapshot.json.{}.tmp", std::process::id()));
        fs::write(&tmp, &json).map_err(|e| format!("write failed: {e}"))?;
        fs::rename(&tmp, &dest).map_err(|e| format!("rename failed: {e}"))?;
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Clone-flow fixtures (probe flow #5: Setup "Clone & connect" → project).
//
// The probe needs a REAL local git repo to clone from. These two commands are
// hard-gated on CORTEX_E2E and confined to `~/.cortex/e2e/fixtures/`, so a
// production session can neither create nor delete anything through them.
// `make` also snapshots `~/.cortex/last-project.json` and `cleanup` restores
// it, because the flow exercises the real `set_active_project` hand-off and
// must not clobber the user's persisted project choice.
// ---------------------------------------------------------------------------

fn fixtures_dir() -> Result<PathBuf, String> {
    Ok(e2e_dir()?.join("fixtures"))
}

fn run_git(args: &[&str], cwd: &PathBuf) -> Result<(), String> {
    let out = crate::sys::no_window("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&"?"),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Like [`run_git`] but returns trimmed stdout (used to resolve commit hashes).
fn run_git_out(args: &[&str], cwd: &PathBuf) -> Result<String, String> {
    let out = crate::sys::no_window("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&"?"),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Create a throwaway source repo under the e2e fixtures dir and return
/// `{ src, dst }` — `dst` is a not-yet-existing sibling for `clone_git_repo`
/// to clone into. Snapshots `last-project.json` for later restore.
#[tauri::command]
pub async fn e2e_make_clone_fixture() -> Result<serde_json::Value, String> {
    if !e2e_enabled() {
        return Err("e2e mode not enabled".into());
    }
    tokio::task::spawn_blocking(move || {
        let dir = fixtures_dir()?;
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;

        // Snapshot the user's persisted active project (or record its absence).
        let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
        let last_project = home.join(".cortex").join("last-project.json");
        let backup = dir.join("last-project.pre-e2e.json");
        let absent_marker = dir.join("last-project.was-absent");
        if last_project.exists() {
            fs::copy(&last_project, &backup).map_err(|e| format!("backup failed: {e}"))?;
            let _ = fs::remove_file(&absent_marker);
        } else {
            fs::write(&absent_marker, b"").map_err(|e| format!("marker failed: {e}"))?;
            let _ = fs::remove_file(&backup);
        }

        let stamp = format!("{}-{}", std::process::id(), now_ms());
        let src = dir.join(format!("clone-src-{stamp}"));
        let dst = dir.join(format!("clone-dst-{stamp}"));
        fs::create_dir_all(&src).map_err(|e| format!("mkdir src failed: {e}"))?;
        run_git(&["init", "-q"], &src)?;
        fs::write(src.join("README.md"), b"# cortex e2e clone fixture\n")
            .map_err(|e| format!("write failed: {e}"))?;
        run_git(&["add", "-A"], &src)?;
        run_git(
            &[
                "-c",
                "user.email=e2e@cortex.local",
                "-c",
                "user.name=cortex-e2e",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "e2e fixture",
            ],
            &src,
        )?;
        Ok::<serde_json::Value, String>(serde_json::json!({
            "src": src.to_string_lossy(),
            "dst": dst.to_string_lossy(),
        }))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Delete a throwaway session the clone-flow probe created by bootstrapping
/// the fixture project (one system context message per run — without this,
/// nightly e2e runs would slowly fill the sessions list with dead
/// `clone-dst-*` rows). Gated on CORTEX_E2E like the other fixtures.
#[tauri::command]
pub async fn e2e_delete_session(
    session_id: String,
    store: tauri::State<'_, crate::observability::tracing_store::TracingStore>,
) -> Result<usize, String> {
    if !e2e_enabled() {
        return Err("e2e mode not enabled".into());
    }
    store
        .delete_session_messages(&session_id)
        .map_err(|e| e.to_string())
}

/// Remove the fixture repos, unregister `dst` from the project registry, and
/// restore the pre-flow `last-project.json`. Paths are canonicalized and must
/// live under the e2e fixtures dir — anything else is refused.
#[tauri::command]
pub async fn e2e_cleanup_clone_fixture(src: String, dst: String) -> Result<(), String> {
    if !e2e_enabled() {
        return Err("e2e mode not enabled".into());
    }
    tokio::task::spawn_blocking(move || {
        let dir = fixtures_dir()?;
        let dir_canon = dir
            .canonicalize()
            .map_err(|e| format!("fixtures dir unreadable: {e}"))?;

        for p in [&src, &dst] {
            let pb = PathBuf::from(p);
            // Unregister regardless of existence (matches stored canonical or
            // literal paths). Best-effort: a missing entry isn't an error.
            let _ = crate::projects::unregister_project_path(&pb);
            if pb.exists() {
                let canon = pb
                    .canonicalize()
                    .map_err(|e| format!("cannot resolve {p}: {e}"))?;
                if !canon.starts_with(&dir_canon) {
                    return Err(format!("refusing to remove path outside fixtures dir: {p}"));
                }
                fs::remove_dir_all(&canon).map_err(|e| format!("remove failed: {e}"))?;
            }
        }

        // Restore the user's persisted active project exactly as it was.
        let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
        let last_project = home.join(".cortex").join("last-project.json");
        let backup = dir.join("last-project.pre-e2e.json");
        let absent_marker = dir.join("last-project.was-absent");
        if backup.exists() {
            fs::rename(&backup, &last_project).map_err(|e| format!("restore failed: {e}"))?;
        } else if absent_marker.exists() {
            let _ = fs::remove_file(&last_project);
            let _ = fs::remove_file(&absent_marker);
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

// ---------------------------------------------------------------------------
// Git-history fixtures (probe flow: GitHistoryPanel "load-more + per-file diff
// navigation"). The clone fixture above is a single-commit repo, which can
// exercise neither deeper pagination nor a multi-file commit. This builds a
// small but realistic repo with FIVE commits — including one that touches two
// files and two that modify an existing file — so the probe can verify:
//   • `git_history` offset paging returns distinct deeper pages (load-more),
//   • `git_commit_files` lists every file a commit touched,
//   • `git_commit_file_diff` returns the unified diff for a single file.
// Hard-gated on CORTEX_E2E and confined to the e2e fixtures dir like the rest.
// ---------------------------------------------------------------------------

/// Build a multi-commit fixture repo and return `{ root, multi_hash, edit_hash }`.
/// `multi_hash` is the commit that added two files at once; `edit_hash` is the
/// commit that modified `a.txt` (so its single-file diff has real +/- rows).
#[tauri::command]
pub async fn e2e_make_history_fixture() -> Result<serde_json::Value, String> {
    if !e2e_enabled() {
        return Err("e2e mode not enabled".into());
    }
    tokio::task::spawn_blocking(move || {
        let dir = fixtures_dir()?;
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
        let stamp = format!("{}-{}", std::process::id(), now_ms());
        let root = dir.join(format!("history-{stamp}"));
        fs::create_dir_all(&root).map_err(|e| format!("mkdir root failed: {e}"))?;
        run_git(&["init", "-q"], &root)?;

        // Deterministic identity so the panel's author column is populated and
        // `git commit` never falls back to a host config that might be absent.
        let commit = |root: &PathBuf, msg: &str| -> Result<String, String> {
            run_git(&["add", "-A"], root)?;
            run_git(
                &[
                    "-c",
                    "user.email=e2e@cortex.local",
                    "-c",
                    "user.name=cortex-e2e",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-qm",
                    msg,
                ],
                root,
            )?;
            run_git_out(&["rev-parse", "HEAD"], root)
        };

        // c1: README — c2: a.txt — c3: b.txt + c.txt (multi-file) —
        // c4: edit a.txt — c5: edit README.
        fs::write(root.join("README.md"), b"# history fixture\n")
            .map_err(|e| format!("write failed: {e}"))?;
        commit(&root, "c1 readme")?;

        fs::write(root.join("a.txt"), b"alpha one\nalpha two\nalpha three\n")
            .map_err(|e| format!("write failed: {e}"))?;
        commit(&root, "c2 add a")?;

        fs::write(root.join("b.txt"), b"bravo\n").map_err(|e| format!("write failed: {e}"))?;
        fs::write(root.join("c.txt"), b"charlie\n").map_err(|e| format!("write failed: {e}"))?;
        let multi_hash = commit(&root, "c3 add bc")?;

        fs::write(
            root.join("a.txt"),
            b"alpha one\nalpha two CHANGED\nalpha three\n",
        )
        .map_err(|e| format!("write failed: {e}"))?;
        let edit_hash = commit(&root, "c4 edit a")?;

        fs::write(root.join("README.md"), b"# history fixture v2\n")
            .map_err(|e| format!("write failed: {e}"))?;
        commit(&root, "c5 edit readme")?;

        Ok::<serde_json::Value, String>(serde_json::json!({
            "root": root.to_string_lossy(),
            "multi_hash": multi_hash,
            "edit_hash": edit_hash,
        }))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Remove a git-history fixture repo. Path must resolve under the e2e fixtures
/// dir; anything else is refused. Gated on CORTEX_E2E.
#[tauri::command]
pub async fn e2e_cleanup_history_fixture(root: String) -> Result<(), String> {
    if !e2e_enabled() {
        return Err("e2e mode not enabled".into());
    }
    tokio::task::spawn_blocking(move || {
        let dir = fixtures_dir()?;
        let dir_canon = dir
            .canonicalize()
            .map_err(|e| format!("fixtures dir unreadable: {e}"))?;
        let pb = PathBuf::from(&root);
        if pb.exists() {
            let canon = pb
                .canonicalize()
                .map_err(|e| format!("cannot resolve {root}: {e}"))?;
            if !canon.starts_with(&dir_canon) {
                return Err(format!("refusing to remove path outside fixtures dir: {root}"));
            }
            fs::remove_dir_all(&canon).map_err(|e| format!("remove failed: {e}"))?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}
