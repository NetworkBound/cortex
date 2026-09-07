//! Bridge external memory sources (e.g. claude-mem under
//! `~/.claude/projects/*/memory/`) into Cortex's local imported-memory
//! directory. Copies are content-addressed — if a markdown file with the
//! same content fingerprint already lives in the destination, we skip it.
//! Existing files are never overwritten.

use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const MAX_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB — match sources::walk_markdown
// Full FNV-1a 64-bit digest (16 hex) plus an 8-hex length suffix. The length
// guard means two inputs collide only if they share both the same FNV hash and
// the same byte length, which is far less likely than a bare 64-bit collision.
#[cfg(test)]
const HASH_HEX_LEN: usize = 24;

#[derive(Debug, Serialize)]
pub struct ImportSummary {
    pub scanned: usize,
    pub imported: usize,
    pub skipped: usize,
    pub destination: String,
}

#[tauri::command]
pub async fn import_claude_mem() -> Result<ImportSummary, String> {
    tokio::task::spawn_blocking(run_import)
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| e.to_string())
}

fn run_import() -> anyhow::Result<ImportSummary> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let claude_proj = home.join(".claude").join("projects");
    let dest = imported_memory_dir()?;
    fs::create_dir_all(&dest)?;

    let mut existing_hashes = scan_existing_hashes(&dest);

    let mut scanned = 0usize;
    let mut imported = 0usize;
    let mut skipped = 0usize;

    if !claude_proj.exists() {
        return Ok(ImportSummary {
            scanned,
            imported,
            skipped,
            destination: dest.display().to_string(),
        });
    }

    for proj_entry in fs::read_dir(&claude_proj)?.flatten() {
        let mem_root = proj_entry.path().join("memory");
        if !mem_root.exists() {
            continue;
        }
        let proj_label = proj_entry.file_name().to_string_lossy().to_string();
        for path in walk_markdown(&mem_root) {
            scanned += 1;
            let Ok(bytes) = fs::read(&path) else {
                skipped += 1;
                continue;
            };
            let hash = short_hash(&bytes);
            if existing_hashes.contains(&hash) {
                skipped += 1;
                continue;
            }
            let target_name = unique_target_name(&dest, &proj_label, &path, &hash);
            let target = dest.join(&target_name);
            // Be paranoid: only write if path doesn't exist (preserves "never overwrite").
            if target.exists() {
                skipped += 1;
                continue;
            }
            match fs::write(&target, &bytes) {
                Ok(_) => {
                    existing_hashes.insert(hash);
                    imported += 1;
                }
                Err(_) => {
                    skipped += 1;
                }
            }
        }
    }

    Ok(ImportSummary {
        scanned,
        imported,
        skipped,
        destination: dest.display().to_string(),
    })
}

/// Returns the destination directory where imported markdown lives.
fn imported_memory_dir() -> anyhow::Result<PathBuf> {
    let local = dirs::data_local_dir().ok_or_else(|| anyhow::anyhow!("no local data dir"))?;
    Ok(local.join("cortex").join("imported-memory"))
}

/// Compute a stable content fingerprint for dedupe ("have I already imported
/// this file?"). This is a non-cryptographic FNV-1a 64-bit hash combined with
/// the byte length — not a cryptographic digest, and not collision-proof, but
/// the full 64-bit hash plus length guard makes accidental collisions between
/// distinct claude-mem files vanishingly unlikely. We avoid pulling in `sha2`.
fn short_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let len = bytes.len() as u64;
    // 16 hex of FNV + 8 hex of length = HASH_HEX_LEN chars. Keep the full
    // string: truncating away the length suffix would discard the extra guard.
    format!("{:016x}{:08x}", h, len)
}

/// Walk markdown files under a single memory root, respecting the same
/// 1 MiB ceiling that `sources::walk_markdown` uses for parity.
fn walk_markdown(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .max_depth(6)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == "md" || s == "markdown")
        })
        .filter(|e| e.metadata().map(|m| m.len() < MAX_FILE_BYTES).unwrap_or(false))
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Pre-compute hashes of files already in the destination so subsequent
/// imports stay idempotent.
fn scan_existing_hashes(dest: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    for entry in WalkDir::new(dest)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let mut buf = Vec::new();
        if let Ok(mut f) = fs::File::open(entry.path()) {
            if f.read_to_end(&mut buf).is_ok() {
                set.insert(short_hash(&buf));
            }
        }
    }
    set
}

/// Build a target filename like `claude-foo__some-note.abc1234.md` that
/// preserves the source project label and is unique per content hash.
fn unique_target_name(dest: &Path, proj_label: &str, src: &Path, hash: &str) -> String {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "memory".to_string());
    let safe_proj = sanitize(proj_label);
    let safe_stem = sanitize(&stem);
    let base = format!("{}__{}.{}.md", safe_proj, safe_stem, &hash[..hash.len().min(7)]);
    if !dest.join(&base).exists() {
        return base;
    }
    // Extremely unlikely (hash collision on disk) — append a counter.
    for n in 1..1000 {
        let candidate = format!(
            "{}__{}.{}.{}.md",
            safe_proj,
            safe_stem,
            &hash[..hash.len().min(7)],
            n
        );
        if !dest.join(&candidate).exists() {
            return candidate;
        }
    }
    base
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_is_stable_and_distinct() {
        let a = short_hash(b"hello world");
        let b = short_hash(b"hello world");
        let c = short_hash(b"hello world!");
        assert_eq!(a, b);
        assert_ne!(a, c);
        // 16 hex of FNV + 8 hex of length — the full HASH_HEX_LEN string is
        // kept (truncating away the length suffix would discard the guard).
        assert_eq!(a.len(), HASH_HEX_LEN);
    }

    #[test]
    fn sanitize_strips_unsafe_chars() {
        assert_eq!(sanitize("foo/bar baz"), "foo-bar-baz");
        assert_eq!(sanitize("-home-user-"), "home-user");
    }
}
