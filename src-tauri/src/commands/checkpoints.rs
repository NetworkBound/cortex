//! Cursor-style workspace checkpoints — git-independent rollback.
//!
//! Each checkpoint is a gzipped tarball of the project tree (sans heavy/derived
//! dirs) at `<project_root>/.cortex/checkpoints/<unix_ms>-<short_uuid>.tar.gz`.
//! Restore extracts the tarball over the project, overwriting current files.
//!
//! Size is hard-capped at 50 MB total; oversize files are skipped with a
//! tracing warning rather than truncating the archive mid-stream.

use chrono::Utc;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use tar::{Archive, Builder};
use uuid::Uuid;
use walkdir::WalkDir;

const MAX_BYTES: u64 = 50 * 1024 * 1024;
const KEEP_RECENT: usize = 10;
const EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".next",
    "__pycache__",
    ".cortex/checkpoints",
];
const EXCLUDE_NAMES: &[&str] = &[".DS_Store"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub id: String,
    pub ts: i64,
    pub label: Option<String>,
    pub size_bytes: u64,
    pub file_count: usize,
}

fn checkpoints_dir(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("checkpoints")
}

fn ensure_dirs(project_root: &Path) -> Result<PathBuf, String> {
    let dir = checkpoints_dir(project_root);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir checkpoints: {e}"))?;
    // Auto-add a .gitignore inside .cortex so checkpoints never get committed.
    let gi = project_root.join(".cortex").join(".gitignore");
    if !gi.exists() {
        let _ = fs::write(&gi, "checkpoints/\n");
    }
    Ok(dir)
}

fn is_excluded(rel: &Path) -> bool {
    let s = rel.to_string_lossy().replace('\\', "/");
    for d in EXCLUDE_DIRS {
        if s == *d || s.starts_with(&format!("{d}/")) {
            return true;
        }
    }
    if let Some(name) = rel.file_name().and_then(|n| n.to_str()) {
        if EXCLUDE_NAMES.iter().any(|x| *x == name) {
            return true;
        }
    }
    false
}

fn short_uuid() -> String {
    Uuid::new_v4().to_string().chars().take(8).collect()
}

/// Reject caller-supplied checkpoint ids that could escape the checkpoints
/// directory once interpolated into `{id}.tar.gz` / `{id}.json`. Mirrors the
/// guard in `threads.rs`. Ids we generate are `<unix_ms>-<short_uuid>`, so the
/// alphanumeric/`-`/`_` allowlist is comfortably permissive for real ids.
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("empty checkpoint id".into());
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(format!("unsafe checkpoint id: {id}"));
    }
    for ch in id.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            return Err(format!("unsafe checkpoint id char: {ch}"));
        }
    }
    Ok(())
}

/// Synchronous core of [`create_checkpoint`] — snapshots the project tree into a
/// gzipped tarball under `.cortex/checkpoints/`. Exposed so other backend
/// features (e.g. the SEARCH/REPLACE edit-block applier) can snapshot the
/// workspace *before* a destructive operation, giving the user a one-click undo.
/// The caller must ensure `root` is a directory; this does blocking IO and is
/// meant to run inside `spawn_blocking`.
pub fn make_checkpoint(root: &Path, label: Option<String>) -> Result<CheckpointInfo, String> {
    let dir = ensure_dirs(root)?;
    let ts_ms = Utc::now().timestamp_millis();
    let id = format!("{ts_ms}-{}", short_uuid());
    let out_path = dir.join(format!("{id}.tar.gz"));

    // First pass: enumerate files, enforcing the 50MB hard cap.
    let mut entries: Vec<(PathBuf, PathBuf, u64)> = Vec::new();
    let mut total: u64 = 0;
    for de in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        let path = de.path();
        if path == root {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if is_excluded(&rel) {
            continue;
        }
        if !de.file_type().is_file() {
            continue;
        }
        let size = de.metadata().map(|m| m.len()).unwrap_or(0);
        if total + size > MAX_BYTES {
            tracing::warn!(
                "checkpoint: skipping {} ({} bytes) — would exceed 50MB cap",
                rel.display(),
                size
            );
            continue;
        }
        total += size;
        entries.push((path.to_path_buf(), rel, size));
    }

    // Second pass: stream files into a gzipped tarball.
    let f = File::create(&out_path).map_err(|e| format!("create tarball: {e}"))?;
    let enc = GzEncoder::new(BufWriter::new(f), Compression::default());
    let mut tar = Builder::new(enc);
    let mut file_count: usize = 0;
    for (abs, rel, _) in &entries {
        match File::open(abs) {
            Ok(mut fh) => {
                if let Err(e) = tar.append_file(rel, &mut fh) {
                    tracing::warn!("checkpoint: skip {}: {e}", rel.display());
                    continue;
                }
                file_count += 1;
            }
            Err(e) => tracing::warn!("checkpoint: open {} failed: {e}", abs.display()),
        }
    }
    tar.finish().map_err(|e| format!("finalize tar: {e}"))?;
    let size_bytes = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);

    // Persist sidecar metadata so we can recover labels without untarring.
    let info = CheckpointInfo { id: id.clone(), ts: ts_ms, label, size_bytes, file_count };
    let meta_path = dir.join(format!("{id}.json"));
    if let Ok(s) = serde_json::to_string(&info) {
        let _ = fs::write(&meta_path, s);
    }
    Ok(info)
}

#[tauri::command]
pub async fn create_checkpoint(
    project_root: String,
    label: Option<String>,
) -> Result<CheckpointInfo, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    tokio::task::spawn_blocking(move || make_checkpoint(&root, label))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

/// Synchronous core of [`list_checkpoints`]: enumerate the project's checkpoints
/// sorted newest-first. Exposed so the undo path can pick the latest snapshot
/// without going through the async command.
pub fn list_checkpoints_sync(root: &Path) -> Result<Vec<CheckpointInfo>, String> {
    let dir = checkpoints_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<CheckpointInfo> = Vec::new();
    for de in fs::read_dir(&dir).map_err(|e| format!("read dir: {e}"))?.flatten() {
        let p = de.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.ends_with(".tar.gz") {
            continue;
        }
        let id = name.trim_end_matches(".tar.gz").to_string();
        let meta_path = dir.join(format!("{id}.json"));
        let info = match fs::read_to_string(&meta_path).ok().and_then(|s| serde_json::from_str(&s).ok()) {
            Some(info) => info,
            None => {
                // Fall back to filename-derived metadata.
                let ts = id.split('-').next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
                let size_bytes = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                CheckpointInfo { id: id.clone(), ts, label: None, size_bytes, file_count: 0 }
            }
        };
        out.push(info);
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    Ok(out)
}

#[tauri::command]
pub async fn list_checkpoints(project_root: String) -> Result<Vec<CheckpointInfo>, String> {
    list_checkpoints_sync(&PathBuf::from(&project_root))
}

fn project_has_dirty_files(project_root: &Path) -> bool {
    // Best-effort git check — used only to gate restore when force=false.
    if !project_root.join(".git").exists() {
        return false;
    }
    let out = crate::sys::no_window("git")
        .arg("-C")
        .arg(project_root)
        .arg("status")
        .arg("--porcelain")
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

/// Synchronous core of [`restore_checkpoint`] — extracts checkpoint `id` over the
/// project tree. Does blocking IO; meant to run inside `spawn_blocking`. Exposed
/// so the undo path can restore without re-implementing the safe-extraction
/// logic. The caller must have already confirmed `root` is a directory.
pub fn restore_checkpoint_core(root: &Path, id: &str, force: bool) -> Result<(), String> {
    validate_id(id)?;
    let dir = checkpoints_dir(root);
    let tarball = dir.join(format!("{id}.tar.gz"));
    if !tarball.exists() {
        return Err(format!("checkpoint not found: {id}"));
    }
    if !force && project_has_dirty_files(root) {
        return Err(
            "uncommitted changes detected — pass force=true to overwrite, or commit/stash first"
                .into(),
        );
    }
    let f = File::open(&tarball).map_err(|e| format!("open tarball: {e}"))?;
    let dec = GzDecoder::new(BufReader::new(f));
    let mut arc = Archive::new(dec);
    arc.set_overwrite(true);
    // Symlinks in restored archives are never legitimate (our own checkpoints
    // only ever contain regular files — see `create_checkpoint`), and they are
    // the classic vector for writing outside the project root: a symlink entry
    // pointing at `/etc` (or `..`) followed by a file entry that resolves
    // *through* that symlink. So we refuse to materialize any link.
    arc.set_preserve_permissions(false);
    // Canonical project root used to confirm every write lands inside the tree.
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("canonicalize project root: {e}"))?;
    // Pre-validate entries don't escape the project root via `..`, absolute
    // paths, symlink/hardlink trickery, or a parent that resolves (through an
    // already-extracted symlink) to somewhere outside root.
    for entry in arc.entries().map_err(|e| format!("read entries: {e}"))? {
        let mut entry = entry.map_err(|e| format!("entry: {e}"))?;
        let p = entry.path().map_err(|e| format!("entry path: {e}"))?.into_owned();
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            tracing::warn!("checkpoint restore: skip unsafe path {}", p.display());
            continue;
        }
        // Reject symlinks and hardlinks outright — they cannot appear in our own
        // archives and are the primary write-outside-root vector.
        let etype = entry.header().entry_type();
        if etype.is_symlink() || etype.is_hard_link() {
            tracing::warn!("checkpoint restore: skip link entry {}", p.display());
            continue;
        }
        let target = root.join(&p);
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
            // Re-resolve the parent: if it traverses a pre-existing symlink that
            // escapes the project root, canonicalization reveals it and we skip.
            match parent.canonicalize() {
                Ok(parent_canon) if parent_canon.starts_with(&root_canon) => {}
                Ok(escaped) => {
                    tracing::warn!(
                        "checkpoint restore: skip {} — parent {} escapes root",
                        p.display(),
                        escaped.display()
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        "checkpoint restore: skip {} — cannot resolve parent: {e}",
                        p.display()
                    );
                    continue;
                }
            }
        }
        entry.unpack(&target).map_err(|e| format!("unpack {}: {e}", p.display()))?;
    }
    Ok(())
}

/// Per-file change between a checkpoint and the live worktree, from the
/// perspective of *applying* the checkpoint (i.e. what `restore` would do).
///
/// - `added`    — file lives in the checkpoint but is missing from the worktree;
///                restore would create it.
/// - `modified` — file exists in both but the bytes differ; restore would
///                overwrite the current copy.
/// - `removed`  — file exists in the worktree but not in the checkpoint. Note:
///                restore extracts *over* the tree and never deletes, so these
///                files survive a restore — we surface them so the user knows
///                they won't be reverted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiffStatus {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointDiffEntry {
    pub path: String,
    pub status: DiffStatus,
    /// Worktree (current) contents, when textual and within the size cap.
    /// `None` for the `added` case or when the file is binary/oversize.
    pub old_content: Option<String>,
    /// Checkpoint contents, when textual and within the size cap.
    /// `None` for the `removed` case or when the file is binary/oversize.
    pub new_content: Option<String>,
    /// True when either side was binary or exceeded the per-file content cap,
    /// so the UI can show a "binary / too large to diff" placeholder instead of
    /// a line diff.
    pub binary: bool,
    /// Worktree file size in bytes (0 for `added`).
    pub old_size: u64,
    /// Checkpoint file size in bytes (0 for `removed`).
    pub new_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointDiff {
    pub id: String,
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
    pub entries: Vec<CheckpointDiffEntry>,
}

/// Per-file content cap for inline diffing. Beyond this we mark the entry
/// `binary` and drop the contents so we never balloon a payload (or block the
/// UI) on a multi-megabyte file.
const DIFF_CONTENT_CAP: usize = 512 * 1024;

/// Decode bytes to a diffable string, or `None` if it looks binary (contains a
/// NUL) or exceeds the per-file cap.
fn diffable_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() > DIFF_CONTENT_CAP || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// Compute the diff between a checkpoint and the current worktree WITHOUT
/// mutating anything. Reads the tarball into memory, walks the live tree with
/// the same exclusion rules `create_checkpoint` uses, and classifies each path.
#[tauri::command]
pub async fn diff_checkpoint(
    project_root: String,
    id: String,
) -> Result<CheckpointDiff, String> {
    validate_id(&id)?;
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let dir = checkpoints_dir(&root);
    let tarball = dir.join(format!("{id}.tar.gz"));
    if !tarball.exists() {
        return Err(format!("checkpoint not found: {id}"));
    }

    // 1. Load every regular file from the checkpoint tarball into memory.
    //    Same safety posture as restore: skip links, absolute/`..` paths.
    let f = File::open(&tarball).map_err(|e| format!("open tarball: {e}"))?;
    let dec = GzDecoder::new(BufReader::new(f));
    let mut arc = Archive::new(dec);
    let mut snapshot: std::collections::BTreeMap<String, Vec<u8>> =
        std::collections::BTreeMap::new();
    for entry in arc.entries().map_err(|e| format!("read entries: {e}"))? {
        let mut entry = entry.map_err(|e| format!("entry: {e}"))?;
        let etype = entry.header().entry_type();
        if etype.is_symlink() || etype.is_hard_link() || !etype.is_file() {
            continue;
        }
        let p = entry.path().map_err(|e| format!("entry path: {e}"))?.into_owned();
        if p.is_absolute()
            || p.components().any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let rel = p.to_string_lossy().replace('\\', "/");
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("read entry {rel}: {e}"))?;
        snapshot.insert(rel, buf);
    }

    // 2. Walk the current worktree with the SAME exclusion rules as create.
    let mut worktree: std::collections::BTreeMap<String, PathBuf> =
        std::collections::BTreeMap::new();
    for de in WalkDir::new(&root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        let path = de.path();
        if path == root {
            continue;
        }
        let rel = match path.strip_prefix(&root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if is_excluded(&rel) || !de.file_type().is_file() {
            continue;
        }
        let key = rel.to_string_lossy().replace('\\', "/");
        worktree.insert(key, path.to_path_buf());
    }

    // 3. Classify. Union of both key sets.
    let mut entries: Vec<CheckpointDiffEntry> = Vec::new();
    let (mut added, mut modified, mut removed) = (0usize, 0usize, 0usize);

    for (path, ck_bytes) in &snapshot {
        let new_size = ck_bytes.len() as u64;
        match worktree.get(path) {
            Some(abs) => {
                // Present in both — compare bytes.
                let wt_bytes = fs::read(abs).unwrap_or_default();
                if wt_bytes == *ck_bytes {
                    continue; // unchanged — omit from the diff
                }
                modified += 1;
                let old_text = diffable_text(&wt_bytes);
                let new_text = diffable_text(ck_bytes);
                let binary = old_text.is_none() || new_text.is_none();
                entries.push(CheckpointDiffEntry {
                    path: path.clone(),
                    status: DiffStatus::Modified,
                    old_size: wt_bytes.len() as u64,
                    new_size,
                    old_content: if binary { None } else { old_text },
                    new_content: if binary { None } else { new_text },
                    binary,
                });
            }
            None => {
                // In checkpoint, not in worktree — restore would re-create it.
                added += 1;
                let new_text = diffable_text(ck_bytes);
                let binary = new_text.is_none();
                entries.push(CheckpointDiffEntry {
                    path: path.clone(),
                    status: DiffStatus::Added,
                    old_size: 0,
                    new_size,
                    old_content: None,
                    new_content: new_text,
                    binary,
                });
            }
        }
    }

    for (path, abs) in &worktree {
        if snapshot.contains_key(path) {
            continue;
        }
        // In worktree, not in checkpoint — survives restore (we never delete).
        removed += 1;
        let wt_bytes = fs::read(abs).unwrap_or_default();
        let old_text = diffable_text(&wt_bytes);
        let binary = old_text.is_none();
        entries.push(CheckpointDiffEntry {
            path: path.clone(),
            status: DiffStatus::Removed,
            old_size: wt_bytes.len() as u64,
            new_size: 0,
            old_content: old_text,
            new_content: None,
            binary,
        });
    }

    // Stable, scannable order: modified, then added, then removed; path within.
    entries.sort_by(|a, b| {
        fn rank(s: &DiffStatus) -> u8 {
            match s {
                DiffStatus::Modified => 0,
                DiffStatus::Added => 1,
                DiffStatus::Removed => 2,
            }
        }
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(CheckpointDiff { id, added, modified, removed, entries })
}

#[tauri::command]
pub async fn restore_checkpoint(
    project_root: String,
    id: String,
    force: Option<bool>,
) -> Result<(), String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let force = force.unwrap_or(false);
    tokio::task::spawn_blocking(move || restore_checkpoint_core(&root, &id, force))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

/// Find the newest checkpoint by timestamp and restore it — the quick "undo"
/// path. Returns the restored checkpoint's metadata, or `None` when the project
/// has no checkpoints to undo.
pub fn restore_last_core(root: &Path, force: bool) -> Result<Option<CheckpointInfo>, String> {
    // `list_checkpoints_sync` already sorts newest-first.
    let Some(latest) = list_checkpoints_sync(root)?.into_iter().next() else {
        return Ok(None);
    };
    restore_checkpoint_core(root, &latest.id, force)?;
    Ok(Some(latest))
}

/// Restore the most-recent checkpoint (aider's `/undo`), complementing the
/// auto-checkpoint taken before `/apply`. With no checkpoints the result is
/// `None` so the caller can say "nothing to undo" rather than erroring.
#[tauri::command]
pub async fn restore_last_checkpoint(
    project_root: String,
    force: Option<bool>,
) -> Result<Option<CheckpointInfo>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let force = force.unwrap_or(false);
    tokio::task::spawn_blocking(move || restore_last_core(&root, force))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn delete_checkpoint(project_root: String, id: String) -> Result<(), String> {
    validate_id(&id)?;
    let root = PathBuf::from(&project_root);
    let dir = checkpoints_dir(&root);
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

/// Drop everything older than the most-recent `KEEP_RECENT` checkpoints.
#[tauri::command]
pub async fn prune_checkpoints(project_root: String) -> Result<usize, String> {
    let all = list_checkpoints(project_root.clone()).await?;
    if all.len() <= KEEP_RECENT {
        return Ok(0);
    }
    let mut removed = 0;
    for ck in all.into_iter().skip(KEEP_RECENT) {
        if delete_checkpoint(project_root.clone(), ck.id).await.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Helper for the file-write peek used by `WalkDir`. Kept here only so the
/// unused-import lint doesn't trip when callers wire this module up.
#[allow(dead_code)]
fn _peek<R: Read>(_r: R) {}

#[cfg(test)]
mod tests {
    use super::{make_checkpoint, restore_last_checkpoint, restore_last_core, validate_id};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn restore_last_restores_newest_checkpoint() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("data.txt");
        // Snapshot v1, then change to v2 and snapshot again.
        fs::write(&f, "v1\n").unwrap();
        let _a = make_checkpoint(td.path(), Some("a".into())).unwrap();
        // Guarantee a strictly-newer timestamp so "newest" is unambiguous
        // (checkpoint ids are millisecond-stamped).
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&f, "v2\n").unwrap();
        let b = make_checkpoint(td.path(), Some("b".into())).unwrap();
        // Now make an unwanted edit and undo it — the latest checkpoint (b)
        // should be the one restored, bringing the file back to v2 (not v1).
        fs::write(&f, "v3-unwanted\n").unwrap();
        let restored = restore_last_core(td.path(), true)
            .unwrap()
            .expect("a checkpoint should be restored");
        assert_eq!(restored.id, b.id, "undo must restore the newest checkpoint");
        assert_eq!(fs::read_to_string(&f).unwrap(), "v2\n");
    }

    #[test]
    fn restore_last_with_no_checkpoints_is_none() {
        let td = TempDir::new().unwrap();
        fs::write(td.path().join("x.txt"), "hi\n").unwrap();
        // Nothing to undo → Ok(None), never an error.
        assert!(restore_last_core(td.path(), true).unwrap().is_none());
    }

    /// The real user flow end-to-end: `/apply` mutates a file and auto-snapshots
    /// the workspace first, then `/undo` (restore_last_checkpoint) rolls it back.
    /// Exercises the actual async Tauri command paths across both modules.
    #[test]
    fn apply_then_undo_round_trip_restores_the_file() {
        use crate::commands::apply_edits::apply_edit_blocks;

        let td = TempDir::new().unwrap();
        let f = td.path().join("greet.py");
        let original = "def hi():\n    return 'hi'\n";
        fs::write(&f, original).unwrap();

        // /apply: a real SEARCH/REPLACE edit — this auto-checkpoints first.
        let edit = "greet.py\n<<<<<<< SEARCH\n    return 'hi'\n=======\n    return 'hello'\n>>>>>>> REPLACE\n";
        let report = tauri::async_runtime::block_on(apply_edit_blocks(
            td.path().display().to_string(),
            edit.to_string(),
            None,
        ))
        .unwrap();
        assert_eq!(report.applied, 1);
        assert!(report.checkpoint_id.is_some(), "apply should snapshot first");
        assert_eq!(fs::read_to_string(&f).unwrap(), "def hi():\n    return 'hello'\n");

        // /undo: restore the most-recent checkpoint (the pre-apply snapshot).
        let restored = tauri::async_runtime::block_on(restore_last_checkpoint(
            td.path().display().to_string(),
            Some(true),
        ))
        .unwrap()
        .expect("the pre-apply checkpoint should be restored");
        assert_eq!(restored.id, report.checkpoint_id.unwrap());
        assert_eq!(restored.label.as_deref(), Some("before /apply"));
        // File is back to its pre-apply contents.
        assert_eq!(fs::read_to_string(&f).unwrap(), original);
    }

    #[test]
    fn validate_id_rejects_traversal() {
        // Real ids look like "<unix_ms>-<short_uuid>".
        assert!(validate_id("1717000000000-a1b2c3d4").is_ok());
        // Path-escape attempts and odd chars are rejected.
        assert!(validate_id("../../etc/passwd").is_err());
        assert!(validate_id("..").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("a\\b").is_err());
        assert!(validate_id("").is_err());
        assert!(validate_id("name with space").is_err());
    }
}
