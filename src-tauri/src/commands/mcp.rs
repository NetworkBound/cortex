//! Tauri command surface for the MCP stdio client host.
//!
//! Thin wrappers over `crate::mcp::{config, client}`. Connection-spawning
//! commands (`mcp_connect`, `mcp_call_tool`) only ever touch a child process
//! the user has explicitly asked for — the host is inert until then.

use crate::mcp::client::{self, McpTool};
use crate::mcp::config::{self, McpServerConfig};
use serde_json::Value;

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

/// Call a tool on a connected server. `args` defaults to an empty object.
#[tauri::command]
pub async fn mcp_call_tool(
    id: String,
    tool: String,
    args: Option<Value>,
) -> Result<String, String> {
    client::call_tool(&id, &tool, args.unwrap_or(Value::Null)).await
}
