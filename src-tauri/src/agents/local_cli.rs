//! Generic, data-driven local-CLI agent framework.
//!
//! Any headless AI CLI (claude, codex, gemini, …) can be launched locally with
//! NO homelab dependency by describing it as a [`CliSpec`] and registering a
//! [`GenericCliAgent`] around it. Auth is each CLI's own login — Cortex just
//! spawns the binary and translates its stdout into Cortex [`AgentEvent`]s.
//!
//! This generalizes the original hand-written `claude_cli` adapter: binary
//! discovery is delegated to [`crate::agents::cli_discovery`], and the
//! spawn/stream loop (kill_on_drop, null stdin, piped stdout+stderr, concurrent
//! stderr drain, per-line parse) is shared. The per-CLI specifics — argv,
//! capabilities, and how to parse a line of stdout — live entirely in the spec.
//!
//! Claude is itself expressed as a spec ([`crate::agents::claude_spec::CLAUDE_SPEC`]),
//! so its registry id (`"claude-cli"`), capabilities, and event stream are
//! identical to the original adapter.

use super::adapter::{
    AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest,
};
use super::cli_discovery::{self, DirProvider};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// How a CLI's stdout should be parsed into [`AgentEvent`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// Newline-delimited Claude Code `stream-json` events (the original
    /// `claude_cli` format): partial-message deltas → Token/Reasoning, tool-use
    /// starts → ToolCall, terminal `result` → Done (+ optional Error).
    ClaudeStreamJson,
    /// Newline-delimited JSON events emitted by OpenAI Codex (`codex exec
    /// --json`) and xAI Grok Build (`grok -p --output-format streaming-json`),
    /// which share the same thread/turn/item event vocabulary. `item.completed`
    /// of an assistant message → Token; `command_execution` items → ToolCall;
    /// `turn.completed` → Done (usage tokens); `error` → Error.
    CodexJsonl,
    /// A single JSON object printed by Gemini CLI / Qwen Code under
    /// `--output-format json`: `{ "response": "...", "stats": {...},
    /// "error": {...}? }`. The whole stdout is buffered, parsed once, and the
    /// `response` string is emitted as one Token before Done. Any `error` is
    /// surfaced as an Error.
    GeminiJson,
    /// Plain text: each stdout line/chunk is forwarded verbatim as a Token, and
    /// EOF yields a single Done. For CLIs without a structured stream format.
    PlainTextStream,
}

/// Context handed to a spec's `headless_args` builder. Carries everything an
/// argv builder needs without coupling the spec to the full [`ChatRequest`].
pub struct LaunchCtx<'a> {
    /// The fully-built prompt (history already folded in by the caller).
    pub prompt: &'a str,
    /// Resolved model slug/hint for this turn (may be empty if N/A).
    pub model: &'a str,
    /// The original request, for specs that need extra fields.
    pub req: &'a ChatRequest,
}

/// A fully data-driven description of one local AI CLI. Adding a new CLI is just
/// a new `static CliSpec` plus a registration in `lib.rs` — no new adapter type.
pub struct CliSpec {
    /// Registry id (also the descriptor id). e.g. `"claude-cli"`.
    pub id: &'static str,
    /// Human label for the picker. e.g. `"Claude (CLI)"`.
    pub label: &'static str,
    /// Descriptor description line.
    pub description: &'static str,
    /// Per-OS candidate binary file names, in preference order. On Windows
    /// include the `.exe`/`.cmd`/`.bat` shims; POSIX is the bare name.
    pub bin_names: &'static [&'static str],
    /// Extra directories to search beyond `~/.local/bin` and `$PATH` (e.g. the
    /// Windows npm global prefix). May be empty.
    pub extra_dirs: &'static [DirProvider],
    /// Builds the headless argv (excluding the binary itself) for one turn.
    pub headless_args: fn(&LaunchCtx) -> Vec<String>,
    /// How to parse this CLI's stdout.
    pub output_kind: OutputKind,
    /// Capabilities advertised in the descriptor.
    pub capabilities: &'static [AgentCapability],
    /// Where to point a user who needs to install the CLI.
    pub install_url: &'static str,
    /// One-line install hint surfaced in the "not found" error.
    pub install_hint: &'static str,
    /// A short logging/error tag for this CLI (e.g. `"claude"`).
    pub tag: &'static str,
    /// The login/auth command (and args) a user runs to sign this CLI into
    /// their account, e.g. `&["codex", "login"]` or `&["claude", "/login"]`.
    /// Empty when the CLI authenticates only via an API-key env var (aider),
    /// or when sign-in is the bare interactive binary (Gemini). The first
    /// element is the program; the rest are its args. Surfaced to the Settings
    /// "Sign in" button, which spawns it in a PTY.
    pub login_cmd: &'static [&'static str],
    /// Fallback model slug when the request carries no slug this CLI recognizes.
    /// Empty means "pass the raw request model through (or nothing)".
    pub default_model: &'static str,
    /// Lowercase slug prefixes this CLI "owns". When the per-call model slug
    /// starts with any of these (or canonicalizes to this CLI's catalog
    /// source), it is forwarded as-is; otherwise `default_model` is used. Empty
    /// means "always forward the raw request model verbatim".
    pub model_prefixes: &'static [&'static str],
    /// Best-effort auth markers, relative to the user's home dir (e.g.
    /// `".codex/auth.json"`). If ANY exists, the CLI is *probably* signed in.
    /// Empty means "auth state is unknown / not file-detectable" — the Settings
    /// UI then reports `authenticated: None` (it still offers the Sign-in
    /// button). Never a hard gate; just a hint surfaced to the user.
    pub auth_paths: &'static [&'static str],
}

impl CliSpec {
    /// Resolve this CLI's binary, if installed.
    pub fn discover(&self) -> Option<PathBuf> {
        cli_discovery::discover(self.bin_names, self.extra_dirs)
    }

    /// Best-effort sign-in probe from `auth_paths`. Returns:
    ///   * `None`  — no markers configured (auth state not file-detectable), or
    ///     the home dir can't be resolved.
    ///   * `Some(true)`  — at least one marker exists (probably signed in).
    ///   * `Some(false)` — markers configured but none exist (probably not).
    pub fn authenticated(&self) -> Option<bool> {
        if self.auth_paths.is_empty() {
            return None;
        }
        let home = dirs::home_dir()?;
        Some(self.auth_paths.iter().any(|rel| home.join(rel).exists()))
    }
}

/// A generic [`AgentAdapter`] over a [`CliSpec`]. One instance per CLI; the spec
/// is `&'static` so this is cheap to construct and register.
pub struct GenericCliAgent {
    spec: &'static CliSpec,
}

impl GenericCliAgent {
    pub fn new(spec: &'static CliSpec) -> Self {
        Self { spec }
    }
}

#[async_trait::async_trait]
impl AgentAdapter for GenericCliAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            id: self.spec.id.to_string(),
            label: self.spec.label.to_string(),
            description: self.spec.description.to_string(),
            capabilities: self.spec.capabilities.to_vec(),
            // Reflects whether the binary is resolvable, so the picker / routing
            // can avoid offering an agent that can't actually run.
            available: self.spec.discover().is_some(),
        }
    }

    async fn health_check(&self) -> bool {
        self.spec.discover().is_some()
    }

    async fn run(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        let spec = self.spec;
        let id = spec.id;

        let Some(bin) = spec.discover() else {
            let _ = tx
                .send(AgentEvent::Started { agent_id: id.into(), run_id: None })
                .await;
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!(
                        "`{}` CLI not found. {} ({})",
                        spec.tag, spec.install_hint, spec.install_url
                    ),
                })
                .await;
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
            return Ok(());
        };

        // Resolve the model via the spec-agnostic hook on the request. Specs that
        // don't care just ignore the model string in `headless_args`.
        let model = spec_resolve_model(spec, &req);

        // Working directory: the project root when it's a real dir, else home,
        // else the OS temp dir. CLIs need a sane cwd for their file tools.
        let cwd: PathBuf = req
            .project_root
            .as_ref()
            .filter(|p| p.is_dir())
            .cloned()
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir);

        // Build the prompt (history folded in), then let the spec build argv.
        let prompt = build_prompt(&req);
        let ctx = LaunchCtx { prompt: &prompt, model: &model, req: &req };
        let args = (spec.headless_args)(&ctx);

        // Build argv individually — the user message is a single arg, never a
        // shell string, so it can't break out / inject.
        // On Windows, npm-installed CLIs resolve to `.cmd`/`.bat` shims, which
        // cannot be launched via CreateProcess (and thus `Command::new`) directly
        // — they must run through `cmd.exe /C`. `.exe` shims and all POSIX
        // binaries spawn directly. Without this, every npm-installed maker CLI
        // (claude, codex, …) fails with "failed to spawn" on Windows.
        let is_shim = cfg!(windows)
            && matches!(
                bin.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref(),
                Some("cmd") | Some("bat")
            );
        let mut cmd = if is_shim {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&bin).args(&args);
            c
        } else {
            let mut c = Command::new(&bin);
            c.args(&args);
            c
        };
        cmd.current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Started { agent_id: id.into(), run_id: None })
                    .await;
                let _ = tx
                    .send(AgentEvent::Error {
                        message: format!("failed to spawn `{}`: {e}", spec.tag),
                    })
                    .await;
                let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
                return Ok(());
            }
        };

        // Announce the run immediately; structured streams may carry a real
        // session id later, but the UI wants a Started promptly.
        let _ = tx
            .send(AgentEvent::Started { agent_id: id.into(), run_id: None })
            .await;

        // Drain stderr concurrently so a chatty CLI can't dead-lock the pipe.
        // Keep a short tail to surface on a non-zero exit with no result.
        let stderr = child.stderr.take();
        let tag = spec.tag;
        let stderr_task = tokio::spawn(async move {
            let mut tail = String::new();
            if let Some(stderr) = stderr {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "local_cli", tag, "stderr: {line}");
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > 2048 {
                        let cut = tail.len() - 2048;
                        tail.drain(..cut);
                    }
                }
            }
            tail
        });

        // Parse stdout keyed by the spec's output_kind. Streaming kinds parse
        // line-by-line; GeminiJson buffers the whole object and parses once.
        let mut saw_result = false;
        let mut last_rate_limit: Option<Value> = None;
        if let Some(stdout) = child.stdout.take() {
            if spec.output_kind == OutputKind::GeminiJson {
                // Buffer the entire stdout, then parse the single JSON object.
                use tokio::io::AsyncReadExt;
                let mut buf = String::new();
                let mut rdr = BufReader::new(stdout);
                let _ = rdr.read_to_string(&mut buf).await;
                if handle_gemini_json(&buf, &tx).await {
                    saw_result = true;
                }
            } else {
                let mut lines = BufReader::new(stdout).lines();
                // Loop (not `while let Ok(...)`) so a transient read/decode error
                // on one line skips that line instead of aborting the stream.
                loop {
                    let line = match lines.next_line().await {
                        Ok(Some(line)) => line,
                        Ok(None) => break,  // EOF
                        Err(_) => continue, // bad line — skip, keep parsing
                    };
                    match spec.output_kind {
                        OutputKind::ClaudeStreamJson => {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let Ok(json) = serde_json::from_str::<Value>(line)
                            else {
                                continue; // skip non-JSON noise
                            };
                            // Capture rate-limit events (out-of-band).
                            if json.get("type").and_then(Value::as_str)
                                == Some("rate_limit_event")
                            {
                                if let Some(info) = json.get("rate_limit_info") {
                                    last_rate_limit = Some(info.clone());
                                }
                                continue;
                            }
                            if let EventOutcome::Result =
                                handle_claude_event(&json, &tx).await
                            {
                                saw_result = true;
                            }
                        }
                        OutputKind::CodexJsonl => {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let Ok(json) = serde_json::from_str::<Value>(line)
                            else {
                                continue; // skip non-JSON noise
                            };
                            if let EventOutcome::Result =
                                handle_codex_event(&json, &tx).await
                            {
                                saw_result = true;
                            }
                        }
                        OutputKind::PlainTextStream => {
                            // Each line is forwarded verbatim (newline preserved)
                            // as a Token. Done is emitted once at EOF below.
                            let _ = tx
                                .send(AgentEvent::Token {
                                    delta: format!("{line}\n"),
                                })
                                .await;
                        }
                        // Handled above by the buffer-all branch.
                        OutputKind::GeminiJson => unreachable!(),
                    }
                }
            }
        }

        // Best-effort: persist the latest rate-limit info for the dashboard.
        if let Some(info) = last_rate_limit.clone() {
            tokio::task::spawn_blocking(move || persist_claude_limit(&info));
        }

        // Reap the process and inspect its exit status.
        let status = child.wait().await;
        let stderr_tail = stderr_task.await.unwrap_or_default();

        // If the process failed and we never got a terminal result, surface
        // stderr. For PlainTextStream there is no in-band result, so any
        // non-zero exit reports here.
        let exited_bad = matches!(&status, Ok(s) if !s.success()) || status.is_err();
        if exited_bad && !saw_result {
            let tail = stderr_tail.trim();
            let msg = if tail.is_empty() {
                match &status {
                    Ok(s) => format!("`{}` exited with {s}", spec.tag),
                    Err(e) => format!("`{}` wait failed: {e}", spec.tag),
                }
            } else {
                let last = tail.lines().last().unwrap_or(tail);
                format!("`{}` failed: {last}", spec.tag)
            };
            let _ = tx.send(AgentEvent::Error { message: msg }).await;
        }

        // Always close with a Done if a terminal result didn't already emit one.
        if !saw_result {
            let _ = tx.send(AgentEvent::Done { total_tokens: None, run_id: None }).await;
        }

        Ok(())
    }
}

/// Resolve the model string for a spec, generically, from its `model_prefixes`
/// + `default_model`:
///
///   * If the per-call slug starts with any prefix this CLI owns, OR the slug
///     canonicalizes to this CLI's catalog source, forward it verbatim — the
///     user explicitly asked for one of this provider's models.
///   * Otherwise fall back to the spec's `default_model` (when set), so a
///     cross-provider slug doesn't get handed to the wrong CLI.
///   * A spec with no prefixes and no default just forwards the raw slug (or
///     empty), letting the CLI use its own configured default.
///
/// Kept here so model policy stays adjacent to the generic loop.
fn spec_resolve_model(spec: &CliSpec, req: &ChatRequest) -> String {
    let raw = req.model.as_deref().map(str::trim).unwrap_or("");
    let lower = raw.to_ascii_lowercase();

    let owns = !lower.is_empty()
        && (spec.model_prefixes.iter().any(|p| lower.starts_with(p))
            || crate::orchestrator::aliases::source_of(&lower) == Some(spec.id));

    if owns {
        return raw.to_string();
    }
    if !spec.default_model.is_empty() {
        return spec.default_model.to_string();
    }
    raw.to_string()
}

// ---------------------------------------------------------------------------
// Prompt building (moved verbatim from claude_cli; CLI-agnostic).
// ---------------------------------------------------------------------------

/// Multi-turn coherence for headless single-shot CLIs: each call is a fresh,
/// stateless turn, so prior context is re-supplied in-band. Renders
/// `req.history` into a compact `<conversation_history>` block prepended to the
/// user message (a single process arg — never a shell string).
const MAX_HISTORY_TURNS: usize = 20;
const MAX_HISTORY_BYTES: usize = 12 * 1024;

pub(crate) fn build_prompt(req: &ChatRequest) -> String {
    if req.history.is_empty() {
        return req.message.clone();
    }

    let render = |turn: &super::adapter::ChatTurn| -> Option<String> {
        let content = turn.content.trim();
        if content.is_empty() {
            return None;
        }
        let label = match turn.role.trim().to_lowercase().as_str() {
            "assistant" => "Assistant",
            "system" => "System",
            _ => "User",
        };
        Some(format!("{label}: {content}"))
    };

    let mut rendered: Vec<String> = req
        .history
        .iter()
        .rev()
        .take(MAX_HISTORY_TURNS)
        .filter_map(render)
        .collect();
    rendered.reverse(); // back to chronological order

    let mut total: usize = rendered.iter().map(|s| s.len() + 1).sum();
    let mut start = 0;
    while start < rendered.len() && total > MAX_HISTORY_BYTES {
        total -= rendered[start].len() + 1;
        start += 1;
    }
    let transcript = rendered[start..].join("\n");

    if transcript.is_empty() {
        return req.message.clone();
    }

    format!(
        "The following is prior conversation context for reference only. \
Use it to stay coherent, but only the final user message below needs a response.\n\
<conversation_history>\n{transcript}\n</conversation_history>\n\n{}",
        req.message
    )
}

// ---------------------------------------------------------------------------
// ClaudeStreamJson parser (moved verbatim from claude_cli::handle_event).
// ---------------------------------------------------------------------------

enum EventOutcome {
    None,
    /// The terminal `result` event was seen. Any error it carried has already
    /// been emitted as an `AgentEvent::Error` inside the handler, so the caller
    /// only needs to know that a result arrived (to suppress the synthetic Done).
    Result,
}

/// Map a single parsed Claude `stream-json` event onto zero or more
/// [`AgentEvent`]s. Returns whether this was the terminal `result` event (which
/// also emits Done).
async fn handle_claude_event(json: &Value, tx: &mpsc::Sender<AgentEvent>) -> EventOutcome {
    let ty = json.get("type").and_then(Value::as_str).unwrap_or("");

    match ty {
        "system" => EventOutcome::None,

        "stream_event" => {
            let Some(event) = json.get("event") else {
                return EventOutcome::None;
            };
            let etype = event.get("type").and_then(Value::as_str).unwrap_or("");
            match etype {
                "content_block_delta" => {
                    let delta = event.get("delta");
                    let dtype = delta
                        .and_then(|d| d.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    match dtype {
                        "text_delta" => {
                            if let Some(text) =
                                delta.and_then(|d| d.get("text")).and_then(Value::as_str)
                            {
                                let _ = tx
                                    .send(AgentEvent::Token { delta: text.to_string() })
                                    .await;
                            }
                        }
                        "thinking_delta" => {
                            if let Some(text) = delta
                                .and_then(|d| d.get("thinking"))
                                .and_then(Value::as_str)
                            {
                                let _ = tx
                                    .send(AgentEvent::Reasoning { text: text.to_string() })
                                    .await;
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_start" => {
                    let block = event.get("content_block");
                    let is_tool = block
                        .and_then(|b| b.get("type"))
                        .and_then(Value::as_str)
                        == Some("tool_use");
                    if is_tool {
                        let name = block
                            .and_then(|b| b.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let _ = tx
                            .send(AgentEvent::ToolCall {
                                name,
                                args: Value::Null,
                                preview: None,
                            })
                            .await;
                    }
                }
                _ => {}
            }
            EventOutcome::None
        }

        "result" => {
            let is_error = json
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            if is_error {
                let message = json
                    .get("api_error_status")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| json.get("result").and_then(Value::as_str))
                    .unwrap_or("claude returned an error")
                    .to_string();
                let _ = tx.send(AgentEvent::Error { message }).await;
            }

            let total_tokens = json
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64);

            let _ = tx
                .send(AgentEvent::Done { total_tokens, run_id: None })
                .await;
            EventOutcome::Result
        }

        _ => EventOutcome::None,
    }
}

// ---------------------------------------------------------------------------
// CodexJsonl parser — OpenAI Codex (`codex exec --json`) + xAI Grok Build
// (`grok -p --output-format streaming-json`). Both emit newline-delimited JSON
// with a shared `thread.*` / `turn.*` / `item.*` / `error` vocabulary:
//   {"type":"item.completed","item":{"type":"assistant_message","text":"…"}}
//   {"type":"item.completed","item":{"type":"command_execution","command":"…"}}
//   {"type":"turn.completed","usage":{"input_tokens":…,"output_tokens":…}}
//   {"type":"error","message":"…"}
// We forward assistant text as a Token, command executions as a ToolCall, and
// close on `turn.completed` (Done + usage). An `error` event emits Error.
// ---------------------------------------------------------------------------

/// Map one parsed Codex/Grok JSONL event onto zero or more [`AgentEvent`]s.
/// Returns whether this was a terminal `turn.completed` (which emits Done).
async fn handle_codex_event(json: &Value, tx: &mpsc::Sender<AgentEvent>) -> EventOutcome {
    let ty = json.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "item.completed" | "item.updated" => {
            let Some(item) = json.get("item") else {
                return EventOutcome::None;
            };
            let itype = item.get("type").and_then(Value::as_str).unwrap_or("");
            match itype {
                // Final assistant message: stream its text out as a Token.
                "assistant_message" | "agent_message" => {
                    if let Some(text) = item
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        let _ = tx
                            .send(AgentEvent::Token { delta: text.to_string() })
                            .await;
                    }
                }
                // Model "thinking" / reasoning blocks.
                "reasoning" => {
                    if let Some(text) = item
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        let _ = tx
                            .send(AgentEvent::Reasoning { text: text.to_string() })
                            .await;
                    }
                }
                // A shell command the agent ran — surface as a ToolCall so the
                // UI shows the activity (honest: these CLIs do run shell).
                "command_execution" => {
                    let cmd = item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let _ = tx
                        .send(AgentEvent::ToolCall {
                            name: "shell".to_string(),
                            args: Value::Null,
                            preview: if cmd.is_empty() { None } else { Some(cmd) },
                        })
                        .await;
                }
                // A file edit/patch the agent applied.
                "file_change" | "patch" => {
                    let _ = tx
                        .send(AgentEvent::ToolCall {
                            name: "edit".to_string(),
                            args: Value::Null,
                            preview: None,
                        })
                        .await;
                }
                _ => {}
            }
            EventOutcome::None
        }

        "turn.completed" | "thread.completed" => {
            let total_tokens = json
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64);
            let _ = tx
                .send(AgentEvent::Done { total_tokens, run_id: None })
                .await;
            EventOutcome::Result
        }

        "error" => {
            let message = json
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| json.get("error").and_then(Value::as_str))
                .unwrap_or("CLI returned an error")
                .to_string();
            let _ = tx.send(AgentEvent::Error { message }).await;
            EventOutcome::None
        }

        _ => EventOutcome::None,
    }
}

// ---------------------------------------------------------------------------
// GeminiJson parser — Gemini CLI / Qwen Code (`--output-format json`). The
// whole stdout is a single object: { "response": "...", "stats": {...},
// "error": {...}? }. We emit `response` as one Token, then Done; an `error`
// object (or a parse failure with non-empty text) is surfaced as Error.
// ---------------------------------------------------------------------------

/// Parse the buffered Gemini/Qwen JSON object and emit events. Returns `true`
/// when a terminal Done was emitted (so the caller suppresses the synthetic one).
async fn handle_gemini_json(buf: &str, tx: &mpsc::Sender<AgentEvent>) -> bool {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return false;
    }
    let Ok(json) = serde_json::from_str::<Value>(trimmed) else {
        // Not JSON (e.g. the CLI fell back to text or printed a bare error):
        // forward the raw text so the user still sees something useful.
        let _ = tx
            .send(AgentEvent::Token { delta: trimmed.to_string() })
            .await;
        let _ = tx
            .send(AgentEvent::Done { total_tokens: None, run_id: None })
            .await;
        return true;
    };

    // Surface an error object if present.
    if let Some(err) = json.get("error") {
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| err.as_str())
            .unwrap_or("CLI returned an error")
            .to_string();
        if !message.is_empty() {
            let _ = tx.send(AgentEvent::Error { message }).await;
        }
    }

    if let Some(text) = json
        .get("response")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let _ = tx
            .send(AgentEvent::Token { delta: text.to_string() })
            .await;
    }

    let total_tokens = json
        .get("stats")
        .and_then(|s| {
            s.get("output_tokens")
                .or_else(|| s.get("tokens").and_then(|t| t.get("output")))
        })
        .and_then(Value::as_u64);

    let _ = tx
        .send(AgentEvent::Done { total_tokens, run_id: None })
        .await;
    true
}

// ---------------------------------------------------------------------------
// Claude rate-limit persistence (moved verbatim from claude_cli).
// ---------------------------------------------------------------------------

use std::time::{SystemTime, UNIX_EPOCH};

/// Persist the latest `rate_limit_info` to `~/.cortex/claude-usage.json` so the
/// Usage dashboard (`usage.rs`) can surface Claude's rate-limit window/status.
/// Atomic write; entirely best-effort.
fn persist_claude_limit(info: &Value) {
    let Some(home) = dirs::home_dir() else { return };
    let dir = home.join(".cortex");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let out = serde_json::json!({
        "status": info.get("status").and_then(Value::as_str),
        "resets_at": info.get("resetsAt").and_then(Value::as_i64),
        "rate_limit_type": info.get("rateLimitType").and_then(Value::as_str),
        "overage_status": info.get("overageStatus").and_then(Value::as_str),
        "out_of_credits": info
            .get("overageDisabledReason")
            .and_then(Value::as_str)
            == Some("out_of_credits"),
        "is_using_overage": info
            .get("isUsingOverage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "updated_ms": now_ms,
    });

    let Ok(bytes) = serde_json::to_vec_pretty(&out) else { return };
    let target = dir.join("claude-usage.json");
    let tmp = dir.join(format!("claude-usage.json.tmp.{now_ms}"));
    if std::fs::write(&tmp, &bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &target).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::ChatTurn;

    fn req(message: &str, history: Vec<ChatTurn>) -> ChatRequest {
        ChatRequest {
            session_id: "s".into(),
            message: message.into(),
            project_root: None,
            history,
            model: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn build_prompt_passthrough_without_history() {
        let r = req("hello", vec![]);
        assert_eq!(build_prompt(&r), "hello");
    }

    #[test]
    fn build_prompt_folds_history_block() {
        let r = req(
            "final question",
            vec![
                ChatTurn { role: "user".into(), content: "earlier".into(), agent: None },
                ChatTurn {
                    role: "assistant".into(),
                    content: "reply".into(),
                    agent: None,
                },
            ],
        );
        let p = build_prompt(&r);
        assert!(p.contains("<conversation_history>"));
        assert!(p.contains("User: earlier"));
        assert!(p.contains("Assistant: reply"));
        assert!(p.ends_with("final question"));
    }

    #[test]
    fn claude_spec_descriptor_is_stable() {
        let agent = GenericCliAgent::new(&crate::agents::claude_spec::CLAUDE_SPEC);
        let d = agent.descriptor();
        assert_eq!(d.id, "claude-cli");
        assert_eq!(d.label, "Claude (CLI)");
        assert_eq!(
            d.capabilities,
            vec![
                AgentCapability::Chat,
                AgentCapability::CodeEdit,
                AgentCapability::ShellExec,
                AgentCapability::Vision,
                AgentCapability::LongContext,
                AgentCapability::Approval,
            ]
        );
    }

    #[test]
    fn claude_spec_builds_expected_argv() {
        let r = req("what is 2+2", vec![]);
        let ctx = LaunchCtx {
            prompt: "what is 2+2",
            model: "claude-sonnet-4-6",
            req: &r,
        };
        let args = (crate::agents::claude_spec::CLAUDE_SPEC.headless_args)(&ctx);
        assert_eq!(
            args,
            vec![
                "-p",
                "what is 2+2",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--model",
                "claude-sonnet-4-6",
            ]
        );
    }

    #[test]
    fn model_resolution_honors_claude_slug_else_defaults() {
        let spec = &crate::agents::claude_spec::CLAUDE_SPEC;
        let mut r = req("hi", vec![]);
        r.model = Some("claude-opus-4-1".into());
        assert_eq!(spec_resolve_model(spec, &r), "claude-opus-4-1");
        r.model = Some("gpt-4o".into()); // not a claude slug
        assert_eq!(
            spec_resolve_model(spec, &r),
            crate::agents::claude_spec::DEFAULT_MODEL
        );
        r.model = None;
        assert_eq!(
            spec_resolve_model(spec, &r),
            crate::agents::claude_spec::DEFAULT_MODEL
        );
    }
}
