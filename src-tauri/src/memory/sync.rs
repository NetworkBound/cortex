//! Cross-device sync over Tailscale via `rsync`. Phase 3 implements
//! scheduled + on-demand sync; Phase 5+ surfaces conflict UI in the panel.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDevice {
    pub label: String,
    pub host: String,    // tailscale ip or hostname
    pub user: String,    // typically "user"
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub peer: String,
    pub ok: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

pub async fn sync_to_peer(peer: &PeerDevice) -> SyncResult {
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut all_ok = true;

    let home = dirs::home_dir().unwrap_or_default();

    for rel in &peer.paths {
        let trimmed = rel.trim_start_matches('/');
        // Refuse empty / root-equivalent path entries. With `--delete-after`
        // an empty entry resolves `local` to $HOME and `remote` to `~/`,
        // mirroring (and deleting under) the entire home dir on the peer.
        // Also reject `..` segments that would escape $HOME.
        if trimmed.is_empty()
            || trimmed
                .split(['/', '\\'])
                .any(|seg| seg == ".." || seg == ".")
        {
            all_ok = false;
            combined_stderr.push_str(&format!(
                "rsync skipped unsafe path entry {:?} (empty or contains ..)\n",
                rel
            ));
            continue;
        }
        let local = home.join(trimmed);
        if !local.exists() {
            continue;
        }
        let remote = format!("{}@{}:~/{}", peer.user, peer.host, trimmed);
        let output = Command::new("rsync")
            .arg("-az")
            .arg("--delete-after")
            .arg("--info=stats0")
            .arg(format!("{}/", local.display()))
            .arg(&remote)
            .output()
            .await;
        match output {
            Ok(o) => {
                if !o.status.success() { all_ok = false; }
                let so = String::from_utf8_lossy(&o.stdout);
                let se = String::from_utf8_lossy(&o.stderr);
                combined_stdout.push_str(&so);
                combined_stderr.push_str(&se);
            }
            Err(e) => {
                all_ok = false;
                combined_stderr.push_str(&format!("rsync invoke failed for {}: {}\n", rel, e));
            }
        }
    }

    SyncResult {
        peer: peer.label.clone(),
        ok: all_ok,
        stdout_tail: tail(&combined_stdout, 2048),
        stderr_tail: tail(&combined_stderr, 2048),
    }
}

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max { return s.to_string(); }
    // Clamp the start index up to the next UTF-8 char boundary so we never
    // slice through a multi-byte character (which would panic).
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

pub fn default_paths() -> Vec<String> {
    vec![
        ".claude/projects/".to_string(),
        ".claude/CLAUDE.md".to_string(),
        "CLAUDE.md".to_string(),
    ]
}

pub fn config_path() -> PathBuf {
    let cfg = dirs::config_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
    cfg.join("cortex").join("peers.json")
}
