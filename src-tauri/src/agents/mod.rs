//! Agent adapters — every way Cortex can talk to a model, behind the single
//! [`AgentAdapter`] trait:
//!
//! * `gateway_remote` — the Cortex Gateway (`/v1/runs` + SSE), which fans out
//!   to upstream providers via its own credential pool.
//! * `local_cli` (+ the `*_spec` modules) — locally installed agent CLIs
//!   (`claude`, `codex`, `gemini`, `grok`, `qwen`, `aider`, …) driven as
//!   subprocesses and parsed into [`AgentEvent`]s.
//! * `openai_compat` / `custom_endpoint` / `ollama` / `local_runtime` —
//!   direct OpenAI-compatible HTTP providers and local model runtimes.
//! * `anthropic_direct` / `openai_direct` — first-party APIs for the
//!   `standalone` (no-homelab) build.
//!
//! `registry` holds the live set; `roles` maps agents onto orchestration roles.

pub mod adapter;
pub mod aider_spec;
pub mod claude_cli;
pub mod claude_spec;
pub mod cli_discovery;
pub mod codex_spec;
pub mod custom_endpoint;
pub mod e2e_fake;
pub mod gateway_remote;
pub mod gemini_spec;
pub mod grok_spec;
pub mod local_cli;
pub mod local_runtime;
pub mod mistral_vibe_spec;
pub mod ollama;
pub mod oneshot;
pub mod openai_compat;
pub mod qwen_spec;
pub mod registry;
pub mod roles;

// Direct provider adapters for the standalone (no-homelab) build variant.
// Gated on the `standalone` feature so the default build never compiles them.
#[cfg(feature = "standalone")]
pub mod anthropic_direct;
#[cfg(feature = "standalone")]
pub mod openai_direct;

pub use adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest, ChatTurn,
};
pub use e2e_fake::E2eFakeAgent;
pub use local_runtime::{LocalRuntimeAgent, RuntimeSpec, RUNTIMES};
pub use openai_compat::{OpenAiCompatAgent, ProviderSpec, PROVIDERS};
pub use registry::Registry;

use local_cli::CliSpec;

/// Every local AI-maker CLI Cortex knows how to drive, as data. This is the
/// single source of truth: `lib.rs` registers a `GenericCliAgent` per entry,
/// and the `list_local_cli_providers` command reports detection/login state for
/// each. Adding a new maker's CLI is just a new `*_spec.rs` + one line here.
///
/// Claude leads the list (it's the most battle-tested adapter); the rest follow
/// in rough "major maker" order. DeepSeek has no first-party CLI as of June
/// 2026 — it's served through the Cortex Gateway / direct adapters instead, so
/// it intentionally has no entry here.
pub static ALL_CLI_SPECS: &[&CliSpec] = &[
    &claude_spec::CLAUDE_SPEC,
    &codex_spec::CODEX_SPEC,
    &gemini_spec::GEMINI_SPEC,
    &qwen_spec::QWEN_SPEC,
    &grok_spec::GROK_SPEC,
    &aider_spec::AIDER_SPEC,
    &mistral_vibe_spec::MISTRAL_VIBE_SPEC,
];

#[cfg(feature = "standalone")]
pub use anthropic_direct::AnthropicDirectAgent;
#[cfg(feature = "standalone")]
pub use openai_direct::OpenAIDirectAgent;
