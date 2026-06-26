//! Tauri commands for the global project-trust list.
//!
//! Trust is **global**, not per-project — the list lives at
//! `~/.cortex/trusted-paths.json` and is shared across every Cortex window
//! and session. See `orchestrator::trust` for the storage layer.
//!
//! Untrusted is the default state. The UI is expected to:
//!   * Call `get_trust_status` on project switch.
//!   * Show a banner offering "Trust this project" when status is `false`.
//!   * Call `trust_project` when the user confirms.
//!
//! Untrusted projects have their sandbox tier forced to `read-only` in
//! `commands/chat.rs` and their `.cortex/rules/*.md` skipped in
//! `commands/sessions.rs::gather_project_context`.

use std::path::PathBuf;

use crate::orchestrator::trust;

fn parse_root(project_root: &str) -> Result<PathBuf, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is required".into());
    }
    Ok(PathBuf::from(project_root))
}

/// Returns `true` iff `project_root` is in `~/.cortex/trusted-paths.json`.
/// Missing file / unknown path → `false` (deny-bias default).
#[tauri::command]
pub async fn get_trust_status(project_root: String) -> Result<bool, String> {
    let root = parse_root(&project_root)?;
    Ok(trust::is_trusted(&root))
}

/// Add `project_root` to the global trust list. Idempotent.
#[tauri::command]
pub async fn trust_project(project_root: String) -> Result<(), String> {
    let root = parse_root(&project_root)?;
    trust::trust_path(&root).map_err(|e| e.to_string())
}

/// Remove `project_root` from the global trust list. Idempotent.
#[tauri::command]
pub async fn untrust_project(project_root: String) -> Result<(), String> {
    let root = parse_root(&project_root)?;
    trust::untrust_path(&root).map_err(|e| e.to_string())
}

// ── Cline-style granular trust matrix ────────────────────────────────────
//
// A separate, simpler store from the project-trust list above: just an
// 8-toggle policy + `max_requests_per_task` cap, persisted at
// `~/.cortex/trust-matrix.json`. The UI panel (`TrustMatrix.tsx`) reads it
// at mount and writes back on every change.
//
// Missing / corrupt file → defaults (everything off, cap = 20). This keeps
// the deny-bias consistent with the project-trust default.
//
// ENFORCEMENT (wired 2026-07-01, `matrix_auto_approves` below, consumed by
// `commands::chat`): the matrix is a THIRD source of auto-approval,
// alongside the glob allowlist (`orchestrator::auto_approve`) and the
// project's `ApprovalPolicy`. Same invariant as both of those — an
// auto-approve decision here only ever ADDS permission to skip the prompt;
// it can never widen what the sandbox tier or guardrails already forbid,
// because `chat.rs` re-runs those gates on ANY auto-approve source before
// honoring it (`auto_approve_blocked_by_gates`). Critically, `chat.rs` only
// ever loads/consults the matrix for a TRUSTED project — exactly mirroring
// how `ApprovalPolicy` is forced to `OnRequest` for an untrusted one. An
// unfamiliar repo must not be able to ride a user's global "auto-approve
// all commands" toggle just because it was left on for other projects.
//
// `browser` and `mcp` are intentionally NOT wired: as of this writing,
// `gateway::tool_virtualizer`'s own doc comment states MCP/REST tool calls
// are "exposed to the agent layer in a follow-up" — they don't yet reach
// the `AgentEvent::ToolCall`/`ApprovalRequest` pipeline this function
// classifies against, and there's no established tool-name convention for
// browser/playwright calls in this codebase to pattern-match against
// either. Guessing at an unverified naming convention risked either the
// toggle silently never firing, or matching too broadly by accident — both
// worse than being explicit that these two are UI-only until the
// underlying routing exists. The toggles still save/persist; they're
// simply not consulted yet, same as before this change, and the frontend
// panel says so.

use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustMatrix {
    #[serde(default)]
    pub read_in_workspace: bool,
    #[serde(default)]
    pub read_outside: bool,
    #[serde(default)]
    pub edit_in_workspace: bool,
    #[serde(default)]
    pub edit_outside: bool,
    #[serde(default)]
    pub safe_commands: bool,
    #[serde(default)]
    pub all_commands: bool,
    #[serde(default)]
    pub browser: bool,
    #[serde(default)]
    pub mcp: bool,
    #[serde(default = "default_max_requests")]
    pub max_requests_per_task: u32,
}

fn default_max_requests() -> u32 {
    20
}

impl Default for TrustMatrix {
    fn default() -> Self {
        Self {
            read_in_workspace: false,
            read_outside: false,
            edit_in_workspace: false,
            edit_outside: false,
            safe_commands: false,
            all_commands: false,
            browser: false,
            mcp: false,
            max_requests_per_task: default_max_requests(),
        }
    }
}

fn trust_matrix_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex").join("trust-matrix.json"))
}

impl TrustMatrix {
    /// Synchronous loader — used both by the Tauri command below (wrapped in
    /// `spawn_blocking` for the IPC boundary) and directly by `chat.rs`'s
    /// per-`chat_send` setup (mirrors `AutoApproveList::load()`'s pattern:
    /// a plain sync fn, no Tauri state needed). Missing/corrupt file →
    /// defaults (everything off) — fail-closed, matches the doc comment
    /// above.
    pub fn load() -> Self {
        let Ok(path) = trust_matrix_path() else {
            return Self::default();
        };
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Decide whether `(tool_name, payload)` should be auto-approved under
    /// this matrix. Caller (`chat.rs`) is responsible for: (a) only calling
    /// this for a TRUSTED project, and (b) re-running the sandbox tier +
    /// guardrails on any resulting auto-approve before honoring it — this
    /// function does not duplicate those checks, matching the pattern
    /// `ApprovalPolicy::auto_approves` already establishes.
    ///
    /// Classification reuses `orchestrator::sandbox`'s READ_TOKENS/
    /// WRITE_TOKENS/path_inside so this can never drift from what the
    /// sandbox tier itself considers a read vs. a write — two independent
    /// classifiers for the same question would be a real security smell.
    pub fn auto_approves(
        &self,
        tool_name: &str,
        payload_json: &str,
        project_root: Option<&std::path::Path>,
    ) -> bool {
        use crate::orchestrator::sandbox::{collect_paths, name_matches_any, READ_TOKENS, WRITE_TOKENS};

        let is_exec = name_matches_any(tool_name, &["run_", "exec", "shell", "bash"]);
        if is_exec {
            if self.all_commands {
                return true;
            }
            if self.safe_commands {
                if let Some(cmd) = crate::orchestrator::safe_commands::extract_command(payload_json) {
                    if crate::orchestrator::safe_commands::is_read_only_command(&cmd) {
                        return true;
                    }
                }
            }
            // An exec tool that isn't covered by all_commands/safe_commands
            // falls through to the read/write classification below in case
            // it also matches a read/write token (e.g. `run_grep`) — if not,
            // it simply won't match either branch and this returns false.
        }

        let is_read = name_matches_any(tool_name, READ_TOKENS) && !name_matches_any(tool_name, WRITE_TOKENS);
        let is_write = name_matches_any(tool_name, WRITE_TOKENS);

        let paths = collect_paths(payload_json);
        // No project root, or no path in the payload: can't distinguish
        // "in workspace" from "outside" — fail closed to the OUTSIDE toggle
        // only (the more permissive one requires an explicit affirmative we
        // can't establish here), mirroring `sandbox::tier_allows`'s own
        // fail-closed stance on unconfirmable targets.
        let all_paths_in_root = match project_root {
            Some(root) if !paths.is_empty() => {
                paths.iter().all(|p| crate::orchestrator::sandbox::path_inside(root, p))
            }
            _ => false,
        };

        if is_read {
            return (all_paths_in_root && self.read_in_workspace) || self.read_outside;
        }
        if is_write {
            return (all_paths_in_root && self.edit_in_workspace) || self.edit_outside;
        }
        false
    }
}

#[tauri::command]
pub async fn get_trust_matrix() -> Result<TrustMatrix, String> {
    tokio::task::spawn_blocking(TrustMatrix::load)
        .await
        .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn set_trust_matrix(matrix: TrustMatrix) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = trust_matrix_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        let json =
            serde_json::to_vec_pretty(&matrix).map_err(|e| format!("serialize failed: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod matrix_tests {
    use super::*;

    fn matrix(mutate: impl FnOnce(&mut TrustMatrix)) -> TrustMatrix {
        let mut m = TrustMatrix::default();
        mutate(&mut m);
        m
    }

    #[test]
    fn all_off_by_default_approves_nothing() {
        let m = TrustMatrix::default();
        assert!(!m.auto_approves("read_file", r#"{"path":"src/main.rs"}"#, None));
        assert!(!m.auto_approves("write_file", r#"{"path":"src/main.rs"}"#, None));
        assert!(!m.auto_approves("shell_exec", r#"{"cmd":"git status"}"#, None));
    }

    #[test]
    fn read_in_workspace_only_approves_paths_actually_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let m = matrix(|m| m.read_in_workspace = true);
        let inside = serde_json::json!({ "path": "src/main.rs" }).to_string();
        assert!(m.auto_approves("read_file", &inside, Some(root)));
        let outside = serde_json::json!({ "path": "/etc/passwd" }).to_string();
        assert!(!m.auto_approves("read_file", &outside, Some(root)));
        // No project root at all → can't confirm "in workspace" → denied.
        assert!(!m.auto_approves("read_file", &inside, None));
    }

    #[test]
    fn read_outside_approves_regardless_of_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let m = matrix(|m| m.read_outside = true);
        let outside = serde_json::json!({ "path": "/etc/passwd" }).to_string();
        assert!(m.auto_approves("read_file", &outside, Some(root)));
        assert!(m.auto_approves("read_file", &outside, None));
    }

    #[test]
    fn edit_toggles_mirror_read_toggles_for_write_tools() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let m = matrix(|m| m.edit_in_workspace = true);
        let inside = serde_json::json!({ "path": "src/main.rs" }).to_string();
        assert!(m.auto_approves("write_file", &inside, Some(root)));
        let outside = serde_json::json!({ "path": "/etc/passwd" }).to_string();
        assert!(!m.auto_approves("write_file", &outside, Some(root)));
        // A read tool must never be approved by an edit toggle.
        assert!(!m.auto_approves("read_file", &inside, Some(root)));
    }

    #[test]
    fn safe_commands_only_approves_provably_read_only_shell() {
        let m = matrix(|m| m.safe_commands = true);
        let ro = serde_json::json!({ "cmd": "git status" }).to_string();
        assert!(m.auto_approves("shell_exec", &ro, None));
        let rw = serde_json::json!({ "cmd": "rm -rf build" }).to_string();
        assert!(!m.auto_approves("shell_exec", &rw, None));
    }

    #[test]
    fn all_commands_approves_any_shell_call() {
        let m = matrix(|m| m.all_commands = true);
        let rw = serde_json::json!({ "cmd": "rm -rf build" }).to_string();
        assert!(m.auto_approves("shell_exec", &rw, None));
        assert!(m.auto_approves("run_bash", "{}", None));
    }

    /// `safe_commands`/`all_commands` are exec-only — they must never leak
    /// into approving a plain (non-exec-shaped) write tool.
    #[test]
    fn exec_toggles_do_not_approve_non_exec_write_tools() {
        let m = matrix(|m| {
            m.all_commands = true;
            m.safe_commands = true;
        });
        assert!(!m.auto_approves("write_file", r#"{"path":"a.txt"}"#, None));
    }
}
