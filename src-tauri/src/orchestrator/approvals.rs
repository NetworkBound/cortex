//! Persistent approval rules.
//!
//! Stores per-project regex → decision mappings in
//! `<project_root>/.cortex/approvals.toml`. Consulted from `chat.rs` BEFORE
//! emitting an `approval_request` event so the user can pre-authorize (or
//! pre-reject) repetitive tool invocations.
//!
//! On-disk schema:
//! ```toml
//! [[rule]]
//! pattern  = "^bash echo "
//! decision = "approve"
//!
//! [[rule]]
//! pattern  = "rm -rf /"
//! decision = "deny"
//! ```
//!
//! High-risk guardrail hits ALWAYS override an `approve` decision — that
//! gating lives in the caller (see `chat.rs`).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Outcome of a matching rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Approve,
    Deny,
}

impl Decision {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "approve" | "allow" => Some(Decision::Approve),
            "deny" | "reject" => Some(Decision::Deny),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Decision::Approve => "approve",
            Decision::Deny => "deny",
        }
    }
}

/// Compiled approval ruleset.
pub struct ApprovalRules {
    pub patterns: Vec<(Regex, Decision)>,
}

/// On-disk schema for `.cortex/approvals.toml`.
#[derive(Debug, Deserialize)]
struct ApprovalsFile {
    #[serde(default)]
    rule: Vec<ApprovalRule>,
}

#[derive(Debug, Deserialize)]
struct ApprovalRule {
    pattern: String,
    decision: String,
}

impl ApprovalRules {
    /// Build an empty ruleset (used when no file exists / on parse error).
    pub fn empty() -> Self {
        Self { patterns: Vec::new() }
    }

    /// Load rules from `<project_root>/.cortex/approvals.toml`. Missing or
    /// malformed files yield an empty ruleset — approval rules are strictly
    /// opt-in.
    pub fn load(project_root: &Path) -> Self {
        let path = Self::rules_path(project_root);
        match Self::load_from_file(&path) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    "approvals: no rules loaded ({}): {e}",
                    path.display()
                );
                Self::empty()
            }
        }
    }

    fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = fs::read_to_string(path)?;
        let parsed: ApprovalsFile = toml::from_str(&raw)?;
        let mut patterns = Vec::with_capacity(parsed.rule.len());
        for rule in parsed.rule {
            let decision = Decision::parse(&rule.decision).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid decision '{}' for pattern '{}'",
                    rule.decision,
                    rule.pattern
                )
            })?;
            let re = Regex::new(&rule.pattern).map_err(|e| {
                anyhow::anyhow!("invalid regex '{}': {e}", rule.pattern)
            })?;
            patterns.push((re, decision));
        }
        Ok(Self { patterns })
    }

    /// First matching pattern wins. Matched against `name\npayload_json`,
    /// mirroring `Guardrails::evaluate`.
    pub fn evaluate(&self, call_name: &str, payload_json: &str) -> Option<Decision> {
        let haystack = format!("{call_name}\n{payload_json}");
        for (re, decision) in &self.patterns {
            if re.is_match(&haystack) {
                return Some(*decision);
            }
        }
        None
    }

    /// Append a new rule to the on-disk file (creating it if missing). The
    /// pattern is validated as a regex before being written.
    pub fn append_rule(
        project_root: &Path,
        pattern: &str,
        decision: Decision,
    ) -> anyhow::Result<()> {
        Regex::new(pattern).map_err(|e| anyhow::anyhow!("invalid regex '{pattern}': {e}"))?;

        let path = Self::rules_path(project_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let needs_leading_newline = path
            .metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false);

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let escaped_pattern = pattern.replace('\\', "\\\\").replace('"', "\\\"");
        let block = format!(
            "{}[[rule]]\npattern  = \"{}\"\ndecision = \"{}\"\n",
            if needs_leading_newline { "\n" } else { "" },
            escaped_pattern,
            decision.as_str(),
        );
        file.write_all(block.as_bytes())?;
        Ok(())
    }

    fn rules_path(project_root: &Path) -> PathBuf {
        project_root.join(".cortex").join("approvals.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_when_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let rules = ApprovalRules::load(tmp.path());
        assert!(rules.patterns.is_empty());
        assert!(rules.evaluate("bash", "{}").is_none());
    }

    #[test]
    fn first_match_wins() {
        let tmp = tempfile::tempdir().unwrap();
        ApprovalRules::append_rule(tmp.path(), r"^bash echo ", Decision::Approve).unwrap();
        ApprovalRules::append_rule(tmp.path(), r"echo", Decision::Deny).unwrap();
        let rules = ApprovalRules::load(tmp.path());
        assert_eq!(
            rules.evaluate("bash echo hi", r#"{"cmd":"echo hi"}"#),
            Some(Decision::Approve)
        );
    }

    #[test]
    fn deny_rule_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        ApprovalRules::append_rule(tmp.path(), r#"rm -rf /""#, Decision::Deny).unwrap();
        let rules = ApprovalRules::load(tmp.path());
        // Pattern is anchored to a closing `"`, so a subpath like /tmp/x
        // does NOT match — only the literal `rm -rf /"` does.
        assert_eq!(
            rules.evaluate("shell_exec", r#"{"cmd":"rm -rf /tmp/x"}"#),
            None,
        );
        assert_eq!(
            rules.evaluate("shell_exec", r#"{"cmd":"rm -rf /"}"#),
            Some(Decision::Deny)
        );
    }

    #[test]
    fn append_creates_file_and_dir() {
        let tmp = tempfile::tempdir().unwrap();
        ApprovalRules::append_rule(tmp.path(), r"^foo", Decision::Approve).unwrap();
        let p = tmp.path().join(".cortex").join("approvals.toml");
        assert!(p.exists());
        let contents = fs::read_to_string(&p).unwrap();
        assert!(contents.contains("pattern  = \"^foo\""));
        assert!(contents.contains("decision = \"approve\""));
    }

    #[test]
    fn append_multiple_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        ApprovalRules::append_rule(tmp.path(), r"^a", Decision::Approve).unwrap();
        ApprovalRules::append_rule(tmp.path(), r"^b", Decision::Deny).unwrap();
        let rules = ApprovalRules::load(tmp.path());
        assert_eq!(rules.patterns.len(), 2);
        assert_eq!(rules.evaluate("a thing", ""), Some(Decision::Approve));
        assert_eq!(rules.evaluate("b thing", ""), Some(Decision::Deny));
    }

    #[test]
    fn append_rejects_bad_regex() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ApprovalRules::append_rule(tmp.path(), r"(", Decision::Approve);
        assert!(err.is_err());
        // File should not have been created with junk.
        let p = tmp.path().join(".cortex").join("approvals.toml");
        assert!(!p.exists());
    }

    #[test]
    fn decision_parse_aliases() {
        assert_eq!(Decision::parse("approve"), Some(Decision::Approve));
        assert_eq!(Decision::parse("allow"), Some(Decision::Approve));
        assert_eq!(Decision::parse("DENY"), Some(Decision::Deny));
        assert_eq!(Decision::parse("reject"), Some(Decision::Deny));
        assert_eq!(Decision::parse("maybe"), None);
    }
}
