//! Point-in-time snapshots of every memory source — global-instruction files,
//! ~/.cortex JSON stores, skills/, every claude project memory dir, plus the
//! active project's runbooks/ and `.cortex/*.toml`. Saved as a gzipped tarball
//! at `~/.cortex/snapshots/<unix_ms>-<label>.tar.gz` with a JSON sidecar.
//!
//! Rollback restores files atomically per-entry but is conservative: it never
//! escapes the original capture roots and it skips any current file whose mtime
//! is newer than the snapshot's own creation time (so a user can't accidentally
//! clobber notes written after the snapshot was taken).

use chrono::Utc;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tar::{Archive, Builder};
use walkdir::WalkDir;

const SNAPSHOT_ROOT_REL: &str = ".cortex/snapshots";
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024; // 5 MB per file safety cap
const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024; // 200 MB per snapshot

/// Metadata persisted next to the tarball and returned from `create_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: String,
    pub label: String,
    pub created_unix_ms: i64,
    pub size_bytes: u64,
    pub file_count: usize,
    /// Absolute roots that were captured, recorded so `rollback` can reject
    /// any tar entry that doesn't fall inside one of them.
    pub roots: Vec<String>,
}

/// Result of a rollback operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RollbackReport {
    pub files_restored: usize,
    pub files_skipped: usize,
    pub errors: Vec<String>,
}

/// Returns `~/.cortex/snapshots`, creating it if missing.
pub fn snapshots_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let dir = home.join(SNAPSHOT_ROOT_REL);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir snapshots: {e}"))?;
    Ok(dir)
}

/// Compute the absolute paths that make up a snapshot capture set. The list
/// is deduplicated and excludes any file/dir that doesn't exist on disk so
/// restore can rely on every recorded root actually being real.
pub fn capture_roots(active_project: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let push = |out: &mut Vec<PathBuf>, p: PathBuf| {
        if p.exists() && !out.iter().any(|q| q == &p) {
            out.push(p);
        }
    };
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return out,
    };

    // ~/.claude/projects/*/memory directories
    let claude_proj = home.join(".claude").join("projects");
    if claude_proj.exists() {
        for entry in fs::read_dir(&claude_proj).into_iter().flatten().flatten() {
            let mem = entry.path().join("memory");
            if mem.exists() {
                push(&mut out, mem);
            }
        }
    }

    // ~/.cortex/* well-known JSON stores + skills dir
    push(&mut out, home.join(".cortex").join("snippets.json"));
    push(&mut out, home.join(".cortex").join("agent-instructions.json"));
    push(&mut out, home.join(".cortex").join("skills"));

    if let Some(project) = active_project {
        let runbooks = project.join("runbooks");
        push(&mut out, runbooks);
        // Project-level cortex toml configs (e.g. `.cortex/profile.toml`).
        let cortex_dir = project.join(".cortex");
        if cortex_dir.exists() {
            for entry in fs::read_dir(&cortex_dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                    push(&mut out, p);
                }
            }
        }
    }

    out
}

/// Sanitize a label into something safe for a filename. Non `[A-Za-z0-9_-]`
/// becomes `_`; trims to 40 chars; empty → `snapshot`.
fn sanitize_label(label: &str) -> String {
    let mut s: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if s.len() > 40 {
        s.truncate(40);
    }
    if s.is_empty() {
        s = "snapshot".into();
    }
    s
}

/// Storage key derived from a real path. Each captured root gets a numeric
/// prefix (`r0/`, `r1/`, …) so two different roots can share a filename
/// without colliding inside the archive, and so the original root can be
/// recovered from the sidecar's `roots[i]` during rollback.
fn archive_prefix(idx: usize) -> String {
    format!("r{idx}")
}

/// Walk a single capture root and append entries to a tarball.
fn append_root<W: std::io::Write>(
    tar: &mut Builder<W>,
    idx: usize,
    root: &Path,
    total: &mut u64,
    file_count: &mut usize,
) -> Result<(), String> {
    let prefix = archive_prefix(idx);
    let walk: Box<dyn Iterator<Item = walkdir::DirEntry>> = if root.is_file() {
        Box::new(WalkDir::new(root).max_depth(0).into_iter().filter_map(|e| e.ok()))
    } else {
        Box::new(
            WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok()),
        )
    };

    for de in walk {
        if !de.file_type().is_file() {
            continue;
        }
        let abs = de.path();
        let size = de.metadata().map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            tracing::warn!("snapshot: skip {} ({size} bytes > per-file cap)", abs.display());
            continue;
        }
        if *total + size > MAX_TOTAL_BYTES {
            tracing::warn!("snapshot: hit total cap, stopping at {}", abs.display());
            return Ok(());
        }
        // Relative path inside the root → prepend `rN/`.
        let rel = if abs == root {
            PathBuf::from(
                abs.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
            )
        } else {
            match abs.strip_prefix(root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            }
        };
        let archive_path = Path::new(&prefix).join(&rel);
        match File::open(abs) {
            Ok(mut fh) => {
                if let Err(e) = tar.append_file(&archive_path, &mut fh) {
                    tracing::warn!("snapshot: append {} failed: {e}", abs.display());
                    continue;
                }
                *total += size;
                *file_count += 1;
            }
            Err(e) => tracing::warn!("snapshot: open {} failed: {e}", abs.display()),
        }
    }
    Ok(())
}

/// Build a snapshot. Errors only on fatal IO; per-file problems are logged
/// and skipped so a partial capture is better than no capture at all.
pub fn create(label: &str, active_project: Option<&Path>) -> Result<SnapshotMeta, String> {
    let dir = snapshots_dir()?;
    let roots = capture_roots(active_project);
    if roots.is_empty() {
        return Err("no memory sources found to snapshot".into());
    }

    let ts_ms = Utc::now().timestamp_millis();
    let safe = sanitize_label(label);
    let id = format!("{ts_ms}-{safe}");
    let tar_path = dir.join(format!("{id}.tar.gz"));

    let f = File::create(&tar_path).map_err(|e| format!("create tarball: {e}"))?;
    let enc = GzEncoder::new(BufWriter::new(f), Compression::default());
    let mut tar = Builder::new(enc);

    let mut total: u64 = 0;
    let mut file_count: usize = 0;
    for (idx, root) in roots.iter().enumerate() {
        append_root(&mut tar, idx, root, &mut total, &mut file_count)?;
    }
    tar.finish().map_err(|e| format!("finalize tar: {e}"))?;

    let size_bytes = fs::metadata(&tar_path).map(|m| m.len()).unwrap_or(0);
    let meta = SnapshotMeta {
        id: id.clone(),
        label: safe,
        created_unix_ms: ts_ms,
        size_bytes,
        file_count,
        roots: roots.iter().map(|p| p.to_string_lossy().to_string()).collect(),
    };
    let sidecar = dir.join(format!("{id}.json"));
    if let Ok(s) = serde_json::to_string_pretty(&meta) {
        let _ = fs::write(&sidecar, s);
    }
    Ok(meta)
}

/// List existing snapshots, newest first.
pub fn list() -> Result<Vec<SnapshotMeta>, String> {
    let dir = snapshots_dir()?;
    let mut out: Vec<SnapshotMeta> = Vec::new();
    for de in fs::read_dir(&dir).map_err(|e| format!("read dir: {e}"))?.flatten() {
        let p = de.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.ends_with(".tar.gz") {
            continue;
        }
        let id = name.trim_end_matches(".tar.gz").to_string();
        let meta_path = dir.join(format!("{id}.json"));
        let meta = match fs::read_to_string(&meta_path).ok().and_then(|s| serde_json::from_str::<SnapshotMeta>(&s).ok()) {
            Some(m) => m,
            None => {
                // Fall back to filename-derived metadata.
                let (ts, label) = parse_id(&id);
                let size_bytes = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                SnapshotMeta { id: id.clone(), label, created_unix_ms: ts, size_bytes, file_count: 0, roots: vec![] }
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

/// Returns the mtime of `p` as unix millis, or `None` if it can't be read.
/// Distinguishing "unavailable" from a real `0` matters for the rollback
/// clobber guard: an unreadable mtime must be treated as "possibly newer"
/// so we never overwrite a file we can't reason about.
fn mtime_ms(p: &Path) -> Option<i64> {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

/// Restore a snapshot. Skips entries whose destination is outside the
/// original capture roots OR whose current file is newer than the snapshot.
pub fn rollback(id: &str) -> Result<RollbackReport, String> {
    let dir = snapshots_dir()?;
    let tar_path = dir.join(format!("{id}.tar.gz"));
    if !tar_path.exists() {
        return Err(format!("snapshot not found: {id}"));
    }
    let meta_path = dir.join(format!("{id}.json"));
    let meta: SnapshotMeta = fs::read_to_string(&meta_path)
        .map_err(|e| format!("read sidecar: {e}"))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("parse sidecar: {e}")))?;
    if meta.roots.is_empty() {
        return Err("snapshot missing root metadata — refusing to restore blindly".into());
    }
    let roots: Vec<PathBuf> = meta.roots.iter().map(PathBuf::from).collect();

    let f = File::open(&tar_path).map_err(|e| format!("open tarball: {e}"))?;
    let dec = GzDecoder::new(BufReader::new(f));
    let mut arc = Archive::new(dec);
    arc.set_overwrite(true);

    let mut report = RollbackReport::default();

    for entry in arc.entries().map_err(|e| format!("read entries: {e}"))? {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                report.errors.push(format!("entry: {e}"));
                continue;
            }
        };
        let path_owned = match entry.path() {
            Ok(p) => p.into_owned(),
            Err(e) => {
                report.errors.push(format!("entry path: {e}"));
                continue;
            }
        };
        if path_owned.is_absolute()
            || path_owned.components().any(|c| matches!(c, std::path::Component::ParentDir))
        {
            report.files_skipped += 1;
            continue;
        }
        // Decompose `rN/<rel>` → resolve back to the original root.
        let mut comps = path_owned.components();
        let head = match comps.next() {
            Some(c) => c.as_os_str().to_string_lossy().to_string(),
            None => {
                report.files_skipped += 1;
                continue;
            }
        };
        let Some(stripped) = head.strip_prefix('r') else {
            report.files_skipped += 1;
            continue;
        };
        let Ok(idx) = stripped.parse::<usize>() else {
            report.files_skipped += 1;
            continue;
        };
        let Some(orig_root) = roots.get(idx) else {
            report.files_skipped += 1;
            continue;
        };
        let rel: PathBuf = comps.as_path().to_path_buf();
        let target = if orig_root.is_file() || rel.as_os_str().is_empty() {
            orig_root.clone()
        } else {
            orig_root.join(&rel)
        };

        // Safety: target must be inside (or equal to) the recorded root.
        let canon_root = orig_root.canonicalize().unwrap_or_else(|_| orig_root.clone());
        let canon_target_parent = target
            .parent()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
            .unwrap_or_else(|| target.clone());
        if !canon_target_parent.starts_with(&canon_root)
            && canon_target_parent != canon_root
            && orig_root.is_dir()
        {
            report.files_skipped += 1;
            continue;
        }

        // Don't clobber files newer than the snapshot itself. If the target
        // exists but its mtime can't be read, treat it as "possibly newer"
        // and skip — for file roots the containment check above is bypassed,
        // so this guard is the only thing protecting a legitimately newer
        // file from being silently overwritten.
        if target.exists() {
            match mtime_ms(&target) {
                Some(m) if m > meta.created_unix_ms => {
                    report.files_skipped += 1;
                    continue;
                }
                None => {
                    report.files_skipped += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(parent) = target.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                report
                    .errors
                    .push(format!("mkdir {}: {e}", parent.display()));
                continue;
            }
        }
        match entry.unpack(&target) {
            Ok(_) => report.files_restored += 1,
            Err(e) => report
                .errors
                .push(format!("unpack {}: {e}", target.display())),
        }
    }
    Ok(report)
}

/// Delete a snapshot and its sidecar.
pub fn delete(id: &str) -> Result<(), String> {
    let dir = snapshots_dir()?;
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

/// Keep the newest `keep` snapshots; delete the rest. Returns the count deleted.
pub fn prune(keep: usize) -> Result<usize, String> {
    let all = list()?;
    if all.len() <= keep {
        return Ok(0);
    }
    let mut removed = 0;
    for snap in all.into_iter().skip(keep) {
        if delete(&snap.id).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_strips_unsafe() {
        assert_eq!(sanitize_label("hello world!"), "hello_world_");
        assert_eq!(sanitize_label(""), "snapshot");
        assert_eq!(sanitize_label("ok-name_1"), "ok-name_1");
        let huge = "x".repeat(100);
        assert_eq!(sanitize_label(&huge).len(), 40);
    }

    #[test]
    fn parse_id_splits_ts_and_label() {
        let (ts, label) = parse_id("1700000000000-manual");
        assert_eq!(ts, 1_700_000_000_000);
        assert_eq!(label, "manual");
        let (ts2, label2) = parse_id("bogus");
        assert_eq!(ts2, 0);
        assert_eq!(label2, "bogus");
    }

    #[test]
    fn archive_prefix_format() {
        assert_eq!(archive_prefix(0), "r0");
        assert_eq!(archive_prefix(12), "r12");
    }
}
