//! Per-tool call-rate limiter for MCP tool calls (issue 009 full scope).
//!
//! A sliding 60-second window keyed by `(server_id, tool)`, in-process only
//! (no persistence — restarting the app resets every counter, same as the
//! MCP connection registry in `mcp::client`). Default is **unlimited**: a
//! server whose `rate_limits` map has no entry for a tool (every existing
//! config, and every fresh server, since the field defaults empty) never
//! touches this module's state at all — zero behavior change until the user
//! sets an explicit ceiling in the MCP panel.
//!
//! Enforced from the single audited choke-point in
//! `commands::mcp::call_tool_gated`, so both the manual panel button and the
//! model-initiated chat dispatcher (issue 008) share one limiter — a call
//! that fails here never reaches `mcp::client::call_tool`.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Window width: "per minute" per the issue.
const WINDOW: Duration = Duration::from_secs(60);

/// `(server_id, tool) -> timestamps of calls counted in the current window`.
/// Pruned lazily on each check (no background timer), so an idle tool costs
/// nothing and the map never grows unbounded for a tool that stops being
/// called.
static CALL_TIMES: Lazy<Mutex<HashMap<(String, String), Vec<Instant>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Check whether one more call to `tool` on `server_id` is allowed under
/// `max_per_minute`, and if so, record it as having happened now.
///
/// `None` (no configured limit — the default) always succeeds and never
/// touches the window state. `Some(0)` denies every call outright (the
/// per-tool disable toggle in `mcp::config` is the more direct way to
/// achieve that, but a `0` limit is not special-cased away). Otherwise the
/// call is allowed iff fewer than `max` calls to this exact `(server_id,
/// tool)` pair landed in the trailing 60 seconds.
pub fn check_and_record(server_id: &str, tool: &str, max_per_minute: Option<u32>) -> Result<(), String> {
    let Some(max) = max_per_minute else {
        return Ok(());
    };
    let now = Instant::now();
    let key = (server_id.to_string(), tool.to_string());
    let mut guard = CALL_TIMES.lock().unwrap_or_else(|e| e.into_inner());
    let times = guard.entry(key).or_default();
    times.retain(|&t| now.duration_since(t) < WINDOW);
    if times.len() as u32 >= max {
        return Err(format!(
            "rate limit exceeded for tool '{tool}': max {max} call(s)/minute ({} already in the last minute)",
            times.len()
        ));
    }
    times.push(now);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No configured limit ⇒ unlimited, and the fast path never allocates a
    /// window entry (nothing to prune, nothing to grow).
    #[test]
    fn unlimited_by_default() {
        for _ in 0..500 {
            assert!(check_and_record("srv-unlimited", "any-tool", None).is_ok());
        }
    }

    /// A limit of N allows exactly N calls in the window, then denies with a
    /// clear, actionable error.
    #[test]
    fn limit_enforced_within_window() {
        let tool = "rate-limit-test-tool-a";
        assert!(check_and_record("srv-a", tool, Some(2)).is_ok());
        assert!(check_and_record("srv-a", tool, Some(2)).is_ok());
        let err = check_and_record("srv-a", tool, Some(2)).unwrap_err();
        assert!(err.contains("rate limit"), "{err}");
        assert!(err.contains(tool), "{err}");
    }

    /// The window is keyed per (server, tool) — a limit on one tool doesn't
    /// bleed into another tool or another server using the same tool name.
    #[test]
    fn limit_is_scoped_per_server_and_tool() {
        assert!(check_and_record("srv-scope-a", "shared-tool", Some(1)).is_ok());
        assert!(check_and_record("srv-scope-b", "shared-tool", Some(1)).is_ok());
        assert!(
            check_and_record("srv-scope-a", "shared-tool", Some(1)).is_err(),
            "srv-scope-a already used its one call"
        );
        assert!(
            check_and_record("srv-scope-a", "other-tool", Some(1)).is_ok(),
            "a different tool on the same server has its own window"
        );
    }

    /// A `0` limit is a valid (if unusual) configuration: it denies every
    /// call, never allowing the count to reach zero-minus-one.
    #[test]
    fn zero_limit_denies_every_call() {
        assert!(check_and_record("srv-zero", "zero-tool", Some(0)).is_err());
        assert!(check_and_record("srv-zero", "zero-tool", Some(0)).is_err());
    }
}
