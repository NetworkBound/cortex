//! Subagent role registry — pre-built personas at `~/.cortex/roles/<name>.yaml`.
//!
//! A "role" is a re-usable agent persona (system prompt + tool allowlist +
//! suggested model) you can apply to any concrete agent. The classic example:
//! "code-reviewer" — focused on correctness/security/style, read-mostly tools,
//! a beefier model.
//!
//! On-disk schema (YAML):
//! ```yaml
//! name: code-reviewer
//! description: Reviews PRs for security, style, correctness
//! tools: [read_file, ripgrep, git_diff]
//! model: claude-opus-4-7
//! system_prompt: |
//!   You are a senior code reviewer. Focus on:
//!   - Correctness bugs ...
//! ```
//!
//! On first run we seed five sensible defaults (code-reviewer, test-writer,
//! security-auditor, docs-writer, bug-triager) so the picker is never empty.
//! Subsequent runs see those files already on disk and skip the seed — users
//! can freely edit/delete them without us clobbering their work.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A single role loaded from `~/.cortex/roles/<name>.yaml`. All fields except
/// `name` are optional so a role can carry just the dimensions it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// On-disk form: `name` is optional so the filename can supply it.
#[derive(Debug, Deserialize)]
struct RoleFile {
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
}

/// Location of the roles directory: `~/.cortex/roles/`.
pub fn roles_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("roles"))
}

/// Reject `name`s that contain path separators or `..` so callers can't escape
/// the roles dir. Empty names are also refused.
fn is_safe_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains("..")
}

fn parse_role(raw: &str, fallback_name: &str) -> anyhow::Result<Role> {
    let parsed: RoleFile = serde_yaml::from_str(raw)?;
    Ok(Role {
        name: parsed.name.unwrap_or_else(|| fallback_name.to_string()),
        description: parsed.description,
        tools: parsed.tools,
        model: parsed.model,
        system_prompt: parsed.system_prompt,
    })
}

/// List every role under `~/.cortex/roles/*.yaml`, sorted by name. Malformed
/// files are skipped (with a debug log) — one bad file shouldn't hide the rest.
pub fn list_roles() -> Vec<Role> {
    let Some(dir) = roles_dir() else { return Vec::new() };
    let read = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("roles: no dir ({}): {e}", dir.display());
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|s| s.to_str()) {
            Some("yaml") | Some("yml") => {}
            _ => continue,
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("roles: read failed for {}: {e}", path.display());
                continue;
            }
        };
        match parse_role(&raw, &stem) {
            Ok(r) => out.push(r),
            Err(e) => tracing::debug!("roles: parse failed for {}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    // Two files can resolve to the same logical `name` (e.g. a `.yaml` and a
    // `.yml` with the same stem, or an explicit `name:` colliding with another
    // file's stem). Drop the duplicates so callers see each name once; the list
    // is already sorted, so equal names are adjacent and `dedup_by` keeps the
    // first.
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// Load a single role by filename stem. Returns `None` on missing / malformed.
pub fn get_role(name: &str) -> Option<Role> {
    if !is_safe_name(name) {
        return None;
    }
    let dir = roles_dir()?;
    for ext in ["yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if let Ok(raw) = fs::read_to_string(&path) {
            match parse_role(&raw, name) {
                Ok(r) => return Some(r),
                Err(e) => {
                    tracing::debug!("roles: parse failed for {}: {e}", path.display());
                    return None;
                }
            }
        }
    }
    None
}

/// Persist a role to `~/.cortex/roles/<name>.yaml`. Creates the directory if
/// needed. Refuses names with path separators.
pub fn set_role(role: &Role) -> anyhow::Result<()> {
    if !is_safe_name(&role.name) {
        anyhow::bail!("invalid role name '{}'", role.name);
    }
    let dir = roles_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    fs::create_dir_all(&dir)?;
    // Build the path from the trimmed name so it matches the logical name that
    // `get_role`/`delete_role` look up by; otherwise a name with surrounding
    // whitespace is written to a file no one can find by its logical name.
    let path = dir.join(format!("{}.yaml", role.name.trim()));
    let body = serde_yaml::to_string(role)?;
    fs::write(&path, body)?;
    Ok(())
}

/// Remove a role file. Missing files are a no-op (idempotent delete).
pub fn delete_role(name: &str) -> anyhow::Result<()> {
    if !is_safe_name(name) {
        anyhow::bail!("invalid role name '{name}'");
    }
    let Some(dir) = roles_dir() else {
        return Ok(());
    };
    for ext in ["yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if path.exists() {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The five seed roles written on first run. We only seed once: if the
/// `~/.cortex/roles/` directory already exists we leave it alone, so users
/// can delete a default and not have it come back next launch.
pub fn seed_defaults_if_missing() {
    let Some(dir) = roles_dir() else { return };
    if dir.exists() {
        return;
    }
    if let Err(e) = fs::create_dir_all(&dir) {
        tracing::debug!("roles: seed mkdir failed at {}: {e}", dir.display());
        return;
    }
    for role in default_roles() {
        if let Err(e) = set_role(&role) {
            tracing::debug!("roles: seed write failed for {}: {e}", role.name);
        }
    }
}

fn default_roles() -> Vec<Role> {
    vec![
        Role {
            name: "code-reviewer".into(),
            description: Some("Reviews PRs for security, style, correctness".into()),
            tools: Some(vec!["read_file".into(), "ripgrep".into(), "git_diff".into()]),
            model: Some("claude-opus-4-7".into()),
            system_prompt: Some(
                "You are a senior code reviewer. Focus on:\n\
                 - Correctness bugs (off-by-one, null deref, race conditions)\n\
                 - Security issues (injection, broken auth, secret leaks)\n\
                 - Style consistency with surrounding code\n\
                 - Test coverage gaps\n\
                 Be specific. Quote line numbers. Prefer concrete suggestions\n\
                 over vague feedback."
                    .into(),
            ),
        },
        Role {
            name: "test-writer".into(),
            description: Some("Writes targeted unit + integration tests".into()),
            tools: Some(vec![
                "read_file".into(),
                "write_file".into(),
                "ripgrep".into(),
                "shell".into(),
            ]),
            model: Some("claude-sonnet-4-6".into()),
            system_prompt: Some(
                "You write tests. Prefer fast unit tests over heavy integration\n\
                 tests. Cover happy path + at least two error cases per public\n\
                 function. Match the existing test framework and naming style\n\
                 of the surrounding code. Don't ship a test you haven't run."
                    .into(),
            ),
        },
        Role {
            name: "security-auditor".into(),
            description: Some("Hunts for injection, auth, and secret-leak bugs".into()),
            tools: Some(vec!["read_file".into(), "ripgrep".into(), "git_diff".into()]),
            model: Some("claude-opus-4-7".into()),
            system_prompt: Some(
                "You audit for security issues. Look hard for:\n\
                 - Injection (SQL, command, template, prompt)\n\
                 - Broken authn/authz, missing CSRF, session fixation\n\
                 - Secret material in source / logs / commits\n\
                 - Unsafe deserialization, path traversal, SSRF\n\
                 Cite the file + line. If you're unsure, mark the finding\n\
                 'suspected' instead of 'confirmed'."
                    .into(),
            ),
        },
        Role {
            name: "docs-writer".into(),
            description: Some("Writes concise developer-facing docs".into()),
            tools: Some(vec![
                "read_file".into(),
                "write_file".into(),
                "ripgrep".into(),
            ]),
            model: Some("claude-sonnet-4-6".into()),
            system_prompt: Some(
                "You write developer documentation. Be concise. Prefer code\n\
                 examples over prose. Document the why, not just the what.\n\
                 Match the project's existing doc style and Markdown\n\
                 conventions. Never invent API surface that doesn't exist."
                    .into(),
            ),
        },
        Role {
            name: "bug-triager".into(),
            description: Some("Reproduces bugs and isolates the root cause".into()),
            tools: Some(vec![
                "read_file".into(),
                "ripgrep".into(),
                "shell".into(),
                "git_diff".into(),
            ]),
            model: Some("claude-sonnet-4-6".into()),
            system_prompt: Some(
                "You triage bugs. Steps: (1) reproduce, (2) narrow the failing\n\
                 input, (3) identify the offending commit / function, (4)\n\
                 propose the smallest fix. Output a short root-cause summary\n\
                 and a suggested patch. Don't speculate without evidence."
                    .into(),
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests poke the user's real $HOME by default; serialize them so they don't
    // race on the shared roles dir.
    static LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_home<F: FnOnce()>(f: F) {
        let _g = LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn list_empty_when_dir_missing() {
        with_temp_home(|| {
            assert!(list_roles().is_empty());
        });
    }

    #[test]
    fn set_then_list_then_get() {
        with_temp_home(|| {
            let role = Role {
                name: "demo".into(),
                description: Some("a demo".into()),
                tools: Some(vec!["read_file".into()]),
                model: Some("claude-opus-4-7".into()),
                system_prompt: Some("hello".into()),
            };
            set_role(&role).unwrap();
            let listed = list_roles();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "demo");
            let fetched = get_role("demo").unwrap();
            assert_eq!(fetched, role);
        });
    }

    #[test]
    fn delete_is_idempotent() {
        with_temp_home(|| {
            delete_role("ghost").unwrap();
        });
    }

    #[test]
    fn rejects_path_traversal() {
        with_temp_home(|| {
            assert!(get_role("../etc/passwd").is_none());
            assert!(get_role("sub/dir").is_none());
            assert!(set_role(&Role {
                name: "../evil".into(),
                description: None,
                tools: None,
                model: None,
                system_prompt: None,
            })
            .is_err());
        });
    }

    #[test]
    fn seed_only_once() {
        with_temp_home(|| {
            seed_defaults_if_missing();
            let first = list_roles();
            assert!(first.iter().any(|r| r.name == "code-reviewer"));
            // Delete one and re-seed: it must NOT come back.
            delete_role("code-reviewer").unwrap();
            seed_defaults_if_missing();
            let second = list_roles();
            assert!(second.iter().all(|r| r.name != "code-reviewer"));
        });
    }

    #[test]
    fn name_falls_back_to_filename() {
        with_temp_home(|| {
            let dir = roles_dir().unwrap();
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("scratch.yaml"),
                "description: hi\nmodel: claude-opus-4-7\n",
            )
            .unwrap();
            let r = get_role("scratch").unwrap();
            assert_eq!(r.name, "scratch");
            assert_eq!(r.model.as_deref(), Some("claude-opus-4-7"));
        });
    }
}
