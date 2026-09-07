//! Tauri command surface for the MCP stdio client host.
//!
//! Thin wrappers over `crate::mcp::{config, client}`. Connection-spawning
//! commands (`mcp_connect`, `mcp_call_tool`) only ever touch a child process
//! the user has explicitly asked for — the host is inert until then.

use crate::mcp::client::{self, McpTool};
use crate::mcp::config::{self, McpServerConfig};
use crate::mcp::rate_limit;
use crate::observability::tracing_store::TracingStore;
use crate::orchestrator::command_policy::{CommandPolicy, PolicyAction};
use serde_json::Value;
use tauri::State;

/// List the persisted MCP server registry. Empty on a fresh install.
#[tauri::command]
pub async fn mcp_list_servers() -> Result<Vec<McpServerConfig>, String> {
    Ok(config::load())
}

/// Insert or update a server in the registry; returns the new registry.
#[tauri::command]
pub async fn mcp_save_server(server: McpServerConfig) -> Result<Vec<McpServerConfig>, String> {
    config::upsert(server)
}

/// Remove a server from the registry by id; returns the new registry.
#[tauri::command]
pub async fn mcp_delete_server(id: String) -> Result<Vec<McpServerConfig>, String> {
    config::remove(&id)
}

/// Spawn the configured server, handshake, and return its advertised tools.
#[tauri::command]
pub async fn mcp_connect(id: String) -> Result<Vec<McpTool>, String> {
    let servers = config::load();
    let cfg = servers
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("no MCP server configured with id '{id}'"))?;
    client::connect(&cfg).await
}

/// Kill the server process for `id` and drop its connection.
#[tauri::command]
pub async fn mcp_disconnect(id: String) -> Result<(), String> {
    client::disconnect(&id).await
}

/// Which of `commands` resolve on PATH right now. Powers the MCP catalog's
/// preflight hint: a catalog server whose runtime (`npx`, `uvx`) is missing
/// would otherwise be added fine and then fail opaquely at its first tool
/// call. Probe-only — nothing is executed.
#[tauri::command]
pub async fn mcp_probe_runtimes(
    commands: Vec<String>,
) -> Result<std::collections::HashMap<String, bool>, String> {
    tokio::task::spawn_blocking(move || {
        commands
            .into_iter()
            .map(|c| {
                let found = which::which(&c).is_ok();
                (c, found)
            })
            .collect()
    })
    .await
    .map_err(|e| format!("probe task failed: {e}"))
}

/// Write one per-tool-call audit row. Best-effort — a logging failure must
/// never break (or fail) the user's tool call — and the detail JSON passes
/// through the redact choke-point so a secret embedded in tool args can't
/// leak into the audit feed.
fn record_mcp_audit(store: &TracingStore, action: &str, mut detail: Value) {
    crate::redact::redact_json_value(&mut detail);
    if let Err(e) = store.record_audit(None, None, action, Some(&detail.to_string())) {
        tracing::warn!("failed to record MCP tool audit: {e}");
    }
}

/// The single gated path to `client::call_tool`, shared by the manual
/// `mcp_call_tool` command and the chat dispatcher (issue 008 —
/// `commands::chat::dispatch_mcp_chat_tool`) so a model-initiated call can
/// never reach a server through a side door.
///
/// Gated per issue 009 MVP: the tool must not be disabled for the server, and
/// the server's trust level must allow the call (`ask` needs
/// `approved == true`, set only after an explicit per-call user confirmation;
/// `untrusted` always refuses). Gated per issue 009 full scope, layered as a
/// narrow-only floor UNDER the above (never a replacement — the trust/disable
/// gates above are unaffected and their tests stay green):
///   * `safe_mode_policy` — the effective Safe Mode command policy, or `None`
///     when Safe Mode is off (callers load this the same "cheap per-call
///     check" way `commands::safe_mode::is_enabled` is documented to work; it
///     is NOT loaded inside this function so the gate stays pure/testable
///     without touching the real `~/.cortex` files). A `Deny` rule matching
///     this server/tool (see
///     [`CommandPolicy::evaluate_mcp_tool_call`]) blocks the
///     call before it ever reaches the server process — the SAME engine Safe
///     Mode already uses for shell commands, deliberately the only extra
///     policy consulted here (no second, MCP-specific rule format). `Ask`/
///     `Allow` leave the trust-gate flow above untouched.
///   * A per-tool call-rate ceiling (`mcp::rate_limit`, default unlimited)
///     blocks a call that would exceed it.
/// Every attempt — allowed, denied, rate-limited, or failed — lands in the
/// audit log via [`record_mcp_audit`] when a store is available (`None` only
/// in unit tests; both real callers pass one).
pub async fn call_tool_gated(
    cfg: &McpServerConfig,
    tool: &str,
    args: Option<Value>,
    approved: bool,
    store: Option<&TracingStore>,
    safe_mode_policy: Option<&CommandPolicy>,
) -> Result<String, String> {
    let detail = serde_json::json!({
        "serverId": cfg.id,
        "server": cfg.name,
        "tool": tool,
        "trust": cfg.trust.as_str(),
        "args": args,
    });

    if !config::tool_enabled(cfg, tool) {
        let reason = format!("tool '{tool}' is disabled for server '{}'", cfg.name);
        if let Some(store) = store {
            record_mcp_audit(store, "mcp_tool_denied", detail);
        }
        return Err(reason);
    }
    if let Err(reason) = config::trust_allows(cfg.trust, approved) {
        if let Some(store) = store {
            record_mcp_audit(store, "mcp_tool_denied", detail);
        }
        return Err(reason);
    }

    // Issue 009 full scope (b): when Safe Mode is on, route the per-server/
    // per-tool decision through Safe Mode's SINGLE policy file rather than a
    // second engine. Narrow-only: only a `Deny` blocks; `Ask`/`Allow` leave
    // the trust-gate flow above untouched, exactly like the shell
    // command-policy gate in `chat.rs::maybe_block_by_command_policy`.
    if let Some(policy) = safe_mode_policy {
        let decision = policy.evaluate_mcp_tool_call(&cfg.id, tool);
        if decision.action == PolicyAction::Deny {
            let rule = decision.matched.as_deref().unwrap_or("<none>");
            let reason = decision.reason.as_deref().unwrap_or("denied by policy");
            let reason = format!(
                "safe mode: MCP tool '{tool}' on server '{}' blocked by command policy rule '{rule}' ({}) — {reason}",
                cfg.name, decision.source
            );
            if let Some(store) = store {
                record_mcp_audit(store, "mcp_tool_denied", detail);
            }
            return Err(reason);
        }
    }

    // Issue 009 full scope (a): per-tool call-rate ceiling, default
    // unlimited. Checked last (after every gate that can reject for free) so
    // a call that was always going to be denied doesn't consume a slot in
    // the window.
    if let Some(max) = cfg.rate_limits.get(tool).copied() {
        if let Err(reason) = rate_limit::check_and_record(&cfg.id, tool, Some(max)) {
            if let Some(store) = store {
                record_mcp_audit(store, "mcp_tool_rate_limited", detail);
            }
            return Err(reason);
        }
    }

    let result = client::call_tool(&cfg.id, tool, args.unwrap_or(Value::Null)).await;
    let action = if result.is_ok() {
        "mcp_tool_call_ok"
    } else {
        "mcp_tool_call_err"
    };
    if let Some(store) = store {
        record_mcp_audit(store, action, detail);
    }
    result
}

/// The effective Safe Mode command policy for an MCP call, or `None` when
/// Safe Mode is off — the "cheap per-call check" every other Safe Mode gate
/// in this codebase does (see `commands::safe_mode::is_enabled`,
/// `McpChatGateCtx::resolve`), so a toggle takes effect on the very next call
/// with no restart. Global-only: the MCP registry is a global file, not
/// scoped to a project, so there is no project-policy layer to merge in here.
fn safe_mode_policy_for_mcp() -> Option<CommandPolicy> {
    if crate::commands::safe_mode::is_enabled() {
        Some(crate::orchestrator::command_policy::load_effective(None))
    } else {
        None
    }
}

/// Call a tool on a connected server. `args` defaults to an empty object.
/// Thin lookup wrapper over [`call_tool_gated`] — see there for the gate
/// semantics (per-tool disable, trust level, rate limit, Safe Mode policy,
/// audit trail).
#[tauri::command]
pub async fn mcp_call_tool(
    id: String,
    tool: String,
    args: Option<Value>,
    approved: Option<bool>,
    store: State<'_, TracingStore>,
) -> Result<String, String> {
    let servers = config::load();
    let cfg = servers
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("no MCP server configured with id '{id}'"))?;
    let safe_mode_policy = safe_mode_policy_for_mcp();
    call_tool_gated(
        &cfg,
        &tool,
        args,
        approved.unwrap_or(false),
        Some(&store),
        safe_mode_policy.as_ref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::client::mock_server_tests::{mock_cfg, node_available};
    use crate::mcp::config::McpTrustLevel;
    use crate::orchestrator::command_policy::CommandPolicy;

    fn trusted_cfg(id: &str) -> McpServerConfig {
        let mut cfg = mock_cfg(id);
        cfg.trust = McpTrustLevel::Trusted;
        cfg
    }

    /// Issue 009 full scope (a): a per-tool rate limit is enforced inside the
    /// shared `call_tool_gated` choke-point, independent of trust/disable —
    /// the Nth+1 call within the window is refused with a clear error, and
    /// the refusal (not just the allowed calls) lands in the audit log as its
    /// own distinct action.
    #[tokio::test]
    async fn rate_limit_enforced_with_audit_row_on_exceeded() {
        if !node_available() {
            return;
        }
        let mut cfg = trusted_cfg("test-mcp-rate-limit");
        cfg.rate_limits.insert("echo".to_string(), 1);
        client::connect(&cfg).await.expect("mock connects");
        let store = TracingStore::in_memory();

        // First call is within the limit.
        let ok = call_tool_gated(&cfg, "echo", Some(serde_json::json!({})), true, Some(&store), None)
            .await;
        assert!(ok.is_ok(), "first call must be allowed: {ok:?}");

        // Second call in the same window is over the limit.
        let err = call_tool_gated(&cfg, "echo", Some(serde_json::json!({})), true, Some(&store), None)
            .await
            .expect_err("second call must be rate-limited");
        assert!(err.contains("rate limit"), "{err}");

        // A different tool on the same server has its own, still-unlimited
        // window — the limit is per-tool, not per-server.
        let ok2 = call_tool_gated(&cfg, "now", None, true, Some(&store), None).await;
        assert!(ok2.is_ok(), "an unrelated tool must be unaffected: {ok2:?}");

        client::disconnect(&cfg.id).await.unwrap();

        let rows = store.recent_audit(10).unwrap();
        assert!(
            rows.iter().any(|r| r.action == "mcp_tool_rate_limited"),
            "exceeding the limit must write its own audit row: {rows:?}"
        );
    }

    /// Issue 009 full scope (b): when Safe Mode is on, a `Deny` rule in the
    /// SAME command-policy engine used for shell commands blocks an MCP call
    /// even for an otherwise-Trusted server — one policy engine governs both.
    /// `Ask`/`Allow` (including "no policy loaded", i.e. Safe Mode off) leave
    /// the pre-existing trust-gate behavior completely unchanged.
    #[tokio::test]
    async fn safe_mode_command_policy_governs_mcp_calls() {
        if !node_available() {
            return;
        }
        let cfg = trusted_cfg("test-mcp-safe-mode-governed");
        client::connect(&cfg).await.expect("mock connects");
        let store = TracingStore::in_memory();

        // Safe Mode off (`None`): a Trusted server's tool call succeeds,
        // completely unaffected by whatever a policy might otherwise say.
        let ok = call_tool_gated(&cfg, "echo", Some(serde_json::json!({})), true, Some(&store), None)
            .await;
        assert!(ok.is_ok(), "no policy loaded => unaffected: {ok:?}");

        // Safe Mode on, with a Deny rule targeting this exact server: blocks
        // the call even though the server is Trusted.
        let deny_raw = format!(
            "[[rule]]\npattern = \"mcp {} *\"\naction = \"deny\"\nreason = \"quarantined by safe mode\"\n",
            cfg.id
        );
        let deny_policy = CommandPolicy::from_files(Some(&deny_raw), None);
        let err = call_tool_gated(
            &cfg,
            "echo",
            Some(serde_json::json!({})),
            true,
            Some(&store),
            Some(&deny_policy),
        )
        .await
        .expect_err("safe mode deny rule must block a Trusted server's call");
        assert!(err.contains("safe mode"), "{err}");
        assert!(err.contains("quarantined by safe mode"), "{err}");

        // An Allow-only policy (no matching Deny) leaves the Trusted server's
        // call unaffected.
        let allow_policy = CommandPolicy::from_files(Some(""), None);
        let ok2 = call_tool_gated(
            &cfg,
            "echo",
            Some(serde_json::json!({})),
            true,
            Some(&store),
            Some(&allow_policy),
        )
        .await;
        assert!(ok2.is_ok(), "no Deny rule => unaffected: {ok2:?}");

        client::disconnect(&cfg.id).await.unwrap();

        let rows = store.recent_audit(10).unwrap();
        assert!(
            rows.iter()
                .any(|r| r.action == "mcp_tool_denied" && r
                    .detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains(&cfg.id)),
            "the safe-mode denial must be audited: {rows:?}"
        );
    }
}
