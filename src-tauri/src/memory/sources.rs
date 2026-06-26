use serde::Serialize;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    ClaudeProjectMemory,
    Runbooks,
    GlobalInstructions,
    ProjectInstructions,
    Obsidian,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySource {
    pub kind: SourceKind,
    pub root: PathBuf,
    pub label: String,
}

/// Enumerate every "home" the user might have on this machine. On Windows,
/// that includes `\\wsl.localhost\<distro>\home\<user>\` UNC paths so the
/// production cortex.exe can see Claude memories that Claude Code wrote on
/// the WSL side. On Linux/macOS, just the native home dir.
///
/// Why: user runs cortex.exe on Windows but Claude Code runs in WSL,
/// writing to a completely different filesystem. Without this, three
/// memory filter tabs silently filter to zero even though the files
/// exist a UNC-hop away.
fn all_home_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(h) = dirs::home_dir() {
        roots.push(h);
    }
    #[cfg(windows)]
    {
        // `\\wsl.localhost\<distro>\home\<user>` — distro names probed in
        // order; we accept any that responds. The Windows username is the
        // default WSL username for most setups, so try that first.
        let user = std::env::var("USERNAME")
            .ok()
            .map(|u| u.to_lowercase())
            .unwrap_or_else(|| "user".to_string());
        for distro in ["Ubuntu", "Ubuntu-24.04", "Ubuntu-22.04", "Debian"] {
            // Use the per-user UNC root then walk down; checking existence
            // forces the WSL plan9 server to wake up — a missing distro
            // simply returns false in ~10ms.
            let root = PathBuf::from(format!("\\\\wsl.localhost\\{distro}\\home\\{user}"));
            if root.exists() {
                roots.push(root);
            }
        }
    }
    roots
}

/// Build the default list of sources to scan based on `$HOME` and the
/// active project root (if any). Phase 3 wires the Obsidian path from
/// settings; for now we include only those that exist on disk.
pub fn default_sources(active_project: Option<&Path>, obsidian_vault: Option<&Path>) -> Vec<MemorySource> {
    let homes = all_home_roots();
    if homes.is_empty() { return vec![]; }
    let primary_home = homes[0].clone();
    let mut sources = Vec::new();

    // Per-home scans — covers both Windows home and any reachable WSL homes
    // so the same setup works whether user opens cortex.exe (Windows) or
    // a WSL-native dev build.
    for home in &homes {
        let claude_proj = home.join(".claude").join("projects");
        if claude_proj.exists() {
            for entry in std::fs::read_dir(&claude_proj).into_iter().flatten().flatten() {
                let mem = entry.path().join("memory");
                if mem.exists() {
                    // De-dup if the same root somehow appears twice (e.g.
                    // mapped drive + UNC path to the same dir).
                    if sources.iter().any(|s: &MemorySource| s.root == mem) { continue }
                    sources.push(MemorySource {
                        kind: SourceKind::ClaudeProjectMemory,
                        label: format!("claude:{}", entry.file_name().to_string_lossy()),
                        root: mem,
                    });
                }
            }
        }

        // Global instruction files — CLAUDE.md (Claude Code) + AGENTS.md (Codex /
        // Cursor / Zed cross-tool convention). Both are picked up automatically
        // so cortex respects whatever user already uses across other tools.
        for p in [
            home.join("CLAUDE.md"),
            home.join(".claude/CLAUDE.md"),
            home.join("AGENTS.md"),
            home.join(".cortex/AGENTS.md"),
            home.join(".codex/AGENTS.md"),
        ] {
            if p.exists() {
                if sources.iter().any(|s: &MemorySource| s.root == p) { continue }
                sources.push(MemorySource {
                    kind: SourceKind::GlobalInstructions,
                    label: p
                        .strip_prefix(home)
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|_| {
                            p.file_name().unwrap_or_default().to_string_lossy().to_string()
                        }),
                    root: p,
                });
            }
        }
    }
    // Keep `home` for the rest of the function = the primary (native) home.
    // Wave 178 — underscore prefix; this is shadowed/unused after the
    // multi-home refactor that introduced `homes` (the iterable below) but
    // we keep the binding so a future "just the primary home" path can
    // pick it back up without re-doing the discovery.
    let _home = primary_home;

    if let Some(project) = active_project {
        let runbooks = project.join("runbooks");
        if runbooks.exists() {
            sources.push(MemorySource {
                kind: SourceKind::Runbooks,
                label: "runbooks".into(),
                root: runbooks,
            });
        }
        for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md"] {
            let p = project.join(name);
            if p.exists() {
                sources.push(MemorySource {
                    kind: SourceKind::ProjectInstructions,
                    label: name.into(),
                    root: p,
                });
            }
        }
    }

    // Global runbook discovery: scan ~/projects/*/runbooks across every
    // reachable home (Windows + WSL). Lets the MemoryExplorer surface
    // user's homelab knowledge base regardless of which side the project
    // lives on.
    for home_root in &homes {
        let projects_root = home_root.join("projects");
        if projects_root.exists() {
            for entry in std::fs::read_dir(&projects_root).into_iter().flatten().flatten() {
                let runbooks = entry.path().join("runbooks");
                if !runbooks.exists() { continue }
                if sources.iter().any(|s: &MemorySource| s.root == runbooks) { continue }
                let label = format!(
                    "runbooks:{}",
                    entry.file_name().to_string_lossy()
                );
                sources.push(MemorySource {
                    kind: SourceKind::Runbooks,
                    label,
                    root: runbooks,
                });
            }
        }

        // Migration bundle — user's portable backup of his setup.
        let bundle = home_root.join("claude-migration-bundle");
        if bundle.exists() && !sources.iter().any(|s: &MemorySource| s.root == bundle) {
            sources.push(MemorySource {
                kind: SourceKind::Runbooks,
                label: "claude-migration-bundle".into(),
                root: bundle,
            });
        }
    }

    if let Some(vault) = obsidian_vault {
        if vault.exists() {
            sources.push(MemorySource {
                kind: SourceKind::Obsidian,
                label: format!("obsidian:{}", vault.file_name().unwrap_or_default().to_string_lossy()),
                root: vault.to_path_buf(),
            });
        }
    }

    sources
}

/// Iterate markdown files under a source root (skips files larger than 1 MiB).
pub fn walk_markdown(source: &MemorySource) -> Vec<PathBuf> {
    if source.root.is_file() {
        return vec![source.root.clone()];
    }
    WalkDir::new(&source.root)
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
        .filter(|e| e.metadata().map(|m| m.len() < 1024 * 1024).unwrap_or(false))
        .map(|e| e.path().to_path_buf())
        .collect()
}
