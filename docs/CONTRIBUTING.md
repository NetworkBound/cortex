# Contributing to Cortex

> This describes how to extend Cortex — adding agent adapters, commands, and UI.

## Adding a new agent

Three files, ~60 lines total.

### 1. Implement the adapter

Create `src-tauri/src/agents/your_agent.rs`:

```rust
use super::adapter::{AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest};
use tokio::sync::mpsc;

pub struct YourAgent { /* config */ }

impl YourAgent {
    pub fn new(/* config */) -> Self { Self { /* ... */ } }
}

#[async_trait::async_trait]
impl AgentAdapter for YourAgent {
    fn descriptor(&self) -> AgentDescriptor { /* id, label, caps */ }
    async fn health_check(&self) -> bool { /* ping */ }
    async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        // emit AgentEvent::Started, ::Token deltas, ::Done
        Ok(())
    }
}
```

### 2. Register it

In `src-tauri/src/agents/mod.rs`, add `pub mod your_agent;` and re-export.
In `src-tauri/src/lib.rs`, in the registry init, push `Arc::new(YourAgent::new(...))`.

### 3. Add UI metadata

In `src/lib/agents.ts`, add an entry so the sidebar can render it before the backend reports health.

That's it. The orchestrator, observability, audit log, and cost tracking all wire up automatically because they see only `AgentAdapter` + `AgentEvent`.

## Adding a new observability span type

1. In `src-tauri/src/observability/tracing_store.rs`, add the span name to the documented family list in ADR-004.
2. Wherever you start the operation, wrap with `start_span("your.name", attrs)` and call `finish_ok` or `finish_err`.
3. If the panel needs to render it specially, add a case in `src/components/ObservabilityPanel.tsx`.

## Adding a new memory source

1. Implement `MemoryReader` trait (Phase 3) in `src-tauri/src/memory/your_source.rs`.
2. Register it in `memory::registry`.
3. Add a path-or-config UI in settings if user-configurable.

## Code style

- Rust: `cargo fmt`, `cargo clippy -- -D warnings`. No `unwrap()` in handlers — use `?` or `anyhow`.
- TS: `prettier`, `eslint` (`pnpm lint`). Function components only, hooks > classes.
- Commit messages: conventional commits (`feat:`, `fix:`, `docs:`, `chore:`). One concern per PR.

## Tests

- Rust: `cargo test` per crate; integration tests in `src-tauri/tests/`.
- TS: Vitest in `src/**/*.test.ts(x)` (added in Phase 7).
- E2E: Playwright through Tauri's `webdriver` (Phase 7).

## Local dev

```bash
pnpm install
pnpm tauri:dev   # opens the desktop window with hot reload
```

If Tauri can't find webkit headers on Linux, install:
```bash
sudo apt install -y libwebkit2gtk-4.1-dev libssl-dev libayatana-appindicator3-dev \
                    librsvg2-dev build-essential libxdo-dev pkg-config
```
