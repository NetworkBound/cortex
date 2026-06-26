# Changelog

All notable changes to Cortex are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Phase 0: project scaffold (Tauri 2 + React + TS + Rust), CI/release
  workflows, full docs (architecture, 7 ADRs, security, privacy, contributing).
- Phase 2: streaming chat to the Hermes OpenAI-compatible gateway and to
  Claude / Codex / Ollama CLI agents. Orchestrator with `@mention` routing,
  capability-based scoring, and "ask both" fan-out.
- Phase 3: memory layer reading `~/.claude/projects/*/memory/`, project
  `runbooks/`, global CLAUDE.md files, optional Obsidian vault; chroma
  substring search; per-write versioned backup writer; rsync-over-Tailscale
  cross-device sync.
- Phase 4: local SQLite span store (OpenTelemetry-shaped), homelab health
  pollers for user's configured hosts + a remote host services, Sentry redactor.
- Phase 5: project discovery under `~/projects/*`, file tree, project
  switch wired to spawned agents' cwd; Cmd/Ctrl+K command palette.
- Phase 6: GH Actions matrix release workflow, Tauri updater wiring docs,
  icon generator script.
- Phase 7: audit log (jsonl, retained 90 days), integration tests for
  routing / memory / tracing / redaction.
