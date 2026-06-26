//! Codex-CLI-style **approval policy** — the orthogonal "when do we pause to
//! ask the user?" axis that sits alongside the sandbox tier ("what is allowed
//! at all?").
//!
//! The sandbox tier (`sandbox.rs`) is a hard deny-gate and the guardrails
//! (`guardrails.rs`) block high-risk calls outright. *Within* what those two
//! allow, a tool call that the agent surfaces as an `ApprovalRequest` would
//! normally be forwarded to the UI for the user to approve. The approval policy
//! decides whether such a request is instead auto-approved silently.
//!
//! Variants mirror Codex's `AskForApproval`:
//!   * `Untrusted`  — ask for everything *except* provably read-only inspection
//!                    (read/search tools + a provably read-only shell command).
//!                    This reuses the exact same classification the `ReadOnly`
//!                    sandbox tier applies, so the two stay consistent.
//!   * `OnRequest`  — DEFAULT. Preserve historical behavior: forward every
//!                    approval request to the user (the model/user drives it).
//!   * `Never`      — never pause; auto-approve anything that already passed the
//!                    sandbox tier + guardrails ("full-auto within the sandbox").
//!
//! Persisted at `<project_root>/.cortex/approval-policy.toml`:
//! ```toml
//! policy = "on-request"
//! ```
//!
//! The runtime wiring lives in `commands/chat.rs`: an auto-approve decision here
//! only *adds* permission to skip the prompt — it never widens what the sandbox
//! tier or guardrails already forbid (those gates run first). For an *untrusted*
//! project `chat.rs` pins the policy to `OnRequest` regardless of any on-disk
//! file, mirroring how it pins the sandbox tier to `ReadOnly`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::sandbox::{tier_allows, SandboxTier};

/// Three Codex-style approval policies, ordered from most cautious to most
/// permissive.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    Untrusted,
    OnRequest,
    Never,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        // Default preserves the pre-existing behavior: every approval request is
        // forwarded to the user.
        Self::OnRequest
    }
}

/// What the policy decides for a single approval request that has *already*
/// passed the sandbox tier and guardrails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
    /// Silently approve — skip the user prompt.
    AutoApprove,
    /// Forward the request to the user (historical behavior).
    Ask,
}

impl ApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalPolicy::Untrusted => "untrusted",
            ApprovalPolicy::OnRequest => "on-request",
            ApprovalPolicy::Never => "never",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "untrusted" => Some(ApprovalPolicy::Untrusted),
            "on-request" | "onrequest" | "on_request" => Some(ApprovalPolicy::OnRequest),
            "never" => Some(ApprovalPolicy::Never),
            _ => None,
        }
    }

    /// Decide whether a tool call should be auto-approved (silently) rather than
    /// surfaced to the user. The caller has already ensured the call passed the
    /// sandbox tier and guardrails, so a policy `AutoApprove` cannot widen those.
    pub fn decision(
        self,
        tool_name: &str,
        payload_json: &str,
        project_root: Option<&Path>,
    ) -> PolicyOutcome {
        match self {
            ApprovalPolicy::OnRequest => PolicyOutcome::Ask,
            ApprovalPolicy::Never => PolicyOutcome::AutoApprove,
            ApprovalPolicy::Untrusted => {
                // Auto-approve exactly what the ReadOnly tier would permit:
                // read/search tools, or a shell tool carrying a provably
                // read-only command. Everything else is asked. Reusing
                // `tier_allows` keeps the two classifiers from drifting apart.
                if tier_allows(SandboxTier::ReadOnly, tool_name, payload_json, project_root).is_ok()
                {
                    PolicyOutcome::AutoApprove
                } else {
                    PolicyOutcome::Ask
                }
            }
        }
    }

    /// Convenience boolean form of [`decision`](Self::decision).
    pub fn auto_approves(
        self,
        tool_name: &str,
        payload_json: &str,
        project_root: Option<&Path>,
    ) -> bool {
        matches!(
            self.decision(tool_name, payload_json, project_root),
            PolicyOutcome::AutoApprove
        )
    }
}

/// On-disk schema for `.cortex/approval-policy.toml`.
#[derive(Debug, Deserialize, Serialize, Default)]
struct PolicyFile {
    policy: Option<String>,
}

fn policy_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("approval-policy.toml")
}

/// Load the configured approval policy for a project. Missing / malformed files
/// fall back to `ApprovalPolicy::default()` (`OnRequest`), so behavior is
/// unchanged for any project that never opts in.
pub fn load_policy(project_root: &Path) -> ApprovalPolicy {
    let path = policy_path(project_root);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("approval_policy: no policy file ({}): {e}", path.display());
            return ApprovalPolicy::default();
        }
    };
    let parsed: PolicyFile = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("approval_policy: bad toml at {}: {e}", path.display());
            return ApprovalPolicy::default();
        }
    };
    parsed
        .policy
        .as_deref()
        .and_then(ApprovalPolicy::parse)
        .unwrap_or_default()
}

/// Persist the approval policy for a project, creating `.cortex/` if needed.
pub fn write_policy(project_root: &Path, policy: ApprovalPolicy) -> anyhow::Result<()> {
    let path = policy_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = format!("policy = \"{}\"\n", policy.as_str());
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    file.write_all(body.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_stringify_round_trip() {
        for p in [
            ApprovalPolicy::Untrusted,
            ApprovalPolicy::OnRequest,
            ApprovalPolicy::Never,
        ] {
            assert_eq!(ApprovalPolicy::parse(p.as_str()), Some(p));
        }
        // Aliases + case-insensitivity.
        assert_eq!(ApprovalPolicy::parse("ON_REQUEST"), Some(ApprovalPolicy::OnRequest));
        assert_eq!(ApprovalPolicy::parse("onrequest"), Some(ApprovalPolicy::OnRequest));
        assert_eq!(ApprovalPolicy::parse("  Never "), Some(ApprovalPolicy::Never));
        // Unknown → None (caller falls back to default).
        assert_eq!(ApprovalPolicy::parse("yolo"), None);
        assert_eq!(ApprovalPolicy::parse(""), None);
    }

    #[test]
    fn default_is_on_request_and_asks_for_everything() {
        let p = ApprovalPolicy::default();
        assert_eq!(p, ApprovalPolicy::OnRequest);
        // OnRequest never auto-approves — read OR write, it always asks, so the
        // historical "forward to user" behavior is preserved verbatim.
        assert!(!p.auto_approves("read_file", "{}", None));
        assert!(!p.auto_approves("write_file", r#"{"path":"a.txt"}"#, None));
        assert!(!p.auto_approves("shell_exec", r#"{"cmd":"git status"}"#, None));
        assert!(!p.auto_approves("shell_exec", r#"{"cmd":"rm -rf /"}"#, None));
    }

    #[test]
    fn never_auto_approves_everything() {
        let p = ApprovalPolicy::Never;
        // Anything that reaches the policy already passed tier + guardrails, so
        // Never approves it all — including writes and exec.
        assert!(p.auto_approves("read_file", "{}", None));
        assert!(p.auto_approves("write_file", r#"{"path":"a.txt"}"#, None));
        assert!(p.auto_approves("shell_exec", r#"{"cmd":"cargo build"}"#, None));
        assert_eq!(
            p.decision("anything", "{}", None),
            PolicyOutcome::AutoApprove
        );
    }

    #[test]
    fn untrusted_auto_approves_only_read_only() {
        let p = ApprovalPolicy::Untrusted;
        // Read-shaped tools: auto-approved.
        assert!(p.auto_approves("read_file", "{}", None));
        assert!(p.auto_approves("grep_search", r#"{"query":"foo"}"#, None));
        assert!(p.auto_approves("list_files", "{}", None));
        // A shell tool carrying a provably read-only command: auto-approved
        // (same classifier the ReadOnly tier uses).
        assert!(p.auto_approves("shell_exec", r#"{"cmd":"git status"}"#, None));
        assert!(p.auto_approves("run_command", r#"{"command":"ls -la"}"#, None));
        // A write tool: must ask.
        assert!(!p.auto_approves("write_file", r#"{"path":"a.txt"}"#, None));
        assert!(!p.auto_approves("edit_file", r#"{"path":"a.txt"}"#, None));
        // A shell tool carrying a writing command: must ask.
        assert!(!p.auto_approves("shell_exec", r#"{"cmd":"rm -rf build"}"#, None));
        assert!(!p.auto_approves("shell_exec", r#"{"cmd":"git push --force"}"#, None));
        // Fail closed: an exec tool with no extractable command must ask.
        assert!(!p.auto_approves("shell_exec", "{}", None));
    }

    #[test]
    fn load_missing_or_bad_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // No file → default.
        assert_eq!(load_policy(root), ApprovalPolicy::OnRequest);
        // Malformed toml → default (and a warning is logged, not panicked).
        let cortex = root.join(".cortex");
        fs::create_dir_all(&cortex).unwrap();
        fs::write(cortex.join("approval-policy.toml"), "policy = [not valid").unwrap();
        assert_eq!(load_policy(root), ApprovalPolicy::OnRequest);
        // Valid but unknown value → default.
        fs::write(cortex.join("approval-policy.toml"), "policy = \"bananas\"\n").unwrap();
        assert_eq!(load_policy(root), ApprovalPolicy::OnRequest);
    }

    #[test]
    fn write_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for p in [
            ApprovalPolicy::Untrusted,
            ApprovalPolicy::OnRequest,
            ApprovalPolicy::Never,
        ] {
            write_policy(root, p).unwrap();
            assert_eq!(load_policy(root), p);
        }
        // The persisted file is the documented schema.
        let body = fs::read_to_string(root.join(".cortex").join("approval-policy.toml")).unwrap();
        assert_eq!(body, "policy = \"never\"\n");
    }
}
