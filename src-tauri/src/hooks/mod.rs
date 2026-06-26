//! Cortex hook event system.
//!
//! Mirrors the Claude Code hook spec: an on-disk JSON config maps event
//! names (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`,
//! `SessionStart`, `PermissionRequest`, `Notification`) to an ordered list
//! of external commands. Each hook reads a JSON payload from stdin, may
//! write JSON to stdout, and exits with status 2 to block.
//!
//! The runtime side (`runner.rs`) spawns those commands with a hard
//! timeout and clipped output so a hung or runaway hook can never deadlock
//! the chat loop.
//!
//! Config file: `<project_root>/.cortex/hooks/hooks.json`. Missing or
//! malformed → empty config (feature simply no-ops). We use JSON (not
//! TOML) to stay byte-compatible with the upstream Claude Code spec so
//! users can copy hook config across tools.

pub mod runner;

pub use runner::{fire_event, run_hook, FireResult, HookResult, MAX_HOOK_OUTPUT_BYTES};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// One configured hook: an external command + args, with an optional
/// per-hook timeout. Defaults to `DEFAULT_TIMEOUT_MS` when `timeout_ms`
/// is omitted in the JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Whole on-disk config. Keyed by event name (case-sensitive — match the
/// Claude Code event identifiers exactly).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub events: HashMap<String, Vec<HookSpec>>,
}

/// Canonical event names. Kept as `&'static str` constants so callers in
/// `chat.rs` never spell one wrong.
pub mod events {
    pub const PRE_TOOL_USE: &str = "PreToolUse";
    pub const POST_TOOL_USE: &str = "PostToolUse";
    pub const USER_PROMPT_SUBMIT: &str = "UserPromptSubmit";
    pub const STOP: &str = "Stop";
    pub const SESSION_START: &str = "SessionStart";
    pub const PERMISSION_REQUEST: &str = "PermissionRequest";
    pub const NOTIFICATION: &str = "Notification";
    /// Fired by the orchestrator when a task finishes (success or failure).
    /// Consumed by the desktop-notification hook (`commands::notify::fire`)
    /// so users can switch windows during long runs and still get pinged.
    pub const TASK_COMPLETE: &str = "task.complete";
}

impl HooksConfig {
    /// Load `<project_root>/.cortex/hooks/hooks.json`. Returns an empty
    /// config when the file is missing or malformed — hooks are an opt-in
    /// feature, so absence is the common case.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".cortex").join("hooks").join("hooks.json");
        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<HooksConfig>(&raw) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::warn!(
                        "hooks: ignoring malformed {}: {e}",
                        path.display()
                    );
                    HooksConfig::default()
                }
            },
            Err(_) => HooksConfig::default(),
        }
    }

    /// Return the hook specs registered for `event_name`, or an empty
    /// slice if none. Cheap; callers may invoke this on every event.
    pub fn for_event(&self, event_name: &str) -> &[HookSpec] {
        self.events
            .get(event_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// True when at least one hook is configured for `event_name`.
    pub fn has(&self, event_name: &str) -> bool {
        !self.for_event(event_name).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_empty_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = HooksConfig::load(tmp.path());
        assert!(cfg.events.is_empty());
        assert!(!cfg.has(events::PRE_TOOL_USE));
        assert!(cfg.for_event(events::PRE_TOOL_USE).is_empty());
    }

    #[test]
    fn load_parses_well_formed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".cortex").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("hooks.json"),
            r#"{
              "events": {
                "PreToolUse": [
                  { "command": "/bin/echo", "args": ["hi"], "timeout_ms": 1000 }
                ],
                "Stop": [
                  { "command": "/bin/true" }
                ]
              }
            }"#,
        )
        .unwrap();

        let cfg = HooksConfig::load(tmp.path());
        assert_eq!(cfg.for_event(events::PRE_TOOL_USE).len(), 1);
        assert_eq!(cfg.for_event(events::STOP).len(), 1);
        assert_eq!(cfg.for_event(events::PRE_TOOL_USE)[0].command, "/bin/echo");
        assert_eq!(cfg.for_event(events::PRE_TOOL_USE)[0].timeout_ms, Some(1000));
        assert!(cfg.for_event(events::STOP)[0].timeout_ms.is_none());
    }

    #[test]
    fn load_falls_back_on_bad_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".cortex").join("hooks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hooks.json"), "{ not json").unwrap();

        let cfg = HooksConfig::load(tmp.path());
        assert!(cfg.events.is_empty());
    }
}
