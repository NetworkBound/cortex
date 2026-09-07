//! Tauri command surface for the REST→MCP tool virtualizer.
//!
//! Five commands wrap the registry CRUD + two execution entry points:
//!  - `list_tools` / `get_tool` / `save_tool` / `delete_tool` — registry
//!  - `invoke_tool` — execute + write an audit-log row (the orchestrator
//!     calls this when an agent emits a tool-call event)
//!  - `test_tool`   — execute without logging (the editor's "Test" button
//!     fires this so dry runs don't pollute the audit feed)
//!
//! Registry I/O is wrapped in `spawn_blocking` so the JSON-disk hop doesn't
//! stall the tauri command thread. Invocation already lives on async via
//! reqwest, so it doesn't need the blocking wrapper.

use crate::gateway::tool_virtualizer as tv;
use crate::observability::tracing_store::TracingStore;
use std::collections::HashMap;
use tauri::State;

#[tauri::command]
pub async fn list_tools() -> Result<Vec<tv::ToolDef>, String> {
    tokio::task::spawn_blocking(tv::list_tools_blocking)
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn get_tool(name: String) -> Result<tv::ToolDef, String> {
    tokio::task::spawn_blocking(move || tv::get_tool_blocking(&name))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn save_tool(tool: tv::ToolDef) -> Result<tv::ToolDef, String> {
    tokio::task::spawn_blocking(move || tv::save_tool_blocking(tool))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn delete_tool(name: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || tv::delete_tool_blocking(&name))
        .await
        .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn invoke_tool(
    name: String,
    args: HashMap<String, serde_json::Value>,
    store: State<'_, TracingStore>,
) -> Result<tv::ToolInvocationResult, String> {
    // Load on a blocking worker so registry I/O doesn't share the network
    // thread with the actual HTTP send. Errors here mean the tool name is
    // bogus — surface them as a structured failure rather than a Tauri
    // command error so the UI can render them in the result panel.
    let load_name = name.clone();
    let tool = match tokio::task::spawn_blocking(move || tv::get_tool_blocking(&load_name))
        .await
        .map_err(|e| format!("join: {e}"))?
    {
        Ok(t) => t,
        Err(e) => {
            return Ok(tv::ToolInvocationResult {
                ok: false,
                status: None,
                body: String::new(),
                latency_ms: 0,
                error: Some(e),
                truncated: false,
            })
        }
    };

    let detail = serde_json::json!({
        "tool": name,
        "args": args,
    });
    let result = tv::invoke_tool(tool, args).await;

    // Audit row: best-effort. A logging failure should never break the
    // user's tool call — we just trace it and move on.
    let action = if result.ok {
        "tool_invoke_ok"
    } else {
        "tool_invoke_err"
    };
    if let Err(e) = store.record_audit(None, None, action, Some(&detail.to_string())) {
        tracing::warn!("failed to record tool audit: {e}");
    }

    Ok(result)
}

#[tauri::command]
pub async fn test_tool(
    name: String,
    args: HashMap<String, serde_json::Value>,
) -> Result<tv::ToolInvocationResult, String> {
    // Same path as `invoke_tool` but without the audit write — keeps live
    // editing snappy and stops noisy dry-runs from cluttering history.
    let load_name = name.clone();
    let tool = match tokio::task::spawn_blocking(move || tv::get_tool_blocking(&load_name))
        .await
        .map_err(|e| format!("join: {e}"))?
    {
        Ok(t) => t,
        Err(e) => {
            return Ok(tv::ToolInvocationResult {
                ok: false,
                status: None,
                body: String::new(),
                latency_ms: 0,
                error: Some(e),
                truncated: false,
            })
        }
    };
    Ok(tv::invoke_tool(tool, args).await)
}
