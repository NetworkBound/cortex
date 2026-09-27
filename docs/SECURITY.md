# Cortex Security Notes

This is the operator-facing summary. The enforcement lives in `src-tauri/src/orchestrator/` (`sandbox.rs`, `guardrails.rs`, `command_policy.rs`, `safe_commands.rs`, `approvals.rs`, `trust.rs`) and is wired in `commands/chat.rs`.

## Threat model (lite)

Cortex is the highest-trust app on the user's machine because it owns API keys, can spawn agents that run shell commands, and indexes their memory. The realistic threats:

| Threat | Mitigation |
|---|---|
| Compromised npm/crate dep exfiltrates secrets via the renderer | Renderer has zero secret access; CSP `connect-src` allowlists only homelab IPs |
| Prompt injection from web/file content fed to an agent tricks it into reading SSH keys | Curated env, cwd pinned to project root, file-scope allowlist in Tauri capability |
| Agent CLI is itself compromised | OS keychain holds secrets, audit log catches surprising writes, no blanket FS scope |
| Cortex crashes leak chat content to Sentry | `beforeSend` strips message/content/prompt fields and token-shaped strings |
| `--dangerously-skip-permissions` left on permanently | UI toggle is session-bound, 30-min timeout, persistent banner |
| Auto-update pushes a malicious binary | The Linux AppImage self-update verifies an ed25519 signature against a pinned pubkey before writing anything; the manifest check on other platforms only links to a release and refuses cleartext HTTP to public hosts |
| One device sync overwrites memory on another | Per-write backups under the local data dir (`backups/`, last 5 versions) |

## What lives where

- Secrets → OS keychain (`keyring` crate: Secret Service / macOS Keychain / Windows Credential Manager). Never on disk in clear; the key vault's master key is also in the keychain.
- Memory contents → existing files in `~/.claude/projects/*/memory/`, etc. Cortex indexes but does not duplicate.
- Chat history → `cortex-local.db` in the local data dir: `~/.local/share/cortex/` on Linux, `%LOCALAPPDATA%\cortex\` on Windows, `~/Library/Application Support/cortex/` on macOS. Device-local.
- Audit log → `audit.log` in the same directory. Append-only.
- Policies and trust decisions → `~/.cortex/` (per user) and `<project>/.cortex/` (per project). Plain TOML/JSON, editable by hand.

## What you should rotate if Cortex is compromised

In order of priority:

1. The Cortex Gateway backend API key (`/v1/*` access).
2. Any Anthropic, OpenAI, Gemini API keys configured in Cortex.
3. Any SSH key the app could reach via a spawned agent (it can't reach `~/.ssh` directly, but a spawned `claude` *can*).
4. Re-check `nbai:*` AgentDB namespaces for unexpected writes.

## Hardening still on the roadmap

- Bubblewrap / Firejail wrapper for agent subprocesses on Linux.
- Notarized + signed builds for all OSes before any public release.
- `cargo audit` and `pnpm audit` gates in CI.
- An "incognito" session mode that disables memory write-back.

## Reporting

It's a single-user project; if you find an issue, open one on GitHub.
