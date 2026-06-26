//! Memory writer with atomic write + per-file versioned backup.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn write_memory_file(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if path.exists() {
        let backup_dir = backup_dir()?;
        fs::create_dir_all(&backup_dir)?;
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let backup_name = format!(
            "{}.{}.bak",
            path.file_name().unwrap_or_default().to_string_lossy(),
            ts
        );
        let backup_path = backup_dir.join(backup_name);
        let _ = fs::copy(path, &backup_path);
        prune_backups(&backup_dir, 5)?;
    }

    let tmp = path.with_extension(format!(
        "{}.cortex-tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("md")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn backup_dir() -> anyhow::Result<PathBuf> {
    let local = dirs::data_local_dir().ok_or_else(|| anyhow::anyhow!("no local data dir"))?;
    Ok(local.join("cortex").join("backups"))
}

fn prune_backups(dir: &Path, keep: usize) -> anyhow::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|r| r.ok())
        .collect();
    // Sort newest-first by mtime. Treat an unreadable mtime as the oldest
    // possible time so such entries sort last and are pruned first, rather
    // than being kept ahead of valid, newer backups.
    entries.sort_by_key(|e| {
        std::cmp::Reverse(
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
    });
    for e in entries.into_iter().skip(keep) {
        let _ = fs::remove_file(e.path());
    }
    Ok(())
}
