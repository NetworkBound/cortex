//! Gitea backup auto-mirror.
//!
//! Backs up user's data to a self-hosted Gitea instance: `~/.cortex/`,
//! `~/.claude/projects/*/memory/*.md` (auto-memory only — never the huge
//! jsonl chat transcripts), and `~/Documents/Cortex Brain/`.
//!
//! Strategy: maintain a permanent git working copy at
//! `~/.cortex/gitea-mirror/`. On each backup we walk the source trees, copy
//! new/changed files (per-file mtime + size check), prune anything that
//! disappeared upstream (rsync `--delete` semantics), then
//! `git add . && git commit && git push`. If the Gitea repo doesn't exist
//! we create it via `/api/v1/user/repos`.
//!
//! Config persists at `~/.cortex/gitea-config.json`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

const CONFIG_NAME: &str = "gitea-config.json";
const MIRROR_DIR: &str = "gitea-mirror";
const SIZE_WARN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB
const MAX_TAIL_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GiteaConfig {
    pub base_url: String,
    pub token: String,
    pub owner: String,
    pub repo: String,
}

/// Persisted shape — adds enabled toggle + last-backup metadata so the UI
/// can show "Xm ago" without round-tripping to Gitea.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GiteaSettings {
    #[serde(default)] pub enabled: bool,
    #[serde(default)] pub base_url: String,
    #[serde(default)] pub token: String,
    #[serde(default)] pub owner: String,
    #[serde(default)] pub repo: String,
    #[serde(default)] pub last_backup_unix_ms: i64,
    #[serde(default)] pub last_report: Option<BackupReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackupReport {
    pub repo_url: String,
    pub commits_made: u32,
    pub files_added: u32,
    pub files_changed: u32,
    pub files_deleted: u32,
    pub bytes_total: u64,
    pub errors: Vec<String>,
    pub dry_run: bool,
    pub started_unix_ms: i64,
    pub finished_unix_ms: i64,
}

// ─────────── config persistence ───────────

fn cortex_home() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let dir = home.join(".cortex");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir ~/.cortex: {e}"))?;
    Ok(dir)
}
fn config_path() -> Result<PathBuf, String> { Ok(cortex_home()?.join(CONFIG_NAME)) }
fn mirror_path() -> Result<PathBuf, String> { Ok(cortex_home()?.join(MIRROR_DIR)) }

/// Returns defaults on missing/corrupt file so a half-write never locks the UI out.
pub fn load_settings() -> GiteaSettings {
    let Ok(p) = config_path() else { return GiteaSettings::default(); };
    let Ok(s) = fs::read_to_string(&p) else { return GiteaSettings::default(); };
    serde_json::from_str(&s).unwrap_or_default()
}
pub fn save_settings(s: &GiteaSettings) -> Result<(), String> {
    let p = config_path()?;
    let body = serde_json::to_string_pretty(s).map_err(|e| format!("serialize: {e}"))?;
    fs::write(&p, body).map_err(|e| format!("write {}: {e}", p.display()))
}

// ─────────── source roots + filters ───────────

/// Each tuple is `(absolute_root, mirror_subdir, file_filter)`.
fn source_roots() -> Vec<(PathBuf, &'static str, fn(&Path) -> bool)> {
    let mut out: Vec<(PathBuf, &'static str, fn(&Path) -> bool)> = Vec::new();
    let home = match dirs::home_dir() { Some(h) => h, None => return out };
    let cortex = home.join(".cortex");
    if cortex.exists() { out.push((cortex, "cortex", filter_cortex)); }
    let claude = home.join(".claude").join("projects");
    if claude.exists() { out.push((claude, "claude-memory", filter_claude_memory)); }
    let brain = home.join("Documents").join("Cortex Brain");
    if brain.exists() { out.push((brain, "brain", filter_brain)); }
    out
}

/// Skip local backups (already-archived), the mirror itself (recursive!), caches.
fn filter_cortex(rel: &Path) -> bool {
    let Some(c) = rel.components().next() else { return false };
    let first = c.as_os_str().to_string_lossy();
    !matches!(first.as_ref(), "backups" | "snapshots" | "cache" | ".cache" | MIRROR_DIR)
}

/// Only `<project_id>/memory/<file>.md` — never jsonl chat transcripts.
fn filter_claude_memory(rel: &Path) -> bool {
    let mut comps = rel.components();
    comps.next(); // project id
    let second = comps.next().map(|c| c.as_os_str().to_string_lossy().into_owned());
    if second.as_deref() != Some("memory") { return false; }
    rel.extension().and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("md")).unwrap_or(false)
}

/// Brain vault: skip Obsidian's workspace cache + trash.
fn filter_brain(rel: &Path) -> bool {
    let Some(c) = rel.components().next() else { return false };
    let first = c.as_os_str().to_string_lossy();
    !matches!(first.as_ref(), ".obsidian" | ".trash")
}

fn mtime_ms(p: &Path) -> i64 {
    fs::metadata(p).and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64).unwrap_or(0)
}

// ─────────── core sync ───────────

/// Walk one root → mirror. Returns the set of relative paths it owns
/// (used by the prune pass) and bumps report counters in place.
fn sync_one_root(
    src_root: &Path, subdir: &str, filter: fn(&Path) -> bool,
    mirror_root: &Path, report: &mut BackupReport, dry_run: bool,
) -> HashSet<PathBuf> {
    let dest_root = mirror_root.join(subdir);
    if !dry_run {
        if let Err(e) = fs::create_dir_all(&dest_root) {
            report.errors.push(format!("mkdir {}: {e}", dest_root.display()));
            return HashSet::new();
        }
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    for de in WalkDir::new(src_root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !de.file_type().is_file() { continue; }
        let abs = de.path();
        let rel = match abs.strip_prefix(src_root) { Ok(r) => r.to_path_buf(), Err(_) => continue };
        if !filter(&rel) { continue; }

        let dest = dest_root.join(&rel);
        seen.insert(PathBuf::from(subdir).join(&rel));

        let src_size = match fs::metadata(abs) { Ok(m) => m.len(), Err(_) => continue };
        report.bytes_total = report.bytes_total.saturating_add(src_size);

        let (needs_copy, is_new) = match fs::metadata(&dest) {
            Ok(d) => {
                let d_mt = d.modified().ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|x| x.as_millis() as i64).unwrap_or(0);
                (d.len() != src_size || mtime_ms(abs) > d_mt, false)
            }
            Err(_) => (true, true),
        };
        if !needs_copy { continue; }

        if is_new { report.files_added = report.files_added.saturating_add(1); }
        else      { report.files_changed = report.files_changed.saturating_add(1); }

        if dry_run { continue; }

        if let Some(parent) = dest.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                report.errors.push(format!("mkdir {}: {e}", parent.display()));
                continue;
            }
        }
        if let Err(e) = fs::copy(abs, &dest) {
            report.errors.push(format!("copy {} → {}: {e}", abs.display(), dest.display()));
        }
    }
    seen
}

/// rsync-style `--delete`: any mirror file no longer in `seen` gets removed.
/// `.git/` is preserved automatically since it lives at the mirror root,
/// not inside one of the tracked subdirs.
fn prune_missing(
    mirror_root: &Path, subdirs: &[&str], seen: &HashSet<PathBuf>,
    report: &mut BackupReport, dry_run: bool,
) {
    for subdir in subdirs {
        let dest_root = mirror_root.join(subdir);
        if !dest_root.exists() { continue; }
        for de in WalkDir::new(&dest_root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
            if !de.file_type().is_file() { continue; }
            let abs = de.path();
            let rel = match abs.strip_prefix(mirror_root) { Ok(r) => r.to_path_buf(), Err(_) => continue };
            if seen.contains(&rel) { continue; }
            report.files_deleted = report.files_deleted.saturating_add(1);
            if dry_run { continue; }
            if let Err(e) = fs::remove_file(abs) {
                report.errors.push(format!("rm {}: {e}", abs.display()));
            }
        }
    }
}

// ─────────── git + Gitea API ───────────

fn git(args: &[&str], cwd: &Path) -> Result<(bool, String, String), String> {
    let out = crate::sys::no_window("git").args(args).current_dir(cwd).output()
        .map_err(|e| format!("git {args:?} spawn: {e}"))?;
    Ok((out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// Init the mirror working copy if needed. Idempotent — safe every backup.
fn ensure_repo(mirror: &Path, config: &GiteaConfig) -> Result<(), String> {
    fs::create_dir_all(mirror).map_err(|e| format!("mkdir mirror: {e}"))?;
    if !mirror.join(".git").exists() {
        let (ok, _, err) = git(&["init", "-b", "main"], mirror)?;
        if !ok { return Err(format!("git init failed: {err}")); }
    }
    let remote = build_remote_url(config);
    let (has_origin, _, _) = git(&["remote", "get-url", "origin"], mirror)?;
    if has_origin {
        let _ = git(&["remote", "set-url", "origin", &remote], mirror)?;
    } else {
        let (ok, _, err) = git(&["remote", "add", "origin", &remote], mirror)?;
        if !ok { return Err(format!("git remote add: {err}")); }
    }
    // Gitea rejects anonymous commits in some setups — pin an identity.
    let email = format!("{}@cortex.local", config.owner);
    let _ = git(&["config", "user.email", &email], mirror);
    let _ = git(&["config", "user.name", "Cortex Backup"], mirror);
    Ok(())
}

/// Token-in-URL → avoids a credential helper. Caller redacts before logging.
fn build_remote_url(c: &GiteaConfig) -> String {
    let base = c.base_url.trim_end_matches('/');
    let (scheme, host) = match base.split_once("://") { Some((s, h)) => (s, h), None => ("http", base) };
    format!("{scheme}://{}@{host}/{}/{}.git", c.token, c.owner, c.repo)
}
fn build_web_url(c: &GiteaConfig) -> String {
    format!("{}/{}/{}", c.base_url.trim_end_matches('/'), c.owner, c.repo)
}

/// Probe `/api/v1/repos/...`; create via `/api/v1/user/repos` if missing.
/// Failures here are downgraded to non-fatal — `git push` will surface a
/// clearer error if creation was truly required.
async fn ensure_remote_repo(config: &GiteaConfig) -> Result<(), String> {
    let base = config.base_url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15)).build()
        .map_err(|e| format!("reqwest build: {e}"))?;
    let auth = format!("token {}", config.token);

    let check = format!("{base}/api/v1/repos/{}/{}", config.owner, config.repo);
    if let Ok(r) = client.get(&check).header("Authorization", &auth).send().await {
        if r.status().is_success() { return Ok(()); }
    }

    let create = format!("{base}/api/v1/user/repos");
    let body = serde_json::json!({
        "name": config.repo, "private": true, "auto_init": true,
        "default_branch": "main", "description": "Cortex backup mirror (auto-managed)",
    });
    let r = client.post(&create).header("Authorization", &auth).json(&body)
        .send().await.map_err(|e| format!("create repo: {e}"))?;
    if !r.status().is_success() {
        let status = r.status();
        let txt = r.text().await.unwrap_or_default();
        let snippet = truncate_on_char_boundary(&txt, MAX_TAIL_BYTES);
        return Err(format!("Gitea create-repo {status}: {snippet}"));
    }
    Ok(())
}

/// Truncate `s` to at most `max` bytes, clamping down to the nearest UTF-8
/// char boundary so slicing never panics on multibyte responses.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn detect_default_branch(mirror: &Path) -> String {
    if let Ok((ok, out, _)) = git(&["symbolic-ref", "--short", "HEAD"], mirror) {
        let t = out.trim();
        if ok && !t.is_empty() { return t.to_string(); }
    }
    "main".to_string()
}

fn redact(s: &str, token: &str) -> String {
    if token.is_empty() { s.to_string() } else { s.replace(token, "***") }
}

// ─────────── public entry point ───────────

/// One backup cycle. `dry_run` walks + counts without writing the mirror or pushing.
pub async fn run_backup(config: GiteaConfig, dry_run: bool) -> Result<BackupReport, String> {
    let started = Utc::now().timestamp_millis();
    let mut report = BackupReport {
        repo_url: build_web_url(&config), dry_run, started_unix_ms: started,
        ..Default::default()
    };
    let mirror = mirror_path()?;

    if !dry_run {
        if let Err(e) = ensure_remote_repo(&config).await {
            // Non-fatal — keep going so a bad token doesn't block local mirroring.
            report.errors.push(format!("ensure_remote_repo: {e}"));
        }
        ensure_repo(&mirror, &config)?;
    } else {
        // Dry-run still needs the dir present for the diff comparison.
        fs::create_dir_all(&mirror).map_err(|e| format!("mkdir mirror: {e}"))?;
    }

    let roots = source_roots();
    if roots.is_empty() {
        report.errors.push("no source roots exist on disk".into());
        report.finished_unix_ms = Utc::now().timestamp_millis();
        return Ok(report);
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut subdirs: Vec<&'static str> = Vec::new();
    for (src, sub, filter) in &roots {
        subdirs.push(*sub);
        seen.extend(sync_one_root(src, sub, *filter, &mirror, &mut report, dry_run));
    }
    prune_missing(&mirror, &subdirs, &seen, &mut report, dry_run);

    if report.bytes_total > SIZE_WARN_BYTES {
        let gb = report.bytes_total as f64 / 1024.0 / 1024.0 / 1024.0;
        tracing::warn!(
            "gitea_backup: payload is {:.2} GB — expand Gitea storage; Cortex won't auto-resize remote disks",
            gb);
    }

    if dry_run {
        report.finished_unix_ms = Utc::now().timestamp_millis();
        return Ok(report);
    }

    let _ = git(&["add", "."], &mirror)?;
    let msg = format!("cortex backup {}", Utc::now().to_rfc3339());
    let (commit_ok, _, commit_err) = git(&["commit", "-m", &msg], &mirror)?;
    if commit_ok {
        report.commits_made = 1;
    } else if !commit_err.to_lowercase().contains("nothing") && !commit_err.is_empty() {
        // "nothing to commit" is the no-op path, not a real error.
        report.errors.push(format!("git commit: {commit_err}"));
    }

    if report.commits_made > 0 {
        let branch = detect_default_branch(&mirror);
        let (push_ok, _, push_err) = git(&["push", "-u", "origin", &branch], &mirror)?;
        if !push_ok {
            report.errors.push(format!("git push: {}", redact(&push_err, &config.token)));
        }
    }

    report.finished_unix_ms = Utc::now().timestamp_millis();
    Ok(report)
}

// ─────────── Tauri command surface ───────────

#[tauri::command]
pub async fn gitea_get_settings() -> Result<GiteaSettings, String> { Ok(load_settings()) }

#[tauri::command]
pub async fn gitea_set_settings(settings: GiteaSettings) -> Result<(), String> {
    save_settings(&settings)
}

#[tauri::command]
pub async fn gitea_backup(config: GiteaConfig, dry_run: bool) -> Result<BackupReport, String> {
    let report = run_backup(config, dry_run).await?;
    if !dry_run {
        // Persist last-report so the UI's "last backup" widget survives a restart.
        let mut s = load_settings();
        s.last_backup_unix_ms = report.finished_unix_ms;
        s.last_report = Some(report.clone());
        let _ = save_settings(&s);
    }
    Ok(report)
}

/// Convenience for `/backup-now` + the scheduler: pulls config from saved settings.
#[tauri::command]
pub async fn gitea_backup_now() -> Result<BackupReport, String> {
    let s = load_settings();
    if !s.enabled {
        return Err("Gitea backup is disabled — enable it in the panel first.".into());
    }
    if s.base_url.is_empty() || s.token.is_empty() || s.owner.is_empty() || s.repo.is_empty() {
        return Err("Gitea backup is missing required settings (base_url / token / owner / repo).".into());
    }
    let cfg = GiteaConfig {
        base_url: s.base_url.clone(), token: s.token.clone(),
        owner: s.owner.clone(), repo: s.repo.clone(),
    };
    gitea_backup(cfg, false).await
}

/// Periodic loop. Runs every 6h when `enabled=true`. Re-reads settings each
/// tick so toggling enabled / editing credentials takes effect without a
/// restart. Uses Tauri's async runtime (NOT `tokio::spawn`) — the setup
/// hook isn't inside a Tokio reactor.
pub fn spawn_scheduler(_app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Don't compete with first-run setup IO.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
        interval.tick().await; // skip the immediate initial tick
        loop {
            interval.tick().await;
            let s = load_settings();
            if !s.enabled { continue; }
            if s.base_url.is_empty() || s.token.is_empty()
                || s.owner.is_empty() || s.repo.is_empty() { continue; }
            let cfg = GiteaConfig {
                base_url: s.base_url.clone(), token: s.token.clone(),
                owner: s.owner.clone(), repo: s.repo.clone(),
            };
            match run_backup(cfg, false).await {
                Ok(report) => {
                    let mut latest = load_settings();
                    latest.last_backup_unix_ms = report.finished_unix_ms;
                    latest.last_report = Some(report);
                    let _ = save_settings(&latest);
                }
                Err(e) => tracing::warn!("gitea scheduled backup failed: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_cortex_skips_backups_and_mirror() {
        assert!(!filter_cortex(Path::new("backups/x.tar.gz")));
        assert!(!filter_cortex(Path::new("gitea-mirror/.git/HEAD")));
        assert!(!filter_cortex(Path::new("cache/blob")));
        assert!(filter_cortex(Path::new("snippets.json")));
    }
    #[test]
    fn filter_claude_memory_only_md_under_memory() {
        assert!(filter_claude_memory(Path::new("proj-id/memory/note.md")));
        assert!(!filter_claude_memory(Path::new("proj-id/memory/note.jsonl")));
        assert!(!filter_claude_memory(Path::new("proj-id/sessions/chat.jsonl")));
        assert!(!filter_claude_memory(Path::new("proj-id/memory.md")));
    }
    #[test]
    fn filter_brain_skips_obsidian_workspace() {
        assert!(!filter_brain(Path::new(".obsidian/workspace.json")));
        assert!(!filter_brain(Path::new(".trash/deleted.md")));
        assert!(filter_brain(Path::new("journal/2026-05-27.md")));
    }
    #[test]
    fn build_urls_strip_trailing_slash() {
        let c = GiteaConfig {
            base_url: "https://gitea.example.com/".into(), token: "tok".into(),
            owner: "user".into(), repo: "cortex-backup".into(),
        };
        assert_eq!(build_web_url(&c), "https://gitea.example.com/user/cortex-backup");
        assert!(build_remote_url(&c).contains("tok@gitea.example.com"));
    }
    #[test]
    fn redact_replaces_token() {
        assert_eq!(redact("error: tok=abc push failed", "abc"), "error: tok=*** push failed");
        assert_eq!(redact("no token here", ""), "no token here");
    }
}
