//! Observability — local SQLite-backed span store for agent runs, homelab
//! health pollers for user's LXC + Host topology, Sentry SDK integration
//! (opt-in, off by default).

pub mod audit;
pub mod crash;
pub mod homelab;
pub mod sentry;
pub mod tracing_store;
pub mod webhooks;
