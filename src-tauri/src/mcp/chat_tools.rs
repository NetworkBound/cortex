//! Issue 008 — advertise MCP tools to the model during a chat turn.
//!
//! Pure bookkeeping only: this module decides *which* connected MCP tools may
//! be shown to a tool-capable adapter (`advertised_tools`) and maps a model's
//! qualified tool-call name back to its server + tool (`resolve_call`). It
//! never executes anything — execution lives behind the single gated
//! dispatcher in `commands::chat::dispatch_mcp_chat_tool`, which runs the
//! trust/sandbox/guardrail pipeline before touching `client::call_tool`.
//!
//! Everything here is default-OFF: a server only contributes tools once the
//! user flips its `expose_in_chat` toggle in the MCP panel, and only while it
//! is actually connected (the tool cache is cleared on disconnect).
//!
//! Naming convention: `mcp__<server-id>__<tool>` — the same qualified shape
//! Claude Code uses for MCP tools, so downstream matchers (auto-approve
//! globs, hooks) treat both sources uniformly.

use super::client::{self, McpTool};
use super::config::{self, McpServerConfig, McpTrustLevel};
use crate::observability::tracing_store::TracingStore;
use once_cell::sync::OnceCell;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// Prefix marking a tool-call name as MCP-originated.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// One MCP tool ready to be advertised to a tool-capable adapter.
#[derive(Debug, Clone, Serialize)]
pub struct ChatToolSpec {
    /// Qualified name the model sees and calls: `mcp__<server-id>__<tool>`.
    pub name: String,
    /// Human/model-readable description (falls back to a generated one).
    pub description: String,
    /// JSON Schema for the arguments, passed through verbatim from the
    /// server's `tools/list` (defaulting to a bare object schema).
    pub input_schema: Value,
    /// The configured server this tool belongs to.
    pub server_id: String,
    /// The server-local (unqualified) tool name.
    pub tool: String,
}

/// Server ids are user/catalog-supplied and could theoretically contain the
/// `__` separator; collapse it so a qualified name always splits cleanly.
/// `resolve_in` applies the same mapping when matching, so round-trips hold.
fn sanitized_server_id(id: &str) -> String {
    id.replace("__", "-")
}

/// Build the qualified chat-facing name for a server's tool.
pub fn qualified_name(server_id: &str, tool: &str) -> String {
    format!("{MCP_TOOL_PREFIX}{}__{tool}", sanitized_server_id(server_id))
}

/// Lenient split of a qualified name into `(server_segment, tool)` without
/// consulting the registry — used by display-side gates that only need the
/// bare tool name (the segment before the first `__` is the server). Returns
/// `None` for anything that isn't `mcp__<server>__<tool>` shaped.
pub fn split_qualified(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(MCP_TOOL_PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// The MCP tools currently eligible for advertisement to the model:
/// registry snapshot × live connection cache, filtered by the per-server
/// `expose_in_chat` toggle (default-OFF), trust level, and per-tool disables.
pub fn advertised_tools() -> Vec<ChatToolSpec> {
    let servers = config::load();
    let tools_by_id: HashMap<String, Vec<McpTool>> = servers
        .iter()
        .filter_map(|s| client::cached_tools(&s.id).map(|t| (s.id.clone(), t)))
        .collect();
    advertised_from(&servers, &tools_by_id)
}

/// Pure core of [`advertised_tools`], taking the registry and the connected
/// servers' tool lists explicitly so it is unit-testable without global
/// state. Filters, in order: `expose_in_chat` off → nothing; `untrusted`
/// server → nothing (its calls would be refused anyway, so advertising them
/// would only invite failing calls); not connected → nothing; individually
/// disabled tools → skipped.
pub fn advertised_from(
    servers: &[McpServerConfig],
    tools_by_id: &HashMap<String, Vec<McpTool>>,
) -> Vec<ChatToolSpec> {
    let mut out = Vec::new();
    for cfg in servers {
        if !cfg.expose_in_chat {
            continue;
        }
        if cfg.trust == McpTrustLevel::Untrusted {
            continue;
        }
        let Some(tools) = tools_by_id.get(&cfg.id) else {
            continue;
        };
        for t in tools {
            if !config::tool_enabled(cfg, &t.name) {
                continue;
            }
            out.push(ChatToolSpec {
                name: qualified_name(&cfg.id, &t.name),
                description: t
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("MCP tool '{}' on server '{}'", t.name, cfg.name)),
                input_schema: t
                    .input_schema
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
                server_id: cfg.id.clone(),
                tool: t.name.clone(),
            });
        }
    }
    out
}

/// Resolve a model-issued qualified tool name against the persisted registry.
pub fn resolve_call(qualified: &str) -> Option<(McpServerConfig, String)> {
    resolve_in(&config::load(), qualified)
}

/// Pure core of [`resolve_call`]. Matches by exact `mcp__<server-id>__`
/// prefix (sanitized like `qualified_name`), longest server id first so an id
/// that happens to be a prefix of another id can never shadow it.
pub fn resolve_in(
    servers: &[McpServerConfig],
    qualified: &str,
) -> Option<(McpServerConfig, String)> {
    let rest = qualified.strip_prefix(MCP_TOOL_PREFIX)?;
    let mut candidates: Vec<&McpServerConfig> = servers.iter().collect();
    candidates.sort_by_key(|s| std::cmp::Reverse(s.id.len()));
    for cfg in candidates {
        let prefix = format!("{}__", sanitized_server_id(&cfg.id));
        if let Some(tool) = rest.strip_prefix(&prefix) {
            if !tool.is_empty() {
                return Some((cfg.clone(), tool.to_string()));
            }
        }
    }
    None
}

/// Process-wide audit sink for model-initiated MCP tool calls. The chat
/// dispatcher runs deep inside adapter tasks with no Tauri `State` access, so
/// `lib.rs` hands it the same `TracingStore` the `mcp_call_tool` command
/// audits through — one audit trail for both manual and model-initiated
/// calls. Unset (e.g. in unit tests) simply means "no audit rows", never an
/// error: the Run Replay event stream still captures every call.
static AUDIT_STORE: OnceCell<TracingStore> = OnceCell::new();

/// Install the audit sink. First call wins; later calls are ignored.
pub fn set_audit_store(store: TracingStore) {
    let _ = AUDIT_STORE.set(store);
}

/// The installed audit sink, if any.
pub fn audit_store() -> Option<&'static TracingStore> {
    AUDIT_STORE.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn server(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            name: format!("Server {id}"),
            command: "node".to_string(),
            args: Vec::new(),
            enabled: true,
            env: BTreeMap::new(),
            trust: McpTrustLevel::Trusted,
            disabled_tools: Vec::new(),
            rate_limits: BTreeMap::new(),
            expose_in_chat: true,
        }
    }

    fn tool(name: &str) -> McpTool {
        McpTool {
            name: name.to_string(),
            description: Some(format!("does {name}")),
            input_schema: Some(serde_json::json!({ "type": "object" })),
        }
    }

    fn tools_map(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<McpTool>> {
        entries
            .iter()
            .map(|(id, names)| {
                (
                    id.to_string(),
                    names.iter().map(|n| tool(n)).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    #[test]
    fn qualified_name_round_trips_through_resolve() {
        let servers = vec![server("fs-local"), server("weather")];
        let name = qualified_name("fs-local", "read_file");
        assert_eq!(name, "mcp__fs-local__read_file");
        let (cfg, tool) = resolve_in(&servers, &name).expect("resolves");
        assert_eq!(cfg.id, "fs-local");
        assert_eq!(tool, "read_file");
        // split_qualified (lenient, registry-free) agrees on the bare name.
        let (seg, bare) = split_qualified(&name).unwrap();
        assert_eq!(seg, "fs-local");
        assert_eq!(bare, "read_file");
    }

    #[test]
    fn resolve_prefers_longest_server_id() {
        // "fs" is a prefix of "fs-extra": the longer id must win for its own
        // tools, and "fs" still resolves its own.
        let servers = vec![server("fs"), server("fs-extra")];
        // NB: with `__` as the separator, "mcp__fs-extra__x" can't ambiguously
        // match "fs" (its prefix would need to be "mcp__fs__"), but ids that
        // themselves end like another id + separator are covered by the
        // longest-first ordering.
        let (cfg, tool) = resolve_in(&servers, "mcp__fs-extra__ls").unwrap();
        assert_eq!(cfg.id, "fs-extra");
        assert_eq!(tool, "ls");
        let (cfg, tool) = resolve_in(&servers, "mcp__fs__ls").unwrap();
        assert_eq!(cfg.id, "fs");
        assert_eq!(tool, "ls");
        // Unknown server / non-MCP names don't resolve.
        assert!(resolve_in(&servers, "mcp__nope__ls").is_none());
        assert!(resolve_in(&servers, "write_file").is_none());
    }

    #[test]
    fn server_ids_containing_separator_are_sanitized_consistently() {
        let servers = vec![server("weird__id")];
        let name = qualified_name("weird__id", "go");
        assert_eq!(name, "mcp__weird-id__go");
        let (cfg, tool) = resolve_in(&servers, &name).expect("sanitized match");
        assert_eq!(cfg.id, "weird__id");
        assert_eq!(tool, "go");
    }

    /// Default-OFF invariant: a server never advertises until the user opts
    /// it in, and flipping the toggle off removes its tools again.
    #[test]
    fn advertisement_requires_expose_in_chat() {
        let mut s = server("a");
        let tools = tools_map(&[("a", &["echo"])]);
        s.expose_in_chat = false;
        assert!(advertised_from(&[s.clone()], &tools).is_empty());
        s.expose_in_chat = true;
        let adv = advertised_from(&[s], &tools);
        assert_eq!(adv.len(), 1);
        assert_eq!(adv[0].name, "mcp__a__echo");
    }

    /// Issue test plan: disabling (disconnecting) a server removes its tools
    /// from advertisement — a server absent from the connected-tools map
    /// contributes nothing even with expose on.
    #[test]
    fn disconnected_server_advertises_nothing() {
        let s = server("a");
        let none: HashMap<String, Vec<McpTool>> = HashMap::new();
        assert!(advertised_from(&[s.clone()], &none).is_empty());
        // Once connected (cache entry present) its tools appear.
        let adv = advertised_from(&[s], &tools_map(&[("a", &["echo", "now"])]));
        assert_eq!(adv.len(), 2);
    }

    #[test]
    fn untrusted_server_and_disabled_tools_are_filtered() {
        let mut s = server("a");
        let tools = tools_map(&[("a", &["echo", "now"])]);
        s.trust = McpTrustLevel::Untrusted;
        assert!(
            advertised_from(&[s.clone()], &tools).is_empty(),
            "untrusted servers must not be advertised (their calls are refused)"
        );
        s.trust = McpTrustLevel::Ask;
        s.disabled_tools = vec!["echo".to_string()];
        let adv = advertised_from(&[s], &tools);
        assert_eq!(adv.len(), 1, "per-tool disable filters advertisement");
        assert_eq!(adv[0].tool, "now");
    }
}
