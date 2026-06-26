//! Cortex orchestrator guardrails.
//!
//! Runs every outgoing tool-call / approval-request payload through a small
//! set of regex rules before it's surfaced to the user. The goal is *not* to
//! block — the gateway / the model already gated this with its own approval — but
//! to give Cortex's UI enough signal to flag obviously dangerous calls with a
//! red banner and force explicit (no-auto) approval for high-risk ones.
//!
//! Rules come from `<project_root>/.cortex/danger.toml`. If that file is
//! missing or malformed we fall back to a hard-coded set of defaults so the
//! feature is always on.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Risk level returned by a guardrail hit.
///
/// Order matters: `High > Medium > Low`. `Guardrails::evaluate` uses this to
/// pick the worst match when multiple rules fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Risk::Low),
            "med" | "medium" => Some(Risk::Medium),
            "high" => Some(Risk::High),
            _ => None,
        }
    }
}

/// A compiled guardrail: pattern + human reason + risk level.
pub struct Guardrails {
    pub patterns: Vec<(Regex, String, Risk)>,
}

/// On-disk schema for `.cortex/danger.toml`.
#[derive(Debug, Deserialize)]
struct DangerFile {
    #[serde(default)]
    rule: Vec<DangerRule>,
}

#[derive(Debug, Deserialize)]
struct DangerRule {
    pattern: String,
    reason: String,
    risk: String,
}

impl Guardrails {
    /// Load rules from `<project_root>/.cortex/danger.toml`, falling back to
    /// built-in defaults on any error (missing file, bad TOML, bad regex).
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".cortex").join("danger.toml");
        match Self::load_from_file(&path) {
            Ok(g) if !g.patterns.is_empty() => g,
            Ok(_) => Self::defaults(),
            Err(e) => {
                tracing::debug!(
                    "guardrails: falling back to defaults ({}): {e}",
                    path.display()
                );
                Self::defaults()
            }
        }
    }

    fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = fs::read_to_string(path)?;
        let parsed: DangerFile = toml::from_str(&raw)?;
        let mut patterns = Vec::with_capacity(parsed.rule.len());
        for rule in parsed.rule {
            let risk = Risk::parse(&rule.risk).ok_or_else(|| {
                anyhow::anyhow!("invalid risk '{}' for pattern '{}'", rule.risk, rule.pattern)
            })?;
            let re = Regex::new(&rule.pattern).map_err(|e| {
                anyhow::anyhow!("invalid regex '{}': {e}", rule.pattern)
            })?;
            patterns.push((re, rule.reason, risk));
        }
        Ok(Self { patterns })
    }

    /// Hard-coded default ruleset. Used when no `.cortex/danger.toml` exists
    /// or it fails to parse. Mirrors the SECURITY.md tripwire list.
    pub fn defaults() -> Self {
        let raw: &[(&str, &str, Risk)] = &[
            (
                r"rm\s+-rf\s+/",
                "recursive-force delete from root",
                Risk::High,
            ),
            (
                r"git\s+push.*--force",
                "force-push rewrites remote history",
                Risk::High,
            ),
            (
                r"AWS_SECRET|ANTHROPIC_API_KEY|sk-[A-Za-z0-9]{20,}",
                "looks like a leaked API secret",
                Risk::High,
            ),
            (
                r"\.env(\.local|\.production)?\b",
                "touches a .env file (likely secrets)",
                Risk::High,
            ),
            (
                r"curl.*\b(0\.0\.0\.0|169\.254\.169\.254)",
                "request to metadata / wildcard host (SSRF risk)",
                Risk::Medium,
            ),
            (
                r"(?i)DROP\s+TABLE|TRUNCATE\s+TABLE",
                "destructive SQL (DROP/TRUNCATE TABLE)",
                Risk::High,
            ),
        ];

        let patterns = raw
            .iter()
            .filter_map(|(pat, reason, risk)| {
                Regex::new(pat)
                    .ok()
                    .map(|re| (re, (*reason).to_string(), *risk))
            })
            .collect();

        Self { patterns }
    }

    /// Evaluate a tool call against every rule. Concatenates `name` and
    /// `payload_json` (the latter is the serialized args), runs all patterns,
    /// and returns the worst (highest-risk) hit if any matched.
    pub fn evaluate(&self, call_name: &str, payload_json: &str) -> Option<(Risk, String)> {
        let haystack = format!("{call_name}\n{payload_json}");
        let mut worst: Option<(Risk, String)> = None;
        for (re, reason, risk) in &self.patterns {
            if re.is_match(&haystack) {
                let promote = match &worst {
                    None => true,
                    Some((cur, _)) => *risk > *cur,
                };
                if promote {
                    worst = Some((*risk, reason.clone()));
                }
            }
        }
        worst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_catch_rm_rf_root() {
        let g = Guardrails::defaults();
        let hit = g.evaluate("shell_exec", r#"{"cmd":"rm -rf /"}"#);
        assert!(matches!(hit, Some((Risk::High, _))));
    }

    #[test]
    fn defaults_catch_force_push() {
        let g = Guardrails::defaults();
        let hit = g.evaluate("shell_exec", r#"{"cmd":"git push origin main --force"}"#);
        assert!(matches!(hit, Some((Risk::High, _))));
    }

    #[test]
    fn defaults_catch_leaked_secret() {
        let g = Guardrails::defaults();
        let hit = g.evaluate(
            "write_file",
            r#"{"path":"notes.md","contents":"key=sk-ABCDEFGHIJKLMNOPQRSTUVWX"}"#,
        );
        assert!(matches!(hit, Some((Risk::High, _))));
    }

    #[test]
    fn defaults_catch_metadata_curl_as_medium() {
        let g = Guardrails::defaults();
        let hit = g.evaluate("shell_exec", r#"{"cmd":"curl http://169.254.169.254/"}"#);
        assert_eq!(hit.map(|(r, _)| r), Some(Risk::Medium));
    }

    #[test]
    fn worst_wins_when_multiple_rules_match() {
        let g = Guardrails::defaults();
        // Hits both the metadata-curl (Medium) and the secret leak (High).
        let hit = g.evaluate(
            "shell_exec",
            r#"{"cmd":"curl http://169.254.169.254 -H 'AWS_SECRET=x'"}"#,
        );
        assert_eq!(hit.map(|(r, _)| r), Some(Risk::High));
    }

    #[test]
    fn no_match_returns_none() {
        let g = Guardrails::defaults();
        assert!(g.evaluate("read_file", r#"{"path":"README.md"}"#).is_none());
    }

    #[test]
    fn load_falls_back_to_defaults_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let g = Guardrails::load(tmp.path());
        // Defaults are non-empty.
        assert!(!g.patterns.is_empty());
        assert!(g
            .evaluate("shell_exec", r#"{"cmd":"rm -rf /"}"#)
            .is_some());
    }

    #[test]
    fn load_reads_user_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let cortex_dir = tmp.path().join(".cortex");
        std::fs::create_dir_all(&cortex_dir).unwrap();
        let danger = cortex_dir.join("danger.toml");
        std::fs::write(
            &danger,
            r#"
[[rule]]
pattern = "frobnicate"
reason  = "no frobnicating allowed"
risk    = "high"
"#,
        )
        .unwrap();

        let g = Guardrails::load(tmp.path());
        let hit = g.evaluate("shell_exec", r#"{"cmd":"frobnicate"}"#);
        assert!(matches!(hit, Some((Risk::High, _))));
        // Default `rm -rf /` rule should NOT fire (user overrode).
        assert!(g.evaluate("shell_exec", r#"{"cmd":"rm -rf /"}"#).is_none());
    }
}
