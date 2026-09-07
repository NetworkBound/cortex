//! Persistent registry of user-configured MCP servers.
//!
//! Stored as pretty JSON at `<cortex_dir>/mcp-servers.json`. A missing file
//! is treated as "no servers" rather than an error so a fresh install starts
//! with an empty list and zero side effects.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// How much a server's tools are trusted to run.
///
/// * `Trusted`   — tool calls run without a per-call confirmation.
/// * `Ask`       — every tool call needs an explicit user approval (the
///                 default: a config written before this field existed
///                 deserializes to `Ask`, never silently to `Trusted`).
/// * `Untrusted` — tool calls are refused outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpTrustLevel {
    Trusted,
    #[default]
    Ask,
    Untrusted,
}

impl McpTrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            McpTrustLevel::Trusted => "trusted",
            McpTrustLevel::Ask => "ask",
            McpTrustLevel::Untrusted => "untrusted",
        }
    }
}

/// One configured MCP server. `command` + `args` describe how to spawn the
/// server process; `enabled` is an advisory flag the UI can use to gate
/// auto-connect behaviour (the host itself never auto-connects).
///
/// `env` carries per-server environment variables layered onto the inherited
/// process environment at spawn time. Catalog entries (e.g. github,
/// brave-search) declare which vars a server needs, but the *values* are
/// always user-supplied — the catalog never ships a token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    /// Extra environment variables for the spawned server process.
    /// A `BTreeMap` keeps the persisted JSON key order stable across saves.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Per-server trust level gating tool calls (see [`McpTrustLevel`]).
    #[serde(default)]
    pub trust: McpTrustLevel,
    /// Tool names the user switched off for this server. A tool listed here
    /// is refused by `mcp_call_tool` regardless of trust level. Default-empty
    /// keeps every tool enabled, matching pre-existing behaviour.
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// Issue 009 full scope: per-tool call-rate ceiling, `tool name -> max
    /// calls per rolling 60s window`. A tool with no entry here is
    /// unlimited — default-empty keeps every existing config (and every
    /// tool not explicitly limited) exactly as fast as before. Enforced in
    /// `commands::mcp::call_tool_gated` via `mcp::rate_limit`.
    #[serde(default)]
    pub rate_limits: BTreeMap<String, u32>,
    /// Issue 008: advertise this server's enabled tools to tool-capable chat
    /// adapters so the model can call them mid-turn. Default-OFF — a config
    /// written before this field existed (and every fresh server) exposes
    /// nothing to the model until the user flips the toggle in the MCP panel.
    /// Exposure never bypasses the gates: every model-initiated call still
    /// runs the trust/sandbox/guardrail pipeline (see `commands::chat`'s MCP
    /// dispatcher).
    #[serde(default)]
    pub expose_in_chat: bool,
}

/// True unless the user explicitly disabled this tool for this server.
pub fn tool_enabled(cfg: &McpServerConfig, tool: &str) -> bool {
    !cfg.disabled_tools.iter().any(|t| t == tool)
}

/// The trust gate for a single tool call. `user_approved` is true only when
/// the call was explicitly confirmed by the user for THIS invocation (the UI
/// confirm dialog); it is never inferred.
pub fn trust_allows(trust: McpTrustLevel, user_approved: bool) -> Result<(), String> {
    match trust {
        McpTrustLevel::Trusted => Ok(()),
        McpTrustLevel::Ask if user_approved => Ok(()),
        McpTrustLevel::Ask => Err(
            "this server's trust level is 'ask': the call needs explicit user approval"
                .to_string(),
        ),
        McpTrustLevel::Untrusted => Err(
            "this server is marked untrusted: tool calls are refused".to_string(),
        ),
    }
}

/// Locate the Cortex config dir (`~/.cortex`). Deliberately a small local
/// copy of the helper in `commands::themes` so this subsystem stays
/// self-contained.
fn cortex_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex"))
}

fn config_path() -> Result<PathBuf, String> {
    Ok(cortex_dir()?.join("mcp-servers.json"))
}

/// Load the registry, never failing. A missing, unreadable, or corrupt file
/// yields an empty vec so read-only callers (the UI listing) always have
/// something to render. NOTE: mutating callers must use [`load_strict`] so a
/// corrupt file is *not* silently treated as empty and then overwritten.
pub fn load() -> Vec<McpServerConfig> {
    load_strict().unwrap_or_default()
}

/// Load the registry, distinguishing "no servers yet" from "the file exists
/// but we couldn't parse it". A missing file yields an empty vec; a present
/// file that is unreadable or contains invalid JSON is an error. Mutators
/// (`upsert`/`remove`) use this so they never overwrite — and thereby destroy
/// — a config file they failed to understand.
fn load_strict() -> Result<Vec<McpServerConfig>, String> {
    let path = config_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        // A genuinely missing file means "no servers configured yet".
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read failed: {e}")),
    };
    serde_json::from_slice(&bytes).map_err(|e| format!("corrupt config: {e}"))
}

/// Persist the full registry as pretty JSON, creating the parent dir.
pub fn save(servers: &[McpServerConfig]) -> Result<(), String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(servers).map_err(|e| format!("serialize failed: {e}"))?;
    // Write to a sibling temp file then atomically rename into place, so a
    // crash mid-write leaves the existing registry intact rather than a
    // half-written, corrupt file. The temp file shares the parent dir so the
    // rename stays on the same filesystem (and is therefore atomic).
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("write failed: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))?;
    Ok(())
}

/// Insert a server or replace an existing one with the same `id`. Returns the
/// updated registry.
pub fn upsert(server: McpServerConfig) -> Result<Vec<McpServerConfig>, String> {
    let mut servers = load_strict()?;
    match servers.iter_mut().find(|s| s.id == server.id) {
        Some(existing) => *existing = server,
        None => servers.push(server),
    }
    save(&servers)?;
    Ok(servers)
}

/// Remove the server with the given `id` (no-op if absent). Returns the
/// updated registry.
pub fn remove(id: &str) -> Result<Vec<McpServerConfig>, String> {
    let mut servers = load_strict()?;
    servers.retain(|s| s.id != id);
    save(&servers)?;
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            name: format!("Server {id}"),
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            enabled: true,
            env: BTreeMap::new(),
            trust: McpTrustLevel::default(),
            disabled_tools: Vec::new(),
            rate_limits: BTreeMap::new(),
            expose_in_chat: false,
        }
    }

    #[test]
    fn round_trips_pretty_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        let servers = vec![sample("a"), sample("b")];
        let json = serde_json::to_vec_pretty(&servers).unwrap();
        fs::write(&path, json).unwrap();

        let bytes = fs::read(&path).unwrap();
        let loaded: Vec<McpServerConfig> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(loaded, servers);
    }

    #[test]
    fn deserializes_camel_case_and_defaults() {
        // `args`/`enabled` omitted should fall back to defaults.
        let raw = r#"[{"id":"x","name":"X","command":"foo"}]"#;
        let loaded: Vec<McpServerConfig> = serde_json::from_str(raw).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].command, "foo");
        assert!(loaded[0].args.is_empty());
        assert!(!loaded[0].enabled);
        // A registry written before trust/per-tool policy existed must come
        // back as the safe default: 'ask', with every tool enabled.
        assert_eq!(loaded[0].trust, McpTrustLevel::Ask);
        assert!(loaded[0].disabled_tools.is_empty());
        // Issue 009 full scope: no rate limit entries → every tool unlimited.
        assert!(loaded[0].rate_limits.is_empty());
        // Issue 008: chat exposure is opt-in — an old config (or a fresh
        // server) must never advertise tools to the model by default.
        assert!(!loaded[0].expose_in_chat);
    }

    /// The rate-limit map round-trips through the camelCase wire shape the
    /// frontend would send (`rateLimits`).
    #[test]
    fn rate_limits_round_trip_camel_case() {
        let raw = r#"[{"id":"x","name":"X","command":"foo","rateLimits":{"search":5}}]"#;
        let loaded: Vec<McpServerConfig> = serde_json::from_str(raw).unwrap();
        assert_eq!(loaded[0].rate_limits.get("search"), Some(&5));
        let back = serde_json::to_string(&loaded).unwrap();
        assert!(back.contains("\"rateLimits\""));
    }

    /// The chat-exposure flag round-trips through the camelCase wire shape
    /// the frontend sends (`exposeInChat`).
    #[test]
    fn expose_in_chat_round_trips_camel_case() {
        let raw = r#"[{"id":"x","name":"X","command":"foo","exposeInChat":true}]"#;
        let loaded: Vec<McpServerConfig> = serde_json::from_str(raw).unwrap();
        assert!(loaded[0].expose_in_chat);
        let back = serde_json::to_string(&loaded).unwrap();
        assert!(back.contains("\"exposeInChat\":true"));
    }

    #[test]
    fn trust_level_round_trips_lowercase() {
        for (level, s) in [
            (McpTrustLevel::Trusted, "\"trusted\""),
            (McpTrustLevel::Ask, "\"ask\""),
            (McpTrustLevel::Untrusted, "\"untrusted\""),
        ] {
            assert_eq!(serde_json::to_string(&level).unwrap(), s);
            let back: McpTrustLevel = serde_json::from_str(s).unwrap();
            assert_eq!(back, level);
        }
    }

    /// Trust-gate matrix: trusted always allows; ask allows only with an
    /// explicit per-call approval; untrusted refuses even an approved call.
    #[test]
    fn trust_gate_enforces_levels() {
        assert!(trust_allows(McpTrustLevel::Trusted, false).is_ok());
        assert!(trust_allows(McpTrustLevel::Trusted, true).is_ok());
        assert!(trust_allows(McpTrustLevel::Ask, true).is_ok());
        assert!(trust_allows(McpTrustLevel::Ask, false).is_err());
        assert!(trust_allows(McpTrustLevel::Untrusted, false).is_err());
        assert!(trust_allows(McpTrustLevel::Untrusted, true).is_err());
    }

    /// Per-tool disable: a tool named in `disabled_tools` is off; everything
    /// else (including on a fresh config) stays enabled.
    #[test]
    fn disabled_tool_is_not_enabled() {
        let mut cfg = sample("a");
        assert!(tool_enabled(&cfg, "echo"));
        cfg.disabled_tools.push("echo".to_string());
        assert!(!tool_enabled(&cfg, "echo"));
        assert!(tool_enabled(&cfg, "now"));
    }

    #[test]
    fn missing_file_parses_to_empty() {
        // Mirrors `load()` behaviour: a read failure → empty vec.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let loaded: Vec<McpServerConfig> = match fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        assert!(loaded.is_empty());
    }
}
