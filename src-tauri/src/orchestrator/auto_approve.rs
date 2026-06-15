//! Global auto-approve allowlist.
//!
//! Persists a list of `{tool, pattern, profile?}` entries at
//! `~/.cortex/auto-approve.json`. Patterns are matched with the `globset`
//! crate against the tool call's primary string payload (the `command` /
//! `cmd` / `path` field, or the whole serialized JSON as fallback).
//!
//! Used by `chat.rs` BEFORE emitting an `approval_request` event — a hit
//! here means the user has pre-authorized this exact shape of call and the
//! UI never sees it. Distinct from `.cortex/approvals.toml` which lives
//! per-project and uses regex; this one is user-global and uses globs so
//! `git status*` is a natural pattern to type.

use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

/// A single allowlist entry. Stored as-is in the JSON file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoApproveEntry {
    /// Tool name to match (e.g. `"bash"`, `"shell_exec"`). Compared
    /// case-insensitively. Empty string matches any tool.
    pub tool: String,
    /// Glob pattern matched against the payload (see `payload_for_match`).
    pub pattern: String,
    /// Optional profile id — surfaced in the UI but not yet enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// Compiled allowlist ready for evaluation.
pub struct AutoApproveList {
    pub entries: Vec<(AutoApproveEntry, GlobMatcher)>,
}

impl AutoApproveList {
    /// Build an empty list.
    pub fn empty() -> Self {
        Self { entries: Vec::new() }
    }

    /// Resolve `~/.cortex/auto-approve.json`. Returns `None` when the home
    /// directory can't be determined.
    pub fn file_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".cortex").join("auto-approve.json"))
    }

    /// Load the on-disk allowlist. Missing files yield an empty list;
    /// malformed files log a warning and also yield empty (fail-closed:
    /// we'd rather surface an approval prompt than silently auto-approve
    /// based on a corrupted file).
    pub fn load() -> Self {
        let Some(path) = Self::file_path() else {
            return Self::empty();
        };
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::empty(),
            Err(e) => {
                tracing::debug!("auto-approve: could not read {}: {e}", path.display());
                return Self::empty();
            }
        };
        let parsed: Vec<AutoApproveEntry> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "auto-approve: ignoring malformed {} ({e})",
                    path.display()
                );
                return Self::empty();
            }
        };
        let mut entries = Vec::with_capacity(parsed.len());
        for entry in parsed {
            match Glob::new(&entry.pattern) {
                Ok(g) => entries.push((entry, g.compile_matcher())),
                Err(e) => {
                    tracing::warn!(
                        "auto-approve: skipping invalid pattern '{}': {e}",
                        entry.pattern
                    );
                }
            }
        }
        Self { entries }
    }

    /// Pull out the candidate "command string" fields from a tool-call
    /// payload so glob matching feels natural (`git status*` vs the user
    /// having to remember the JSON shape).
    ///
    /// A payload may carry MORE THAN ONE command-bearing field (e.g. both
    /// `command` and `shell`). Returning only the first one would let a
    /// dangerous command hide behind a benign matched field, so we collect
    /// every present candidate; the caller requires all of them to match
    /// (fail-closed).
    fn payloads_for_match(payload: &serde_json::Value) -> Vec<String> {
        if let Some(obj) = payload.as_object() {
            let mut out = Vec::new();
            for key in &["command", "cmd", "shell", "bash", "path", "file"] {
                if let Some(v) = obj.get(*key).and_then(|v| v.as_str()) {
                    out.push(v.to_string());
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(s) = payload.as_str() {
            return vec![s.to_string()];
        }
        vec![payload.to_string()]
    }

    /// Returns `true` when ANY entry matches `(tool_name, payload)`.
    /// Empty `entry.tool` is a wildcard match.
    pub fn matches(&self, tool_name: &str, payload: &serde_json::Value) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let needles = Self::payloads_for_match(payload);
        // Should never be empty (payloads_for_match always yields at least
        // one element), but guard anyway: an empty candidate set must not
        // vacuously auto-approve.
        if needles.is_empty() {
            return false;
        }
        let tool_lc = tool_name.to_ascii_lowercase();
        for (entry, matcher) in &self.entries {
            let tool_ok = entry.tool.is_empty()
                || entry.tool.eq_ignore_ascii_case(&tool_lc);
            if !tool_ok {
                continue;
            }
            // Require EVERY candidate field to match — otherwise a benign
            // field could shield a dangerous sibling command from review.
            if needles.iter().all(|n| matcher.is_match(n)) {
                return true;
            }
        }
        false
    }

    /// Read the raw entries from disk (for the `list_auto_approve` cmd).
    pub fn list() -> Vec<AutoApproveEntry> {
        let Some(path) = Self::file_path() else {
            return Vec::new();
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Append an entry. The pattern is validated as a glob before write
    /// to avoid persisting garbage that will fail to compile on the next
    /// load. Duplicates (same tool + pattern) are silently de-duped.
    pub fn add(entry: AutoApproveEntry) -> anyhow::Result<()> {
        Glob::new(&entry.pattern)
            .map_err(|e| anyhow::anyhow!("invalid glob '{}': {e}", entry.pattern))?;
        let path = Self::file_path()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut entries = Self::list();
        if entries.iter().any(|e| {
            e.tool.eq_ignore_ascii_case(&entry.tool) && e.pattern == entry.pattern
        }) {
            return Ok(());
        }
        entries.push(entry);
        let buf = serde_json::to_string_pretty(&entries)?;
        fs::write(&path, buf)?;
        Ok(())
    }

    /// Remove the entry at `index` (0-based, matching `list()`). Out-of-
    /// range indices are a no-op so the UI can be loose about staleness.
    pub fn remove(index: usize) -> anyhow::Result<()> {
        let path = Self::file_path()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
        let mut entries = Self::list();
        if index >= entries.len() {
            return Ok(());
        }
        entries.remove(index);
        let buf = serde_json::to_string_pretty(&entries)?;
        fs::write(&path, buf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tool: &str, pattern: &str) -> AutoApproveEntry {
        AutoApproveEntry {
            tool: tool.into(),
            pattern: pattern.into(),
            profile: None,
        }
    }

    fn list_with(entries: Vec<AutoApproveEntry>) -> AutoApproveList {
        let mut out = Vec::new();
        for e in entries {
            let g = Glob::new(&e.pattern).unwrap().compile_matcher();
            out.push((e, g));
        }
        AutoApproveList { entries: out }
    }

    #[test]
    fn empty_list_matches_nothing() {
        let list = AutoApproveList::empty();
        assert!(!list.matches("bash", &serde_json::json!({"command": "ls"})));
    }

    #[test]
    fn matches_command_field() {
        let list = list_with(vec![entry("bash", "git status*")]);
        assert!(list.matches("bash", &serde_json::json!({"command": "git status"})));
        assert!(list.matches("bash", &serde_json::json!({"command": "git status -sb"})));
        assert!(!list.matches("bash", &serde_json::json!({"command": "rm -rf /"})));
    }

    #[test]
    fn tool_name_is_case_insensitive() {
        let list = list_with(vec![entry("Bash", "ls*")]);
        assert!(list.matches("bash", &serde_json::json!({"command": "ls -la"})));
        assert!(list.matches("BASH", &serde_json::json!({"command": "ls"})));
    }

    #[test]
    fn empty_tool_is_wildcard() {
        let list = list_with(vec![entry("", "ls*")]);
        assert!(list.matches("bash", &serde_json::json!({"command": "ls"})));
        assert!(list.matches("shell_exec", &serde_json::json!({"command": "ls -la"})));
    }

    #[test]
    fn multi_command_field_cannot_hide_dangerous_sibling() {
        // A benign `command` matches the glob, but a dangerous `shell`
        // field is also present. Auto-approve must NOT fire: every
        // command-bearing field has to match the pattern.
        let list = list_with(vec![entry("bash", "git status*")]);
        assert!(!list.matches(
            "bash",
            &serde_json::json!({"command": "git status", "shell": "rm -rf /"})
        ));
        // All present fields matching the pattern is still a hit.
        assert!(list.matches(
            "bash",
            &serde_json::json!({"command": "git status", "shell": "git status -sb"})
        ));
    }

    #[test]
    fn falls_back_to_path_field() {
        let list = list_with(vec![entry("read_file", "**/*.md")]);
        assert!(list.matches("read_file", &serde_json::json!({"path": "docs/x.md"})));
        assert!(!list.matches("read_file", &serde_json::json!({"path": "src/x.rs"})));
    }
}
