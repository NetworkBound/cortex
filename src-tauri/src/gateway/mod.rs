//! OpenAI-compatible client for the Cortex Gateway backend (URL comes from
//! `CORTEX_GATEWAY_BASE_URL` / `GATEWAY_BASE_URL` (legacy `HERMES_BASE_URL`) /
//! `~/.cortex/infra.json` / Settings — no baked-in address ships in the
//! binary). Phase 2 will implement streaming `/v1/chat/completions` and
//! `/v1/responses`.

pub mod client;
pub mod tool_virtualizer;
