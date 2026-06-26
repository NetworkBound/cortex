//! Persistent registry of user-configured MCP servers.
//!
//! Stored as pretty JSON at `<cortex_dir>/mcp-servers.json`. A missing file
//! is treated as "no servers" rather than an error so a fresh install starts
//! with an empty list and zero side effects.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

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
