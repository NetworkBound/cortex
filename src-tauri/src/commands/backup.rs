//! Full Cortex backup + restore. Captures `~/.cortex/*` (every user-level
//! config — snippets, agent-instructions, auto-approve, trust-matrix,
//! webhooks, themes, hooks, skills, roles, focus-chains, tools, teams,
//! workflows, AGENTS.md, cortexignore, voice models marker, prps/, etc.) AND
//! `~/.claude/projects/*/memory/*.md` (auto-memory entries; never the jsonl
//! chat transcripts). Tarballs are gzipped at
//! `~/.cortex/backups/<unix_ms>-<label>.tar.gz` with a JSON manifest at
//! `MANIFEST.json`. Restore never overwrites files newer than the backup
//! (unless `force`, not exposed yet) and never writes outside the whitelisted
//! root families. Per-file cap 5 MB, per-archive 250 MB.

use chrono::Utc;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;

const BACKUP_DIR_REL: &str = ".cortex/backups";
const MANIFEST_NAME: &str = "MANIFEST.json";
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 250 * 1024 * 1024;

/// Sidecar + return type for `create_backup`/`list_backups`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMeta {
    pub id: String,
    pub label: String,
    pub created_unix_ms: i64,
    pub size_bytes: u64,
    pub file_count: usize,
    /// Absolute roots captured. Used by restore to decompose archive paths.
    pub roots: Vec<String>,
}

/// In-archive manifest (also written to disk as a sidecar `.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    id: String,
    label: String,
    created_unix_ms: i64,
    roots: Vec<String>,
    file_count: usize,
    total_bytes: u64,
    /// Cortex schema version — bump if archive layout changes.
    schema: u32,
}

/// Result of a restore operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RestoreReport {
    pub files_restored: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
}

fn backups_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let dir = home.join(BACKUP_DIR_REL);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir backups: {e}"))?;
    Ok(dir)
}

/// Compute the absolute roots to capture. Order is significant — restore
/// resolves archive entries by their `rN/` prefix back to `roots[N]`.
fn capture_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return out,
    };

    // Root 0: ~/.cortex (whole dir — sweeps every per-user config). The
    // backups/ subdir is excluded later via `is_excluded_subpath`.
    let cortex = home.join(".cortex");
    if cortex.exists() {
        out.push(cortex);
    }

    // Roots 1..N: ~/.claude/projects/*/memory directories. One per project so
    // archive paths stay legible and restore can be granular per project.
    let claude_proj = home.join(".claude").join("projects");
    if claude_proj.exists() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&claude_proj)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path().join("memory"))
            .filter(|p| p.exists())
            .collect();
        entries.sort();
        out.extend(entries);
    }

    out
}

/// Inside `~/.cortex` we skip the `backups/` subdir (don't backup backups) and
/// any obviously volatile caches. Keep this conservative — the panel is
/// advertised as a full export.
fn is_excluded_subpath(rel: &Path) -> bool {
    let mut comps = rel.components();
    match comps.next().map(|c| c.as_os_str().to_string_lossy().into_owned()) {
        Some(first) => matches!(first.as_str(), "backups" | "snapshots" | "cache" | ".cache"),
        None => false,
    }
}

/// For Claude memory roots, we only include `.md` files — never jsonl chats.
fn include_under_claude(rel: &Path) -> bool {
    rel.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("md")).unwrap_or(false)
}

fn sanitize_label(label: &str) -> String {
    let mut s: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if s.len() > 40 {
        s.truncate(40);
    }
    if s.is_empty() {
        s = "backup".into();
    }
    s
}

fn archive_prefix(idx: usize) -> String {
    format!("r{idx}")
}

/// Append every eligible file under `root` into the tarball under `rN/`.
fn append_root<W: std::io::Write>(
    tar: &mut Builder<W>,
    idx: usize,
    root: &Path,
    is_claude_memory: bool,
    is_cortex_home: bool,
    total: &mut u64,
    file_count: &mut usize,
) -> Result<(), String> {
    let prefix = archive_prefix(idx);
    if root.is_file() {
        return append_one_file(tar, &prefix, root, root, total, file_count);
    }
    for de in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !de.file_type().is_file() {
            continue;
        }
        let abs = de.path();
        let rel = match abs.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if is_cortex_home && is_excluded_subpath(&rel) {
            continue;
        }
        if is_claude_memory && !include_under_claude(&rel) {
            continue;
        }
        if *total >= MAX_TOTAL_BYTES {
            tracing::warn!("backup: hit total cap, stopping at {}", abs.display());
            return Ok(());
        }
        if let Err(e) = append_one_file(tar, &prefix, root, abs, total, file_count) {
            tracing::warn!("backup: skip {}: {e}", abs.display());
        }
    }
    Ok(())
}

fn append_one_file<W: std::io::Write>(
    tar: &mut Builder<W>,
    prefix: &str,
    root: &Path,
    abs: &Path,
    total: &mut u64,
    file_count: &mut usize,
) -> Result<(), String> {
    let meta = abs.metadata().map_err(|e| format!("stat: {e}"))?;
    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("{size} bytes > per-file cap"));
    }
    if *total + size > MAX_TOTAL_BYTES {
        return Err("total cap reached".into());
    }
    let rel = if abs == root {
        PathBuf::from(abs.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
    } else {
        abs.strip_prefix(root).map_err(|e| format!("strip: {e}"))?.to_path_buf()
    };
    let archive_path = Path::new(prefix).join(&rel);
    let mut fh = File::open(abs).map_err(|e| format!("open: {e}"))?;
    tar.append_file(&archive_path, &mut fh).map_err(|e| format!("append: {e}"))?;
    *total += size;
    *file_count += 1;
    Ok(())
}

/// Drop the manifest JSON in as the very first entry so a partial-decode tool
/// can read it without unpacking the whole archive.
fn append_manifest<W: std::io::Write>(tar: &mut Builder<W>, manifest: &Manifest) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(manifest).map_err(|e| format!("manifest serialize: {e}"))?;
    let mut header = Header::new_gnu();
    header.set_path(MANIFEST_NAME).map_err(|e| format!("manifest header path: {e}"))?;
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(Utc::now().timestamp().max(0) as u64);
    header.set_cksum();
    tar.append(&header, body.as_slice()).map_err(|e| format!("manifest append: {e}"))?;
    Ok(())
}

/// Build a backup. Per-file IO errors are logged and skipped; only fatal
/// archive failures bubble up.
pub fn create(label: &str) -> Result<BackupMeta, String> {
    let dir = backups_dir()?;
    let roots = capture_roots();
    if roots.is_empty() {
        return Err("nothing to back up (no ~/.cortex and no Claude project memory)".into());
    }

    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let cortex_home = home.join(".cortex");

    let ts_ms = Utc::now().timestamp_millis();
    let safe = sanitize_label(label);
    let id = format!("{ts_ms}-{safe}");
    let tar_path = dir.join(format!("{id}.tar.gz"));

    let f = File::create(&tar_path).map_err(|e| format!("create tarball: {e}"))?;
    let enc = GzEncoder::new(BufWriter::new(f), Compression::default());
    let mut tar = Builder::new(enc);

    let root_strs: Vec<String> = roots.iter().map(|p| p.to_string_lossy().to_string()).collect();
    // Placeholder manifest with zeroed counts — restored callers only need the
    // roots + id to decode entries. Sidecar gets the final filled-in copy.
    append_manifest(&mut tar, &Manifest { id: id.clone(), label: safe.clone(), created_unix_ms: ts_ms, roots: root_strs.clone(), file_count: 0, total_bytes: 0, schema: 1 })?;

    let mut total: u64 = 0;
    let mut file_count: usize = 0;
    for (idx, root) in roots.iter().enumerate() {
        // Anything that isn't `~/.cortex` is a Claude project memory dir.
        let is_cortex_home = *root == cortex_home;
        append_root(&mut tar, idx, root, !is_cortex_home, is_cortex_home, &mut total, &mut file_count)?;
    }
    tar.finish().map_err(|e| format!("finalize tar: {e}"))?;

    let size_bytes = fs::metadata(&tar_path).map(|m| m.len()).unwrap_or(0);
    let meta = BackupMeta { id: id.clone(), label: safe.clone(), created_unix_ms: ts_ms, size_bytes, file_count, roots: root_strs.clone() };
    let sidecar = dir.join(format!("{id}.json"));
    let final_manifest = Manifest { id, label: safe, created_unix_ms: ts_ms, roots: root_strs, file_count, total_bytes: total, schema: 1 };
    if let Ok(s) = serde_json::to_string_pretty(&final_manifest) {
        let _ = fs::write(&sidecar, s);
    }
    Ok(meta)
}

pub fn list() -> Result<Vec<BackupMeta>, String> {
    let dir = backups_dir()?;
    let mut out: Vec<BackupMeta> = Vec::new();
    for de in fs::read_dir(&dir).map_err(|e| format!("read dir: {e}"))?.flatten() {
        let p = de.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.ends_with(".tar.gz") { continue; }
        let id = name.trim_end_matches(".tar.gz").to_string();
        let size_bytes = fs::metadata(&p).map(|md| md.len()).unwrap_or(0);
        let meta_path = dir.join(format!("{id}.json"));
        let meta = match fs::read_to_string(&meta_path).ok().and_then(|s| serde_json::from_str::<Manifest>(&s).ok()) {
            Some(m) => BackupMeta { id: m.id, label: m.label, created_unix_ms: m.created_unix_ms, size_bytes, file_count: m.file_count, roots: m.roots },
            None => {
                let (ts, label) = parse_id(&id);
                BackupMeta { id: id.clone(), label, created_unix_ms: ts, size_bytes, file_count: 0, roots: vec![] }
            }
        };
        out.push(meta);
    }
    out.sort_by(|a, b| b.created_unix_ms.cmp(&a.created_unix_ms));
    Ok(out)
}

fn parse_id(id: &str) -> (i64, String) {
    if let Some((ts_str, rest)) = id.split_once('-') {
        let ts = ts_str.parse::<i64>().unwrap_or(0);
        (ts, rest.to_string())
    } else {
        (0, id.to_string())
    }
}

fn mtime_ms(p: &Path) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Resolve allowed roots for restore. We pin them to the two whitelisted
/// families regardless of what the manifest claims, so a tampered manifest
/// cannot redirect writes elsewhere.
fn allowed_root_prefixes() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".cortex"));
        v.push(home.join(".claude").join("projects"));
    }
    v
}

/// Canonicalize the deepest *existing* ancestor of `path`, resolving any
/// symlinks along the way, then re-append the not-yet-created tail components.
/// Returns None if no ancestor can be canonicalized. This is symlink-safe:
/// because we resolve a real ancestor rather than falling back to the raw
/// (unresolved) path, a symlinked intermediate directory can no longer
/// redirect the effective location outside the canonicalized tree.
fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(canon) = cur.canonicalize() {
            let mut resolved = canon;
            for comp in tail.iter().rev() {
                resolved.push(comp);
            }
            return Some(resolved);
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            // No more ancestors to climb (reached root or a relative dead-end).
            _ => return None,
        }
    }
}

/// Is `target` safely inside one of the allowed root prefixes?
///
/// We canonicalize the deepest existing ancestor of `target` (which resolves
/// any symlinks in the path) and require the result to stay within a
/// canonicalized allowed root. If we cannot resolve a real ancestor of either
/// the target or a root, we deny rather than fall back to an unresolved path —
/// failing closed prevents a symlinked subdir from escaping the whitelist.
fn is_allowed(target: &Path) -> bool {
    let canon_target = match canonicalize_existing_ancestor(target) {
        Some(t) => t,
        None => return false,
    };
    for root in allowed_root_prefixes() {
        let canon_root = match root.canonicalize() {
            Ok(r) => r,
            Err(_) => continue,
        };
        if canon_target.starts_with(&canon_root) || canon_target == canon_root {
            return true;
        }
    }
    false
}

/// Restore the backup. `dry_run=true` reports what would happen without
/// touching disk. `force=false` skips files newer than the backup.
pub fn restore(id: &str, dry_run: bool, force: bool) -> Result<RestoreReport, String> {
    let dir = backups_dir()?;
    let tar_path = dir.join(format!("{id}.tar.gz"));
    if !tar_path.exists() {
        return Err(format!("backup not found: {id}"));
    }

    // Pull the manifest from the sidecar — falling back to in-archive read if
    // missing. Sidecar reads are cheap and avoid a streaming pass.
    let sidecar_path = dir.join(format!("{id}.json"));
    let manifest: Manifest = match fs::read_to_string(&sidecar_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(m) => m,
        None => read_manifest_from_archive(&tar_path)?,
    };
    if manifest.roots.is_empty() {
        return Err("backup missing root metadata — refusing to restore blindly".into());
    }
    let roots: Vec<PathBuf> = manifest.roots.iter().map(PathBuf::from).collect();

    let f = File::open(&tar_path).map_err(|e| format!("open tarball: {e}"))?;
    let dec = GzDecoder::new(BufReader::new(f));
    let mut arc = Archive::new(dec);
    arc.set_overwrite(true);

    let mut report = RestoreReport::default();

    for entry in arc.entries().map_err(|e| format!("read entries: {e}"))? {
        let mut entry = match entry { Ok(e) => e, Err(e) => { report.errors.push(format!("entry: {e}")); continue; } };
        let path_owned = match entry.path() { Ok(p) => p.into_owned(), Err(e) => { report.errors.push(format!("entry path: {e}")); continue; } };
        // Skip the manifest entry — it isn't a restorable file.
        if path_owned == Path::new(MANIFEST_NAME) { continue; }
        if path_owned.is_absolute() || path_owned.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            report.files_skipped += 1; continue;
        }
        let target = match resolve_target(&path_owned, &roots) {
            Some(t) => t,
            None => { report.files_skipped += 1; continue; }
        };
        // Safety #1: target must live under one of the whitelisted root families.
        // Catches a malicious manifest pointing outside ~/.cortex.
        if !is_allowed(&target) { report.files_skipped += 1; continue; }
        // Safety #2: don't clobber files newer than the backup unless forced.
        if !force && target.exists() && mtime_ms(&target) > manifest.created_unix_ms {
            report.files_skipped += 1; continue;
        }
        if dry_run { report.files_restored += 1; continue; }
        if let Some(parent) = target.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                report.errors.push(format!("mkdir {}: {e}", parent.display()));
                continue;
            }
        }
        match entry.unpack(&target) {
            Ok(_) => report.files_restored += 1,
            Err(e) => report.errors.push(format!("unpack {}: {e}", target.display())),
        }
    }
    Ok(report)
}

/// Decode `rN/<rel>` archive path back to its on-disk target via `roots[N]`.
/// Returns None when the prefix is missing/malformed or N is out of range.
fn resolve_target(archive_path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let mut comps = archive_path.components();
    let head = comps.next()?.as_os_str().to_string_lossy().into_owned();
    let idx: usize = head.strip_prefix('r')?.parse().ok()?;
    let orig_root = roots.get(idx)?;
    let rel = comps.as_path();
    Some(if rel.as_os_str().is_empty() { orig_root.clone() } else { orig_root.join(rel) })
}

/// Read the manifest entry out of a tarball without unpacking everything.
fn read_manifest_from_archive(tar_path: &Path) -> Result<Manifest, String> {
    let f = File::open(tar_path).map_err(|e| format!("open tarball: {e}"))?;
    let dec = GzDecoder::new(BufReader::new(f));
    let mut arc = Archive::new(dec);
    for entry in arc.entries().map_err(|e| format!("read entries: {e}"))? {
        let mut entry = entry.map_err(|e| format!("entry: {e}"))?;
        let path = entry.path().map_err(|e| format!("entry path: {e}"))?.into_owned();
        if path == Path::new(MANIFEST_NAME) {
            let mut buf = String::new();
            entry.read_to_string(&mut buf).map_err(|e| format!("read manifest: {e}"))?;
            return serde_json::from_str::<Manifest>(&buf)
                .map_err(|e| format!("parse manifest: {e}"));
        }
    }
    Err("manifest not found in archive".into())
}

pub fn delete(id: &str) -> Result<(), String> {
    let dir = backups_dir()?;
    let tar = dir.join(format!("{id}.tar.gz"));
    let meta = dir.join(format!("{id}.json"));
    if tar.exists() {
        fs::remove_file(&tar).map_err(|e| format!("rm tarball: {e}"))?;
    }
    if meta.exists() {
        let _ = fs::remove_file(&meta);
    }
    Ok(())
}

// ───────────────────────── Tauri command surface ─────────────────────────

#[tauri::command]
pub async fn create_backup(label: String) -> Result<BackupMeta, String> {
    tokio::task::spawn_blocking(move || create(&label))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn list_backups() -> Result<Vec<BackupMeta>, String> {
    tokio::task::spawn_blocking(list)
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn restore_backup(id: String, dry_run: bool) -> Result<RestoreReport, String> {
    tokio::task::spawn_blocking(move || restore(&id, dry_run, false))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn delete_backup(id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || delete(&id))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_sanitisation() {
        assert_eq!(sanitize_label("hello world!"), "hello_world_");
        assert_eq!(sanitize_label(""), "backup");
        assert_eq!(sanitize_label("ok-name_1"), "ok-name_1");
        assert_eq!(sanitize_label(&"x".repeat(100)).len(), 40);
    }
    #[test]
    fn id_round_trip() {
        assert_eq!(parse_id("1700000000000-manual"), (1_700_000_000_000, "manual".to_string()));
        assert_eq!(parse_id("bogus"), (0, "bogus".to_string()));
        assert_eq!(archive_prefix(0), "r0");
        assert_eq!(archive_prefix(7), "r7");
    }
    #[test]
    fn filters() {
        assert!(is_excluded_subpath(Path::new("backups/foo.tar.gz")));
        assert!(is_excluded_subpath(Path::new("cache/blob")));
        assert!(!is_excluded_subpath(Path::new("snippets.json")));
        assert!(include_under_claude(Path::new("memory.md")));
        assert!(!include_under_claude(Path::new("chat.jsonl")));
    }
}
