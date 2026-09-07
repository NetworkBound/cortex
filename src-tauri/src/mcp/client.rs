//! Newline-delimited JSON-RPC 2.0 stdio client for MCP servers.
//!
//! Transport is the child process's stdin (we write) and stdout (we read).
//! Each message is exactly one UTF-8 JSON line — **not** LSP
//! `Content-Length` framing. We perform the standard MCP handshake
//! (`initialize` → `notifications/initialized` → `tools/list`) and keep the
//! process alive in a global registry so `call_tool` can reuse it.
//!
//! Every read/write is wrapped in a timeout so a hung server can never wedge
//! the app, and any failure surfaces as a clear `Err(String)` — we never
//! panic and never block forever.

use super::config::McpServerConfig;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

/// Wall-clock budget for any single request/response exchange.
const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// MCP protocol version we advertise during `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// A tool advertised by an MCP server via `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for the tool's arguments, passed through verbatim.
    #[serde(default)]
    pub input_schema: Option<Value>,
}

/// A live connection to one spawned MCP server. The whole struct is held
/// behind a per-connection async mutex (see `CONNECTIONS`) so concurrent
/// `call_tool` invocations serialise their request/response exchanges and
/// can't interleave lines on the shared pipes.
struct Connection {
    child: Child,
    stdin: ChildStdin,
    reader: Lines<BufReader<ChildStdout>>,
    /// Monotonic JSON-RPC request id, per connection.
    next_id: u64,
}

impl Connection {
    /// Write one JSON value as a single newline-terminated line to stdin.
    async fn write_message(&mut self, msg: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(msg).map_err(|e| format!("encode failed: {e}"))?;
        line.push('\n');
        timeout(IO_TIMEOUT, self.stdin.write_all(line.as_bytes()))
            .await
            .map_err(|_| "timed out writing to server".to_string())?
            .map_err(|e| format!("write failed: {e}"))?;
        timeout(IO_TIMEOUT, self.stdin.flush())
            .await
            .map_err(|_| "timed out flushing to server".to_string())?
            .map_err(|e| format!("flush failed: {e}"))?;
        Ok(())
    }

    /// Read lines until we find a JSON-RPC response whose `id` matches the
    /// one we sent. Server-initiated notifications (a `method` without an `id`)
    /// are ignored; server-initiated requests (a `method` *with* an `id`) are
    /// answered with a JSON-RPC "method not found" error so the server is not
    /// left waiting, then we keep reading for our own response.
    async fn read_response(&mut self, want_id: u64) -> Result<Value, String> {
        loop {
            let next = timeout(IO_TIMEOUT, self.reader.next_line())
                .await
                .map_err(|_| "timed out reading from server".to_string())?
                .map_err(|e| format!("read failed: {e}"))?;
            let Some(line) = next else {
                return Err("server closed its stdout".to_string());
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                // Not JSON (e.g. a stray log line) — skip rather than fail.
                continue;
            };
            if value.get("method").is_some() {
                // Server-initiated message. A `method` with an `id` is a
                // request that expects a reply; answer it with a standard
                // "method not found" error so the server isn't left hanging.
                // A `method` without an `id` is a notification — ignore it.
                if let Some(id) = value.get("id") {
                    if !id.is_null() {
                        let reply = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": "method not found",
                            },
                        });
                        // Best-effort: don't abort our own request if this
                        // courtesy reply fails to write.
                        let _ = self.write_message(&reply).await;
                    }
                }
                continue;
            }
            // A genuine response carries no `method`; match strictly on a
            // numeric `id` (no string coercion — see `value_id`).
            match value.get("id").and_then(value_id) {
                Some(id) if id == want_id => return Ok(value),
                _ => continue,
            }
        }
    }

    /// Send a request and await its matching response, returning `result` or
    /// translating a JSON-RPC `error` object into an `Err`.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&msg).await?;
        let resp = self.read_response(id).await?;
        if let Some(err) = resp.get("error") {
            return Err(format!("server error on {method}: {err}"));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send a notification (no id, no response expected).
    async fn notify(&mut self, method: &str) -> Result<(), String> {
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        self.write_message(&msg).await
    }
}

/// Extract a JSON-RPC `id` as `u64` for matching against the id we sent.
///
/// We only accept an actual JSON *number*. A JSON *string* id (e.g. `"1"`)
/// is a distinct id under JSON-RPC and must never be coerced to a number, or
/// a server message with a string id could spuriously satisfy a pending
/// numeric-id request.
fn value_id(v: &Value) -> Option<u64> {
    v.as_u64()
}

/// Global registry of live connections, keyed by server id. Each entry is an
/// async mutex so a single connection's I/O is serialised. Empty until the
/// user connects — there is no boot-time initialisation work here.
static CONNECTIONS: Lazy<AsyncMutex<HashMap<String, Arc<AsyncMutex<Connection>>>>> =
    Lazy::new(|| AsyncMutex::new(HashMap::new()));

/// Tools each connected server advertised at handshake time, keyed by server
/// id. A *separate* sync cache (rather than a field read through
/// `CONNECTIONS`) so read-only consumers — chat-tool advertisement, issue 008
/// — never contend with an in-flight tool call's request/response round-trip.
/// Kept in lockstep with `CONNECTIONS`: populated by [`connect`], cleared by
/// [`disconnect`].
static TOOL_CACHE: Lazy<std::sync::Mutex<HashMap<String, Vec<McpTool>>>> =
    Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

/// The tools `id` advertised when it was connected. `None` when the server is
/// not currently connected (never an error — advertisement is best-effort).
pub fn cached_tools(id: &str) -> Option<Vec<McpTool>> {
    TOOL_CACHE
        .lock()
        .ok()
        .and_then(|m| m.get(id).cloned())
}

/// Spawn the server, perform the MCP handshake, fetch its tool list, and
/// store the live connection under `cfg.id`. Reconnecting replaces (and
/// kills) any prior connection for the same id.
pub async fn connect(cfg: &McpServerConfig) -> Result<Vec<McpTool>, String> {
    if cfg.command.trim().is_empty() {
        return Err("server command is empty".to_string());
    }

    let mut child = crate_command(cfg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn '{}': {e}", cfg.command))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;

    let mut conn = Connection {
        child,
        stdin,
        reader: BufReader::new(stdout).lines(),
        next_id: 1,
    };

    // Handshake. On any failure, kill the child so we don't leak a process.
    let tools = match handshake(&mut conn).await {
        Ok(t) => t,
        Err(e) => {
            let _ = conn.child.kill().await;
            return Err(e);
        }
    };

    // Register (replacing + killing any prior connection for this id).
    let mut map = CONNECTIONS.lock().await;
    if let Some(old) = map.remove(&cfg.id) {
        let mut old = old.lock().await;
        let _ = old.child.kill().await;
    }
    map.insert(cfg.id.clone(), Arc::new(AsyncMutex::new(conn)));
    if let Ok(mut cache) = TOOL_CACHE.lock() {
        cache.insert(cfg.id.clone(), tools.clone());
    }

    Ok(tools)
}

/// Run `initialize` → `notifications/initialized` → `tools/list`.
async fn handshake(conn: &mut Connection) -> Result<Vec<McpTool>, String> {
    let init_params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "cortex", "version": env!("CARGO_PKG_VERSION") },
    });
    conn.request("initialize", init_params).await?;
    conn.notify("notifications/initialized").await?;
    let result = conn.request("tools/list", Value::Null).await?;
    parse_tools(&result)
}

/// Extract `result.tools` into `Vec<McpTool>`. A missing `tools` array yields
/// an empty vec.
fn parse_tools(result: &Value) -> Result<Vec<McpTool>, String> {
    let Some(arr) = result.get("tools").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let tool: McpTool = serde_json::from_value(item.clone())
            .map_err(|e| format!("malformed tool entry: {e}"))?;
        out.push(tool);
    }
    Ok(out)
}

/// Build the spawn command. Mirrors `crate::sys::no_window` for the tokio
/// process type so we don't flash a console window on Windows. (We can't
/// reuse that helper directly — it returns `std::process::Command`.)
fn crate_command(cfg: &McpServerConfig) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&cfg.command);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW (0x08000000) — same flag `crate::sys::no_window` uses.
        // (`creation_flags` is inherent on tokio's Command; no trait import.)
        cmd.creation_flags(0x0800_0000);
    }
    cmd.args(&cfg.args);
    // Layer per-server env vars onto the inherited environment. Empty values
    // are skipped so a placeholder the user never filled in (e.g. a token the
    // catalog flagged as required) isn't passed as an empty string — the server
    // then fails loudly on a missing var rather than misbehaving on a blank one.
    for (key, value) in &cfg.env {
        if !value.is_empty() {
            cmd.env(key, value);
        }
    }
    cmd
}

/// Kill the server process for `id` and drop its connection. No-op (Ok) if
/// the id is unknown.
pub async fn disconnect(id: &str) -> Result<(), String> {
    let removed = {
        let mut map = CONNECTIONS.lock().await;
        map.remove(id)
    };
    if let Ok(mut cache) = TOOL_CACHE.lock() {
        cache.remove(id);
    }
    if let Some(conn) = removed {
        let mut conn = conn.lock().await;
        let _ = conn.child.kill().await;
    }
    Ok(())
}

/// Call a tool on a connected server and return its textual result. Content
/// blocks of `type:"text"` are concatenated; other/missing block shapes are
/// stringified so the caller always gets something useful.
pub async fn call_tool(id: &str, tool: &str, args: Value) -> Result<String, String> {
    // Hold the global registry lock only long enough to clone the
    // per-connection handle, then release it so other servers' calls aren't
    // blocked behind this one's request/response round-trip. The per-connection
    // mutex still serialises I/O on this single connection's pipes.
    let conn_mutex = {
        let map = CONNECTIONS.lock().await;
        map.get(id)
            .cloned()
            .ok_or_else(|| format!("not connected to server '{id}'"))?
    };
    let mut conn = conn_mutex.lock().await;

    let params = json!({
        "name": tool,
        "arguments": if args.is_null() { json!({}) } else { args },
    });
    let result = conn.request("tools/call", params).await?;
    Ok(render_content(&result))
}

/// Flatten `result.content` (array of content blocks) into a single string.
fn render_content(result: &Value) -> String {
    let Some(blocks) = result.get("content").and_then(Value::as_array) else {
        // No content array — hand back the whole result as JSON.
        return result.to_string();
    };
    let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
                continue;
            }
        }
        // Non-text or malformed block → stringify it.
        parts.push(block.to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tools_list_response() {
        let result = json!({
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo back input",
                    "inputSchema": { "type": "object" }
                },
                { "name": "now" }
            ]
        });
        let tools = parse_tools(&result).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description.as_deref(), Some("Echo back input"));
        assert!(tools[0].input_schema.is_some());
        assert_eq!(tools[1].name, "now");
        assert!(tools[1].description.is_none());
    }

    #[test]
    fn missing_tools_array_is_empty() {
        let tools = parse_tools(&json!({})).unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn renders_text_content_blocks() {
        let result = json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": "world" }
            ]
        });
        assert_eq!(render_content(&result), "hello\nworld");
    }

    #[test]
    fn stringifies_non_text_content() {
        let result = json!({
            "content": [ { "type": "image", "data": "abc" } ]
        });
        let rendered = render_content(&result);
        assert!(rendered.contains("image"));
    }

    #[test]
    fn matches_numeric_ids_only() {
        // Real JSON numbers match.
        assert_eq!(value_id(&json!(5)), Some(5));
        // A numeric-looking *string* id is a distinct JSON-RPC id and must
        // not be coerced to a number, or it could spuriously satisfy a
        // pending numeric-id request.
        assert_eq!(value_id(&json!("7")), None);
        assert_eq!(value_id(&json!("nope")), None);
    }
}

/// End-to-end tests against a real spawned mock MCP server (issue 008's
/// "mock MCP server" test plan item). The mock is a tiny inline Node script
/// speaking the same newline-delimited JSON-RPC the client does — Node is a
/// safe dev-environment assumption here (the frontend builds with pnpm, and
/// `npx`-launched MCP servers are the catalog's primary runtime). Skips
/// cleanly (with a note) when `node` is not on PATH so `cargo test --lib`
/// never fails on a minimal machine.
#[cfg(test)]
pub(crate) mod mock_server_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Minimal MCP server: handshake + two tools. `echo` reflects its
    /// arguments back as text so calls are observable end-to-end.
    pub(crate) const MOCK_SERVER_JS: &str = r#"
const rl = require('readline').createInterface({ input: process.stdin });
rl.on('line', (l) => {
  l = l.trim();
  if (!l) return;
  let m;
  try { m = JSON.parse(l); } catch { return; }
  const reply = (result) =>
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: m.id, result }) + '\n');
  if (m.method === 'initialize') {
    reply({ protocolVersion: '2024-11-05', capabilities: {}, serverInfo: { name: 'mock', version: '0' } });
  } else if (m.method === 'tools/list') {
    reply({ tools: [
      { name: 'echo', description: 'Echo the arguments back', inputSchema: { type: 'object' } },
      { name: 'now', description: 'Current time' },
    ] });
  } else if (m.method === 'tools/call') {
    const args = (m.params && m.params.arguments) || {};
    reply({ content: [ { type: 'text', text: 'echo:' + JSON.stringify(args) } ] });
  } else if (m.id !== undefined && m.id !== null) {
    reply({});
  }
});
"#;

    /// Config pointing at the inline Node mock, under a unique test id.
    pub(crate) fn mock_cfg(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            name: "Mock MCP".to_string(),
            command: "node".to_string(),
            args: vec!["-e".to_string(), MOCK_SERVER_JS.to_string()],
            enabled: true,
            env: BTreeMap::new(),
            trust: super::super::config::McpTrustLevel::Trusted,
            disabled_tools: Vec::new(),
            rate_limits: BTreeMap::new(),
            expose_in_chat: true,
        }
    }

    pub(crate) fn node_available() -> bool {
        let found = which::which("node").is_ok();
        if !found {
            eprintln!("skipping mock-MCP test: `node` not on PATH");
        }
        found
    }

    #[tokio::test]
    async fn mock_server_connect_call_and_cache_lifecycle() {
        if !node_available() {
            return;
        }
        let cfg = mock_cfg("test-mock-mcp-client");
        let tools = connect(&cfg).await.expect("connect + handshake");
        assert!(tools.iter().any(|t| t.name == "echo"), "tools/list surfaced");

        // Connecting populates the advertisement cache (issue 008).
        let cached = cached_tools(&cfg.id).expect("cache populated on connect");
        assert_eq!(cached.len(), tools.len());

        let out = call_tool(&cfg.id, "echo", json!({ "msg": "hi" }))
            .await
            .expect("tools/call round-trip");
        assert!(out.contains("\"msg\":\"hi\""), "echoed args back: {out}");

        // Disconnecting kills the child AND removes the cached tools, so a
        // disconnected server can no longer be advertised to the model.
        disconnect(&cfg.id).await.unwrap();
        assert!(cached_tools(&cfg.id).is_none(), "cache cleared on disconnect");
    }
}
