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
    /// The project directory this source's content belongs to, if any — used
    /// to tag indexed chunks so RAG retrieval can scope them to that project
    /// (issue 010 full scope: per-project memory namespaces). `None` marks
    /// "global" content (Obsidian vault, global instructions, Claude Code's
    /// own per-tool memory dirs) that stays visible from every project — a
    /// fallback tier, never a leak vector since it was never project-private.
    pub owner_project: Option<PathBuf>,
}

/// Enumerate every "home" the user might have on this machine. On Windows,
/// that includes `\\wsl.localhost\<distro>\home\<user>\` UNC paths so the
/// production cortex.exe can see Claude memories that Claude Code wrote on
/// the WSL side. On Linux/macOS, just the native home dir.
///
/// Why: the app often runs on Windows while Claude Code runs in WSL,
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
        // default WSL username for most setups, so try that first; when it
        // doesn't match (or the env var is absent), list whatever home dirs
        // the distro actually has instead of guessing a literal name.
        let user = std::env::var("USERNAME").ok().map(|u| u.to_lowercase());
        for distro in ["Ubuntu", "Ubuntu-24.04", "Ubuntu-22.04", "Debian"] {
            // Use the per-user UNC root then walk down; checking existence
            // forces the WSL plan9 server to wake up — a missing distro
            // simply returns false in ~10ms.
            if let Some(u) = &user {
                let root = PathBuf::from(format!("\\\\wsl.localhost\\{distro}\\home\\{u}"));
                if root.exists() {
                    roots.push(root);
                    continue;
                }
            }
            let home_root = PathBuf::from(format!("\\\\wsl.localhost\\{distro}\\home"));
            if let Ok(rd) = std::fs::read_dir(&home_root) {
                roots.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
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
    // so the same setup works whether the user opens cortex.exe (Windows) or
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
                        // Keyed by Claude Code's own project slug, not a Cortex
                        // project root we can map back to — treat as global.
                        owner_project: None,
                    });
                }
            }
        }

        // Global instruction files — CLAUDE.md (Claude Code) + AGENTS.md (Codex /
        // Cursor / Zed cross-tool convention). Both are picked up automatically
        // so cortex respects whatever the user already uses across other tools.
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
                    owner_project: None,
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
                owner_project: Some(project.to_path_buf()),
            });
        }
        for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md"] {
            let p = project.join(name);
            if p.exists() {
                sources.push(MemorySource {
                    kind: SourceKind::ProjectInstructions,
                    label: name.into(),
                    root: p,
                    owner_project: Some(project.to_path_buf()),
                });
            }
        }
    }

    // Global runbook discovery: scan ~/projects/*/runbooks across every
    // reachable home (Windows + WSL). Lets the MemoryExplorer surface
    // the user's knowledge base regardless of which side the project
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
                    // Discovered regardless of which project is active, but
                    // still belongs to ITS OWN project dir — tag it so
                    // retrieval keeps this private to that project rather
                    // than leaking into every other project's answers.
                    owner_project: Some(entry.path()),
                    root: runbooks,
                });
            }
        }

        // Migration bundle — the user's portable backup of their setup. Not
        // tied to any single project; stays global.
        let bundle = home_root.join("claude-migration-bundle");
        if bundle.exists() && !sources.iter().any(|s: &MemorySource| s.root == bundle) {
            sources.push(MemorySource {
                kind: SourceKind::Runbooks,
                label: "claude-migration-bundle".into(),
                root: bundle,
                owner_project: None,
            });
        }
    }

    if let Some(vault) = obsidian_vault {
        if vault.exists() {
            sources.push(MemorySource {
                kind: SourceKind::Obsidian,
                label: format!("obsidian:{}", vault.file_name().unwrap_or_default().to_string_lossy()),
                root: vault.to_path_buf(),
                owner_project: None,
            });
        }
    }

    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-project memory must not leak across projects: with project A
    /// active, project-scoped sources (runbooks/, CLAUDE.md, AGENTS.md) must
    /// come ONLY from A — nothing under a sibling project B may appear.
    /// (B lives outside ~/projects and ~/.claude/projects, so the global
    /// discovery scans can't pick it up either.)
    #[test]
    fn project_sources_do_not_leak_across_projects() {
        let root = tempfile::tempdir().expect("tempdir");
        let a = root.path().join("proj-a");
        let b = root.path().join("proj-b");
        for p in [&a, &b] {
            std::fs::create_dir_all(p.join("runbooks")).unwrap();
            std::fs::write(p.join("runbooks").join("notes.md"), "# runbook").unwrap();
            std::fs::write(p.join("CLAUDE.md"), "# project instructions").unwrap();
        }

        let sources = default_sources(Some(&a), None);
        assert!(
            sources.iter().any(|s| s.root.starts_with(&a)),
            "active project's own sources must be included"
        );
        assert!(
            !sources.iter().any(|s| s.root.starts_with(&b)),
            "inactive project B leaked into project A's sources"
        );

        // Symmetric check: activating B must not surface A.
        let sources_b = default_sources(Some(&b), None);
        assert!(sources_b.iter().any(|s| s.root.starts_with(&b)));
        assert!(!sources_b.iter().any(|s| s.root.starts_with(&a)));
    }

    /// Project-scoped sources (the active project's own runbooks/instructions,
    /// and any OTHER project's runbooks discovered via the global `~/projects`
    /// scan) must carry `owner_project` so retrieval can keep them private to
    /// that project. Global sources (home instructions, Obsidian) must not.
    #[test]
    fn owner_project_tags_project_sources_but_not_global_ones() {
        let root = tempfile::tempdir().expect("tempdir");
        let a = root.path().join("proj-a");
        std::fs::create_dir_all(a.join("runbooks")).unwrap();
        std::fs::write(a.join("runbooks").join("notes.md"), "# runbook").unwrap();
        std::fs::write(a.join("CLAUDE.md"), "# project instructions").unwrap();

        let sources = default_sources(Some(&a), None);
        let runbooks = sources
            .iter()
            .find(|s| s.kind == SourceKind::Runbooks && s.root.starts_with(&a))
            .expect("project runbooks source present");
        assert_eq!(runbooks.owner_project.as_deref(), Some(a.as_path()));

        let instructions = sources
            .iter()
            .find(|s| s.kind == SourceKind::ProjectInstructions)
            .expect("project instructions source present");
        assert_eq!(instructions.owner_project.as_deref(), Some(a.as_path()));

        // Global sources (if any turned up on this machine) must never carry
        // an owner_project — they're the fallback tier, visible everywhere.
        assert!(sources
            .iter()
            .filter(|s| matches!(s.kind, SourceKind::GlobalInstructions | SourceKind::ClaudeProjectMemory))
            .all(|s| s.owner_project.is_none()));
    }

    /// The Obsidian vault source is opt-in per config: absent a vault path,
    /// no Obsidian source may appear.
    #[test]
    fn vault_only_included_when_configured() {
        let root = tempfile::tempdir().expect("tempdir");
        let vault = root.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();

        let with = default_sources(None, Some(&vault));
        assert!(with.iter().any(|s| s.kind == SourceKind::Obsidian && s.root == vault));

        let without = default_sources(None, None);
        assert!(!without.iter().any(|s| s.kind == SourceKind::Obsidian && s.root == vault));
    }
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
