//! Per-project model + sandbox + tool-allowlist bundles.
//!
//! A "profile" lives at `<project_root>/.cortex/profiles/<name>.toml` and
//! captures the knobs you'd otherwise have to set one-at-a-time when
//! switching between, say, "read-only research" and "let it rip on a
//! worktree". Mirrors the on-disk style of `approvals.toml` /
//! `danger.toml` for consistency.
//!
//! On-disk schema:
//! ```toml
//! name             = "read-only"
//! model            = "gateway-agent"
//! reasoning_effort = "medium"          # low | medium | high
//! sandbox_tier     = "read-only"       # read-only | workspace-write | danger-full-access
//! allowed_tools    = ["read_file", "grep_*"]   # glob list; omit for all
//! system_prompt    = "You are in audit mode…"
//! ```
//!
//! Profiles are pure data — the loader doesn't apply them. That happens in
//! `commands::profiles::apply_profile`, which mutates `AppState::config`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A single profile loaded from `<project_root>/.cortex/profiles/<name>.toml`.
///
/// All fields except `name` are optional so a profile can carry just the
/// dimensions it cares about (e.g. a "scratch" profile that only flips
/// `sandbox_tier`). `name` is taken from the filename if absent on disk.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// On-disk form: identical to `Profile` but with an optional `name` so the
/// filename can supply it. Kept private so the public API hands callers a
/// fully-populated `Profile`.
#[derive(Debug, Deserialize)]
struct ProfileFile {
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    sandbox_tier: Option<String>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    system_prompt: Option<String>,
}

fn profiles_dir(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("profiles")
}

/// Validates that the value is one of the documented sandbox tiers. We
/// accept anything in `Option::None` form, but if someone wrote a typo we
/// don't want to silently apply garbage to the AppState.
pub fn is_valid_sandbox_tier(s: &str) -> bool {
    matches!(s, "read-only" | "workspace-write" | "danger-full-access")
}

/// Same idea for reasoning effort.
pub fn is_valid_reasoning_effort(s: &str) -> bool {
    matches!(s, "low" | "medium" | "high")
}

fn parse_profile(raw: &str, fallback_name: &str) -> anyhow::Result<Profile> {
    let parsed: ProfileFile = toml::from_str(raw)?;
    if let Some(tier) = parsed.sandbox_tier.as_deref() {
        if !is_valid_sandbox_tier(tier) {
            anyhow::bail!("invalid sandbox_tier '{tier}'");
        }
    }
    if let Some(eff) = parsed.reasoning_effort.as_deref() {
        if !is_valid_reasoning_effort(eff) {
            anyhow::bail!("invalid reasoning_effort '{eff}'");
        }
    }
    Ok(Profile {
        name: parsed.name.unwrap_or_else(|| fallback_name.to_string()),
        model: parsed.model,
        reasoning_effort: parsed.reasoning_effort,
        sandbox_tier: parsed.sandbox_tier,
        allowed_tools: parsed.allowed_tools,
        system_prompt: parsed.system_prompt,
    })
}

/// Read every `<project_root>/.cortex/profiles/*.toml` and return the
/// successfully parsed ones, sorted by name. Malformed files are skipped
/// (with a debug log) — one bad profile shouldn't hide the rest.
pub fn list_profiles(project_root: &Path) -> Vec<Profile> {
    let dir = profiles_dir(project_root);
    let read = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("profiles: no dir ({}): {e}", dir.display());
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() { continue; }
        if path.extension().and_then(|s| s.to_str()) != Some("toml") { continue; }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("profiles: read failed for {}: {e}", path.display());
                continue;
            }
        };
        match parse_profile(&raw, &name) {
            Ok(p) => out.push(p),
            Err(e) => tracing::debug!("profiles: parse failed for {}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Load a single profile by filename stem. `None` on missing/malformed.
pub fn load_profile(project_root: &Path, name: &str) -> Option<Profile> {
    // Disallow path separators so callers can't escape the profiles dir.
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let path = profiles_dir(project_root).join(format!("{name}.toml"));
    let raw = fs::read_to_string(&path).ok()?;
    match parse_profile(&raw, name) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::debug!("profiles: parse failed for {}: {e}", path.display());
            None
        }
    }
}

// ── Per-agent custom instructions ───────────────────────────────────────────
//
// Each agent (keyed by `agent_id`) can carry a free-form `custom_instructions`
// string that gets prepended to the system prompt at chat-time. Storage is a
// flat JSON map at `~/.cortex/agent-instructions.json`:
//
// ```json
// { "gateway-remote": "Always respond in concise bullet points.",
//   "claude":        "Prefer TypeScript examples." }
// ```
//
// Missing file / parse errors degrade to "no overrides" — never an error to
// callers. Writes go through a temp-file + rename for crash-safety, same
// pattern as `trust.rs`.

fn instructions_file() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".cortex").join("agent-instructions.json"))
}

fn load_instructions_map() -> HashMap<String, String> {
    let Some(path) = instructions_file() else { return HashMap::new() };
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("agent-instructions: no file ({}): {e}", path.display());
            return HashMap::new();
        }
    };
    match serde_json::from_str::<HashMap<String, String>>(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("agent-instructions: bad json at {}: {e}", path.display());
            HashMap::new()
        }
    }
}

fn save_instructions_map(map: &HashMap<String, String>) -> anyhow::Result<()> {
    let path = instructions_file().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(map)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all().ok();
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Returns the custom instructions for `agent_id`, or `None` if unset / empty
/// / unreadable. The empty-string case is treated as "unset" so the UI can
/// clear an entry just by saving an empty textarea.
pub fn get_agent_instructions(agent_id: &str) -> Option<String> {
    let map = load_instructions_map();
    map.get(agent_id)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Persist `text` as the custom instructions for `agent_id`. An empty / blank
/// `text` removes the entry. Returns the final stored value (or empty string
/// after a remove) so the UI can confirm what landed on disk.
pub fn set_agent_instructions(agent_id: &str, text: &str) -> anyhow::Result<String> {
    if agent_id.trim().is_empty() {
        anyhow::bail!("agent_id is required");
    }
    let mut map = load_instructions_map();
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        map.remove(agent_id);
        save_instructions_map(&map)?;
        return Ok(String::new());
    }
    map.insert(agent_id.to_string(), trimmed.clone());
    save_instructions_map(&map)?;
    Ok(trimmed)
}

/// Prepend the per-agent custom instructions to `base_system` when present.
/// `base_system` may be empty — in that case the custom block stands alone.
/// Returns `None` when the agent has nothing configured so callers can skip
/// the system-message construction entirely.
pub fn compose_system_prompt(agent_id: &str, base_system: Option<&str>) -> Option<String> {
    let custom = get_agent_instructions(agent_id)?;
    match base_system.map(str::trim).filter(|s| !s.is_empty()) {
        Some(base) => Some(format!("{custom}\n\n---\n\n{base}")),
        None => Some(custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_profile(root: &Path, name: &str, body: &str) {
        let dir = profiles_dir(root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.toml")), body).unwrap();
    }

    #[test]
    fn list_empty_when_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_profiles(tmp.path()).is_empty());
    }

    #[test]
    fn list_skips_non_toml_and_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = profiles_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ok.toml"), r#"name = "ok""#).unwrap();
        fs::write(dir.join("readme.md"), "not a profile").unwrap();
        fs::write(dir.join("bad.toml"), "this is not toml = = =").unwrap();
        let profiles = list_profiles(tmp.path());
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "ok");
    }

    #[test]
    fn list_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "z-last", r#"name = "z-last""#);
        write_profile(tmp.path(), "a-first", r#"name = "a-first""#);
        let names: Vec<_> = list_profiles(tmp.path()).into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["a-first", "z-last"]);
    }

    #[test]
    fn name_falls_back_to_filename() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "scratch", "model = \"gateway-agent\"\n");
        let p = load_profile(tmp.path(), "scratch").unwrap();
        assert_eq!(p.name, "scratch");
        assert_eq!(p.model.as_deref(), Some("gateway-agent"));
    }

    #[test]
    fn loads_full_profile() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(
            tmp.path(),
            "read-only",
            r#"
name             = "read-only"
model            = "gateway-agent"
reasoning_effort = "high"
sandbox_tier     = "read-only"
allowed_tools    = ["read_file", "grep_*"]
system_prompt    = "Audit only."
"#,
        );
        let p = load_profile(tmp.path(), "read-only").unwrap();
        assert_eq!(p.name, "read-only");
        assert_eq!(p.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(p.sandbox_tier.as_deref(), Some("read-only"));
        assert_eq!(p.allowed_tools.unwrap(), vec!["read_file", "grep_*"]);
        assert_eq!(p.system_prompt.as_deref(), Some("Audit only."));
    }

    #[test]
    fn rejects_invalid_sandbox_tier() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "bad", r#"sandbox_tier = "yolo""#);
        assert!(load_profile(tmp.path(), "bad").is_none());
    }

    #[test]
    fn rejects_invalid_reasoning_effort() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "bad", r#"reasoning_effort = "extreme""#);
        assert!(load_profile(tmp.path(), "bad").is_none());
    }

    #[test]
    fn load_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_profile(tmp.path(), "ghost").is_none());
    }

    #[test]
    fn load_blocks_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        // Even if a file existed, traversal-looking names must be refused
        // before touching disk.
        assert!(load_profile(tmp.path(), "../etc/passwd").is_none());
        assert!(load_profile(tmp.path(), "..").is_none());
        assert!(load_profile(tmp.path(), "sub/dir").is_none());
    }
}
