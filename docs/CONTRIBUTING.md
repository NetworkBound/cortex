# Contributing to Cortex

> Internal doc — Cortex is currently a single-developer project. This describes how to extend it and what has to pass before a change lands.

## Local dev

```bash
pnpm install
bash scripts/build-tsnet-sidecar.sh   # required: tauri.conf.json declares the sidecar as externalBin
pnpm tauri:dev                        # opens the desktop window with hot reload
```

`scripts/setup-dev.sh` does all of that plus the Linux system packages (apt, dnf, pacman, zypper). If Tauri can't find webkit headers on Linux, install:

```bash
sudo apt install -y libwebkit2gtk-4.1-dev libssl-dev libayatana-appindicator3-dev \
                    librsvg2-dev build-essential libxdo-dev libdbus-1-dev pkg-config
```

On Windows you need Visual Studio Build Tools (C++ workload), Go, and the WebView2 runtime. Build the sidecar from Git Bash with the same script, or with `go build` as shown in the README, then `pnpm tauri:dev` from PowerShell.

The mobile PWA in `mobile/` is its own pnpm package (`cd mobile && pnpm install && pnpm build`); the embedded server looks for `mobile/dist` next to `src-tauri/` at runtime.

## Testing

CI (`.github/workflows/ci.yml`) runs on every push and pull request, on both `ubuntu-latest` and `windows-latest`. A change has to pass all of these on both:

| Gate | Command | What it catches |
|---|---|---|
| TypeScript | `pnpm typecheck` | type errors in `src/` and the Vite config |
| ESLint | `pnpm lint` | lint errors in `src/` (react-hooks, react-refresh, typescript-eslint) |
| Frontend build | `pnpm build` | anything Vite can't bundle |
| Rust build | `cargo check --manifest-path src-tauri/Cargo.toml --all-targets` | compile errors, including in tests and the `cortex-serve` bin |
| Rust tests | `cargo test --manifest-path src-tauri/Cargo.toml` | unit tests (`#[cfg(test)] mod tests` in ~180 files) and `src-tauri/tests/integration.rs` |

`cargo fmt --check` and `cargo clippy` also run, but as advisory steps that don't fail the build.

Shortcuts: `pnpm check` runs typecheck + lint + `cargo check --all-targets`; `pnpm test` runs `cargo test`. Run both before pushing.

Things to keep in mind when writing tests:

- The Rust side is tested without a Tauri runtime. Logic that needs `AppHandle` or a window belongs behind a thin command wrapper so the rest can be tested as plain functions.
- Tests that touch the home directory must use `paths::test_home::with_temp_home` rather than setting `$HOME`: on Windows `dirs::home_dir()` comes from the known-folder API and ignores env vars. The helper also serialises tests that would otherwise race on the shared env.
- Tests run on Windows too. Don't assert on `/` separators, `\n`-only output, `chmod` bits, or `sh`. Use `std::path` joins and `cfg!(windows)` where the behaviour legitimately differs.
- There are no frontend unit tests today (no Vitest config); `playwright` is a devDependency but only used ad hoc. The typecheck and ESLint gates are what guard `src/`.

## Adding a new agent

Most providers do not need Rust code. CLI agents are declared as a `CliSpec` (see `agents/claude_spec.rs`, `codex_spec.rs`, `gemini_spec.rs`, …) and run by the shared `GenericCliAgent` in `agents/local_cli.rs`, which already handles Windows `.cmd`/`.exe` shims, `CREATE_NO_WINDOW`, and stream parsing. OpenAI-compatible APIs are rows in `PROVIDERS` (`agents/openai_compat.rs`), local runtimes are rows in `RUNTIMES` (`agents/local_runtime.rs`), and anything else OpenAI-shaped can be added at runtime through the Model Fabric UI (`agents/custom_endpoint.rs`).

If you really need a new adapter:

### 1. Implement the adapter

Create `src-tauri/src/agents/your_agent.rs`:

```rust
use super::adapter::{AgentAdapter, AgentDescriptor, AgentEvent, ChatRequest};
use tokio::sync::mpsc;

pub struct YourAgent { /* config */ }

#[async_trait::async_trait]
impl AgentAdapter for YourAgent {
    fn descriptor(&self) -> AgentDescriptor { /* id, label, capabilities, cost hints */ }
    async fn health_check(&self) -> bool { /* cheap ping */ }
    async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        // emit AgentEvent::Started, ::Token deltas, ::ToolCall / ::ToolResult, ::Done
        Ok(())
    }
}
```

### 2. Register it

In `src-tauri/src/agents/mod.rs`, add `pub mod your_agent;`. In `src-tauri/src/lib.rs`, in the registry seeding block (look for `reg.register(Arc::new(...))`), push `Arc::new(YourAgent::new(...))`. The headless `build_headless_state` (used by `cortex-serve`) has its own seeding block a bit further down; add it there too if the mobile server should see it.

### 3. Add UI metadata

In `src/lib/models.ts`, add an entry so the model picker and sidebar can render it before the backend reports health.

The orchestrator, observability, audit log, and cost tracking wire up automatically because they only see `AgentAdapter` + `AgentEvent`.

## Adding a new observability record

Runs are recorded through `TracingStore` (`src-tauri/src/observability/tracing_store.rs`): `start_agent_run` opens a span, `record_event` stores each `AgentEvent`, `finish_agent_run` closes it, and `record_chat_turn` / `record_audit` / `record_health` cover the other tables. The schema lives in `observability/schema.sql`; additive changes go in `TracingStore::migrate`. If the panel needs to render something new, add a case in `src/components/ObservabilityPanel.tsx` (or the Run Replay / Reliability views).

## Adding a new memory source

Sources are `MemorySource` values (`src-tauri/src/memory/sources.rs`) discovered from the user's `~/.claude`, Obsidian vault, and runbook directories. Add a reader module under `src-tauri/src/memory/`, hook it into the source list, and add a path-or-config field in Settings if it is user-configurable.

## Code style

- Rust: `cargo fmt`, `cargo clippy`. No `unwrap()` on user data in command handlers — return `Result<_, String>` or use `anyhow` and map the error.
- Anything platform-specific goes behind `#[cfg(unix)]` / `#[cfg(windows)]`; both targets must compile.
- TS: `prettier`, `eslint` (`pnpm lint`). Function components only, hooks over classes.
- Commit messages: conventional commits (`feat:`, `fix:`, `docs:`, `chore:`). One concern per PR.
