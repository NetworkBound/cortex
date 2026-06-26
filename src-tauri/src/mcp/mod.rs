//! Model Context Protocol (MCP) stdio client host.
//!
//! Lets Cortex connect to local MCP servers — the same protocol Claude
//! Desktop / Cursor / Continue use — by spawning a server process and
//! speaking newline-delimited JSON-RPC 2.0 over its stdin/stdout.
//!
//! This subsystem is **inert by default**: nothing spawns at boot. A child
//! process is only launched when the user explicitly calls `mcp_connect`.
//! The server registry (`mcp-servers.json`) starts empty on a fresh install.

pub mod client;
pub mod config;
