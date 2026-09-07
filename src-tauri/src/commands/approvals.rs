//! Tauri commands for managing persistent approval rules.

use std::path::PathBuf;

use crate::orchestrator::{
    load_policy, write_policy, ApprovalPolicy, ApprovalRules, AutoApproveEntry, AutoApproveList,
    Decision,
};

/// Append a new approval rule to `<project_root>/.cortex/approvals.toml`.
/// The `decision` arg accepts `"approve"` / `"allow"` and `"deny"` /
/// `"reject"`. Errors bubble up as user-visible strings.
#[tauri::command]
pub async fn add_approval_rule(
    project_root: String,
    pattern: String,
    decision: String,
) -> Result<(), String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    if pattern.trim().is_empty() {
        return Err("pattern is required".into());
    }
    let parsed_decision = Decision::parse(&decision)
        .ok_or_else(|| format!("invalid decision '{decision}' (use 'approve' or 'deny')"))?;
    let root = PathBuf::from(&project_root);
    ApprovalRules::append_rule(&root, &pattern, parsed_decision).map_err(|e| e.to_string())
}

/// Return the contents of `~/.cortex/auto-approve.json` (or an empty list).
#[tauri::command]
pub async fn list_auto_approve() -> Result<Vec<AutoApproveEntry>, String> {
    Ok(AutoApproveList::list())
}

/// Append a `{tool, pattern, profile?}` entry to the global allowlist.
/// `pattern` is validated as a glob (`globset`). Duplicates are silently
/// de-duped.
#[tauri::command]
pub async fn add_auto_approve(
    tool: String,
    pattern: String,
    profile: Option<String>,
) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("pattern is required".into());
    }
    let entry = AutoApproveEntry {
        tool: tool.trim().to_string(),
        pattern: pattern.trim().to_string(),
        profile: profile.and_then(|p| {
            let t = p.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        }),
    };
    AutoApproveList::add(entry).map_err(|e| e.to_string())
}

/// Remove the entry at `index` (0-based against the current `list_auto_approve`
/// order). Out-of-range indices are a no-op.
#[tauri::command]
pub async fn remove_auto_approve(index: usize) -> Result<(), String> {
    AutoApproveList::remove(index).map_err(|e| e.to_string())
}

/// Read the configured approval policy for `project_root` (Codex-style "when do
/// we pause to ask?" axis). Missing / malformed files yield the default
/// (`on-request`). Returns the kebab-case string.
#[tauri::command]
pub async fn get_approval_policy(project_root: String) -> Result<String, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let root = PathBuf::from(&project_root);
    Ok(load_policy(&root).as_str().to_string())
}

/// Persist the approval policy for `project_root`. Accepted values:
/// `"untrusted"`, `"on-request"`, `"never"` (plus snake/camel variants — see
/// `ApprovalPolicy::parse`).
#[tauri::command]
pub async fn set_approval_policy(project_root: String, policy: String) -> Result<(), String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let parsed = ApprovalPolicy::parse(&policy)
        .ok_or_else(|| format!("invalid policy '{policy}' (use untrusted|on-request|never)"))?;
    let root = PathBuf::from(&project_root);
    write_policy(&root, parsed).map_err(|e| e.to_string())
}
