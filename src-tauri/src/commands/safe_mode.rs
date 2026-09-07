//! Safe Mode (issue 004) — the one-switch lockdown that composes the
//! existing gates (sandbox tier, guardrails, approval policy) with the pure
//! command policy engine in `orchestrator/command_policy.rs`.
//!
//! State lives at `~/.cortex/safe-mode.json`. DEFAULT-OFF: a missing file
//! means Safe Mode is off and every existing behavior is bit-identical. A
//! *malformed* file fails CLOSED (treated as ON) — the likeliest cause is a
//! torn write of an enable, and a security switch must not fail open.
//!
//! When Safe Mode is ON, `chat.rs` (the only enforcement site):
//!   * clamps the effective sandbox tier `DangerFullAccess → WorkspaceWrite`;
//!   * clamps the approval policy `Never → Untrusted` (narrow-only — the
//!     `OnRequest` pin for untrusted projects is stricter and stays);
//!   * enforces the command allow/denylist between the tier gate and the
//!     guardrails (a Deny blocks the tool call before any approval UI);
//!   * suppresses auto-approval of any command the policy doesn't allow;
//!   * writes every tool call to the audit log.
//!
//! Toggling Safe Mode is itself audited.

use crate::observability::tracing_store::TracingStore;
use crate::orchestrator::command_policy::{self, PolicyDecision};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;

/// On-disk schema for `~/.cortex/safe-mode.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SafeMode {
    pub enabled: bool,
    /// Unix epoch millis of the last enable; `None` when disabled.
    #[serde(default)]
    pub enabled_at: Option<i64>,
    /// Full scope (issue 004): additionally clamps the effective sandbox
    /// tier all the way to `ReadOnly` (narrow-only — strictly stricter than
    /// the existing `DangerFullAccess → WorkspaceWrite` clamp), as part of
    /// the built-in "CI-safe" preset. `#[serde(default)]` so every
    /// `safe-mode.json` written before this field existed still parses and
    /// still means exactly what it meant before (`false` — no extra clamp).
    #[serde(default)]
    pub force_read_only: bool,
}

fn cortex_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|h| h.join(".cortex"))
        .ok_or_else(|| "no home directory".to_string())
}

/// `~/.cortex/safe-mode.json`.
pub fn safe_mode_path() -> Result<PathBuf, String> {
    Ok(cortex_dir()?.join("safe-mode.json"))
}

/// Parse a safe-mode file body. Malformed JSON fails CLOSED to enabled —
/// see the module doc. Pure, so the fail-closed property is unit-testable.
pub(crate) fn parse_safe_mode(raw: &str) -> SafeMode {
    match serde_json::from_str::<SafeMode>(raw) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("safe_mode: malformed safe-mode.json ({e}); failing closed (enabled)");
            SafeMode {
                enabled: true,
                enabled_at: None,
                // Fail closed all the way: an unreadable state file means we
                // don't know what was intended, so assume the strictest
                // clamp too, not just "enabled".
                force_read_only: true,
            }
        }
    }
}

/// Load the current Safe Mode state. A missing file (the default install
/// state) is OFF — zero behavior change; a malformed file is ON (fail
/// closed).
pub fn load() -> SafeMode {
    let Ok(path) = safe_mode_path() else {
        return SafeMode::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse_safe_mode(&raw),
        Err(_) => SafeMode::default(),
    }
}

/// Cheap per-call check used by `chat_send` (mirrors `AutoApproveList::load`
/// being re-read every turn so a toggle takes effect without restart).
pub fn is_enabled() -> bool {
    load().enabled
}

// ────────────────────────────── tauri commands ─────────────────────────────

/// Current Safe Mode state, for the StatusBar badge + Settings toggle.
#[tauri::command]
pub async fn safe_mode_status() -> Result<SafeMode, String> {
    Ok(load())
}

/// Toggle Safe Mode. The write is audited (who/when) — the audit trail must
/// show when the lockdown was lifted, not just what happened while it was on.
/// Only touches `enabled`/`enabled_at`; `force_read_only` (the CI-safe
/// preset's extra clamp) is preserved as-is, since this is the plain
/// on/off switch, not the preset.
#[tauri::command]
pub async fn set_safe_mode(
    enabled: bool,
    store: State<'_, TracingStore>,
) -> Result<SafeMode, String> {
    let previous = load();
    let state = SafeMode {
        enabled,
        enabled_at: enabled.then(|| chrono::Utc::now().timestamp_millis()),
        force_read_only: previous.force_read_only,
    };
    write_safe_mode(&state)?;
    let detail = serde_json::json!({ "enabled": enabled }).to_string();
    if let Err(e) = store.record_audit(None, None, "safe-mode.toggled", Some(&detail)) {
        tracing::warn!("safe_mode: audit write failed: {e}");
    }
    Ok(state)
}

fn write_safe_mode(state: &SafeMode) -> Result<(), String> {
    let path = safe_mode_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create ~/.cortex: {e}"))?;
    }
    let body = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

/// The global command-policy body written by [`apply_ci_safe_profile`]:
/// allowlist mode (`default_ask = true` — a command not matched by any rule
/// asks instead of silently passing) layered on top of the same built-in
/// destructive-command heuristics every Safe Mode session already enforces
/// (see `orchestrator::command_policy::BUILTIN_RULES_TOML`). Exposed so the
/// Settings UI can show exactly what "Apply CI-safe preset" is about to
/// write before the user confirms.
pub fn ci_safe_policy_toml() -> String {
    format!(
        "# CI-safe preset (issue 004 full scope): allowlist mode — any command\n\
         # not explicitly matched below asks instead of running silently.\n\
         default_ask = true\n\n{}",
        command_policy::BUILTIN_RULES_TOML
    )
}

/// Preview of the exact body [`apply_ci_safe_profile`] is about to write to
/// the global command-policy file — lets the Settings UI show the user what
/// "max lockdown" means before they confirm overwriting their existing
/// global policy.
#[tauri::command]
pub async fn ci_safe_policy_preview() -> Result<String, String> {
    Ok(ci_safe_policy_toml())
}

/// Built-in "CI-safe" profile (issue 004 full scope): the maximum-lockdown
/// preset selectable from Settings → Safety. Composes three things that are
/// each independently narrow-only, so this can never be *less* strict than
/// plain Safe Mode:
///   * enables Safe Mode with `force_read_only` — the effective sandbox
///     tier is clamped all the way to `ReadOnly` regardless of any
///     per-project `.cortex/sandbox.toml`;
///   * overwrites the GLOBAL command policy with `default_ask = true` (deny-
///     biased allowlist mode) — the built-in heuristics are always enforced
///     on top of it either way, on or off;
///   * "audit-all" falls out for free: every tool call is already audited
///     whenever Safe Mode is on (see `chat.rs`'s gate wiring).
///
/// This DOES overwrite the user's existing global `command-policy.toml` (it
/// is a "reset to the strictest known preset" action, not a merge) — the
/// Settings UI surfaces the exact body via [`ci_safe_policy_toml`] before
/// the user confirms. Project policy files are untouched (they can still
/// only narrow further, unaffected by this call).
#[tauri::command]
pub async fn apply_ci_safe_profile(store: State<'_, TracingStore>) -> Result<SafeMode, String> {
    let policy_toml = ci_safe_policy_toml();
    command_policy::parse_policy_toml(&policy_toml)
        .map_err(|e| format!("internal error: CI-safe preset TOML failed to validate: {e}"))?;
    let path = command_policy::global_policy_path()
        .ok_or_else(|| "no home directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &policy_toml).map_err(|e| format!("write {}: {e}", path.display()))?;

    let state = SafeMode {
        enabled: true,
        enabled_at: Some(chrono::Utc::now().timestamp_millis()),
        force_read_only: true,
    };
    write_safe_mode(&state)?;

    if let Err(e) = store.record_audit(None, None, "safe-mode.ci-safe-applied", None) {
        tracing::warn!("safe_mode: audit write failed: {e}");
    }
    Ok(state)
}

/// The built-in destructive-command heuristics as text, for the Settings
/// "view built-in rules" panel. Read-only reference — always enforced while
/// Safe Mode's command policy runs (see
/// `orchestrator::command_policy::with_builtin_defaults`), independent of
/// whatever the user's own global/project files say.
#[tauri::command]
pub async fn get_builtin_command_rules() -> Result<String, String> {
    Ok(command_policy::builtin_rules_toml().to_string())
}

/// A starter body shown in the editor when no policy file exists yet.
const POLICY_TEMPLATE: &str = r#"# Cortex command policy (Safe Mode).
# Ordered rules; deny > ask > allow. Patterns are matched against each
# shell command segment: `*` matches anything, a *-free pattern matches
# the exact command or a prefix at a token boundary.
#
# default_ask = true          # allowlist mode: unmatched commands ask
#
# [[rule]]
# pattern = "rm *"
# action = "deny"
# reason = "no deletions under Safe Mode"
#
# [[rule]]
# pattern = "git status*"
# action = "allow"
"#;

fn policy_path_for(project_root: Option<&str>) -> Result<PathBuf, String> {
    match project_root {
        Some(root) if !root.trim().is_empty() => Ok(command_policy::project_policy_path(
            std::path::Path::new(root),
        )),
        _ => command_policy::global_policy_path()
            .ok_or_else(|| "no home directory".to_string()),
    }
}

/// Raw TOML of a policy file for the Settings editor. `project_root: None`
/// (or empty) addresses the global file. A missing file returns a commented
/// starter template rather than an error — empty state, not a failure.
#[tauri::command]
pub async fn get_command_policy(project_root: Option<String>) -> Result<String, String> {
    let path = policy_path_for(project_root.as_deref())?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(POLICY_TEMPLATE.to_string()),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Validate-then-write a policy file. Bad TOML → `Err`, file untouched.
#[tauri::command]
pub async fn set_command_policy(
    project_root: Option<String>,
    raw: String,
) -> Result<(), String> {
    command_policy::parse_policy_toml(&raw)?;
    let path = policy_path_for(project_root.as_deref())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, raw).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Dry-run a command against the effective (global + project) policy.
/// Shows the matched rule + source. NEVER executes anything — it only calls
/// the pure evaluator.
#[tauri::command]
pub async fn test_command_policy(
    project_root: Option<String>,
    command: String,
) -> Result<PolicyDecision, String> {
    if command.trim().is_empty() {
        return Err("enter a command to test".to_string());
    }
    let root = project_root
        .as_deref()
        .filter(|r| !r.trim().is_empty())
        .map(std::path::Path::new);
    Ok(command_policy::load_effective(root).evaluate(&command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_off() {
        let s = SafeMode::default();
        assert!(!s.enabled);
        assert!(s.enabled_at.is_none());
    }

    #[test]
    fn parse_round_trips_and_tolerates_missing_fields() {
        let s = parse_safe_mode(r#"{"enabled": true, "enabled_at": 123}"#);
        assert!(s.enabled);
        assert_eq!(s.enabled_at, Some(123));
        let s2 = parse_safe_mode(r#"{"enabled": false}"#);
        assert!(!s2.enabled);
        assert_eq!(s2.enabled_at, None);
    }

    /// A security switch must not fail open: garbage in the state file is
    /// treated as ON (most likely a torn write of an enable).
    #[test]
    fn malformed_state_fails_closed_to_enabled() {
        assert!(parse_safe_mode("").enabled);
        assert!(parse_safe_mode("{not json").enabled);
        assert!(parse_safe_mode(r#"{"enabled": "yes"}"#).enabled);
        // Fail-closed goes all the way: also the strictest tier clamp.
        assert!(parse_safe_mode("{not json").force_read_only);
    }

    #[test]
    fn policy_template_is_valid_toml() {
        // The template we hand the editor must survive its own save path.
        assert!(crate::orchestrator::command_policy::parse_policy_toml(POLICY_TEMPLATE).is_ok());
    }

    // ---- CI-safe profile (issue 004 full scope) ----

    #[test]
    fn force_read_only_defaults_false_and_round_trips() {
        // A safe-mode.json written before this field existed still parses,
        // and still means "no extra clamp" — zero behavior change.
        let s = parse_safe_mode(r#"{"enabled": true}"#);
        assert!(!s.force_read_only);
        let s2 = parse_safe_mode(r#"{"enabled": true, "force_read_only": true}"#);
        assert!(s2.force_read_only);
    }

    #[test]
    fn ci_safe_policy_toml_is_valid_and_deny_biased() {
        let raw = ci_safe_policy_toml();
        let parsed = command_policy::parse_policy_toml(&raw).expect("must validate");
        assert!(parsed.default_ask, "CI-safe preset must be allowlist mode");
        assert!(!parsed.rule.is_empty(), "must carry the built-in heuristics");
    }

    #[test]
    fn ci_safe_effective_policy_denies_and_asks_by_default() {
        // Simulates what `load_effective` + this preset's global file
        // produce together: nothing explicitly allowed ⇒ default_ask asks;
        // the built-in heuristics still deny/ask their specific patterns.
        let raw = ci_safe_policy_toml();
        let p = command_policy::CommandPolicy::from_files(Some(&raw), None)
            .with_builtin_defaults();
        assert_eq!(
            p.evaluate("rm -rf /").action,
            command_policy::PolicyAction::Deny
        );
        assert_eq!(
            p.evaluate("echo hello").action,
            command_policy::PolicyAction::Ask,
            "allowlist mode: unmatched commands must ask, not silently run"
        );
    }
}
