# Cortex Security Notes

This is the operator-facing summary. The binding decisions are in [`adrs/ADR-006-security-model.md`](adrs/ADR-006-security-model.md).

## Threat model (lite)

Cortex is the highest-trust app on the user's machine because it owns API keys, can spawn agents that run shell commands, and indexes their memory. The realistic threats:

| Threat | Mitigation |
|---|---|
| Compromised npm/crate dep exfiltrates secrets via the renderer | Renderer has zero secret access; CSP `connect-src` allowlists only homelab IPs |
| Prompt injection from web/file content fed to an agent tricks it into reading SSH keys | Curated env, cwd pinned to project root, file-scope allowlist in Tauri capability |
| Agent CLI is itself compromised | OS keychain holds secrets, audit log catches surprising writes, no blanket FS scope |
| Cortex crashes leak chat content to Sentry | `beforeSend` strips message/content/prompt fields and token-shaped strings |
| `--dangerously-skip-permissions` left on permanently | UI toggle is session-bound, 30-min timeout, persistent banner |
| Auto-update pushes a malicious binary | Updater verifies signature against a pinned pubkey |
| One device sync overwrites memory on another | Per-write backups in `~/.local/share/cortex/backups/` (last 5 versions) |

## What lives where

- Secrets → OS keychain (`keyring` crate). Never on disk.
- Memory contents → existing files in `~/.claude/projects/*/memory/`, etc. Cortex indexes but does not duplicate.
- Chat history → `~/.local/share/cortex/cortex-local.db`. Device-local.
- Audit log → `~/.local/share/cortex/audit.log`. Append-only, 90-day retention.

## What you should rotate if Cortex is compromised

In order of priority:

1. The Cortex Gateway backend API key (`/v1/*` access).
2. Any Anthropic, OpenAI, Gemini API keys configured in Cortex.
3. Any SSH key the app could reach via a spawned agent (it can't reach `~/.ssh` directly, but a spawned `claude` *can*).
4. Re-check `nbai:*` AgentDB namespaces for unexpected writes.

## Hardening still on the roadmap

- Bubblewrap / Firejail wrapper for agent subprocesses on Linux (Phase 7+).
- Notarized + signed builds for all OSes before any public release.
- `cargo audit` and `pnpm audit` gates in CI.
- An "incognito" session mode that disables memory write-back.

## Reporting

It's a single-user project; if you find an issue, open one on GitHub.
