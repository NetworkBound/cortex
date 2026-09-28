//! Tauri command surface for **Cortex as an MCP server** (`POST /mcp` on the
//! mobile server — see `crate::mobile_server::mcp`).
//!
//! These only touch `~/.cortex/mcp-server.json` and the key vault; the HTTP
//! endpoint reads both on every request, so flipping a switch here takes
//! effect immediately without restarting the server.

use serde::Serialize;

use crate::mobile_server::mcp::{self, ClientSnippets, McpServerConfig};

/// What the Settings card shows. Never carries the raw token — the UI asks
/// for [`mcp_server_client_snippets`] explicitly when the user wants to copy.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerStatus {
    pub enabled: bool,
    pub allow_destructive: bool,
    /// `http://127.0.0.1:<port>/mcp`.
    pub url: String,
    pub has_token: bool,
    /// Masked token (`012345…cdef`) or empty when none exists.
    pub token_masked: String,
}

fn status(cfg: &McpServerConfig) -> Result<McpServerStatus, String> {
    let token = mcp::current_token()?;
    Ok(McpServerStatus {
        enabled: cfg.enabled,
        allow_destructive: cfg.allow_destructive,
        url: mcp::server_url(),
        has_token: token.is_some(),
        token_masked: token.as_deref().map(mcp::mask_token).unwrap_or_default(),
    })
}

/// Current switches + masked token.
#[tauri::command]
pub async fn mcp_server_get_config() -> Result<McpServerStatus, String> {
    status(&mcp::load_config())
}

/// Flip the server on/off and (optionally) the destructive-tool allowlist.
/// Enabling for the first time mints the bearer token so the endpoint is
/// never reachable without one.
#[tauri::command]
pub async fn mcp_server_set_enabled(
    enabled: bool,
    allow_destructive: Option<bool>,
) -> Result<McpServerStatus, String> {
    let mut cfg = mcp::load_config();
    cfg.enabled = enabled;
    if let Some(d) = allow_destructive {
        cfg.allow_destructive = d;
    }
    if cfg.enabled && mcp::current_token()?.is_none() {
        mcp::rotate_token().await?;
    }
    mcp::save_config(&cfg)?;
    status(&cfg)
}

/// Mint a new token (old one stops working immediately).
#[tauri::command]
pub async fn mcp_server_rotate_token() -> Result<McpServerStatus, String> {
    mcp::rotate_token().await?;
    status(&mcp::load_config())
}

/// Ready-to-paste client configs (Claude Code CLI, Codex `config.toml`,
/// Gemini `settings.json`). Mints a token if none exists yet so the snippets
/// are always complete.
#[tauri::command]
pub async fn mcp_server_client_snippets() -> Result<ClientSnippets, String> {
    let token = match mcp::current_token()? {
        Some(t) => t,
        None => mcp::rotate_token().await?,
    };
    Ok(mcp::client_snippets(&mcp::server_url(), &token))
}
