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

use super::adapter::{AgentAdapter, AgentCapability, AgentDescriptor, AgentEvent, ChatRequest};
use super::cli_discovery::{self, DirProvider};
use super::cli_sessions;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// Parse the invocation line of an npm-style Windows `.cmd` shim (the file
/// `npm i -g` writes next to `node_modules`, e.g. `claude.cmd`) and return the
/// argv that follows the node program token: any node flags the shim passes,
/// then the script path — with `%dp0%` / `%~dp0` expanded to `shim_dir` and
/// the `%*` forwarder dropped. `None` when the file doesn't look like a
/// cmd-shim (or still contains an unexpanded `%var%`), in which case the
/// caller runs the shim itself.
///
/// Handles both shim generations:
///   * modern (`cmd-shim` ≥ 3): `... & "%_prog%"  "%dp0%\node_modules\x\cli.js" %*`
///   * legacy: `node  "%~dp0\node_modules\x\cli.js" %*`
///
/// Pure (no fs, no cfg) so the parser is unit-tested on every platform; the
/// Windows-only [`resolve_cmd_shim`] adds the file read + `node.exe` lookup.
pub(crate) fn parse_cmd_shim_invocation(content: &str, shim_dir: &str) -> Option<Vec<String>> {
    // The invocation line is the (last) one forwarding `%*`.
    let invocation = content.lines().rev().find(|l| l.contains("%*"))?;

    // `%~dp0` carries a trailing backslash, so `%dp0%\node_modules` is really
    // `<dir>\\node_modules`; normalize to a single separator.
    let dir = shim_dir.trim_end_matches(['\\', '/']);
    let expanded = invocation
        .replace("%dp0%\\", &format!("{dir}\\"))
        .replace("%dp0%/", &format!("{dir}/"))
        .replace("%~dp0\\", &format!("{dir}\\"))
        .replace("%~dp0/", &format!("{dir}/"))
        .replace("%dp0%", &format!("{dir}\\"))
        .replace("%~dp0", &format!("{dir}\\"));

    // cmd.exe-style tokenizer: double quotes group, no escape character.
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut open = false;
    for c in expanded.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                open = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if open {
                    tokens.push(std::mem::take(&mut cur));
                    open = false;
                }
            }
            c => {
                cur.push(c);
                open = true;
            }
        }
    }
    if open {
        tokens.push(cur);
    }

    // The node program token: `%_prog%` (modern) or a bare/absolute node[.exe].
    let is_node = |t: &str| {
        if t.contains("%_prog%") {
            return true;
        }
        let lower = t.to_ascii_lowercase();
        lower == "node"
            || lower == "node.exe"
            || lower.ends_with("\\node.exe")
            || lower.ends_with("/node.exe")
    };
    let prog_idx = tokens.iter().position(|t| is_node(t))?;

    let args: Vec<String> = tokens[prog_idx + 1..]
        .iter()
        .filter(|t| t.as_str() != "%*")
        .cloned()
        .collect();
    // Need a script path (a non-flag token) and no leftover batch variables —
    // otherwise we'd hand node something cmd.exe was supposed to expand.
    if !args.iter().any(|a| !a.starts_with('-')) || args.iter().any(|a| a.contains('%')) {
        return None;
    }
    Some(args)
}

/// Resolve a Windows `.cmd` shim to the underlying `node.exe` + argv so we can
/// call `CreateProcess` directly — bypassing `cmd.exe`, whose 8191-character
/// command-line limit a context-prefixed prompt easily exceeds, and which
/// cannot carry multi-line arguments at all. Returns `None` (caller runs the
/// shim itself) when the shim is unparseable, the script it names is missing,
/// or no `node.exe` can be found.
#[cfg(windows)]
fn resolve_cmd_shim(shim: &std::path::Path) -> Option<(PathBuf, Vec<String>)> {
    let content = std::fs::read_to_string(shim).ok()?;
    let shim_dir = shim.parent()?;
    let args = parse_cmd_shim_invocation(&content, &shim_dir.to_string_lossy())?;

    // The script is the last non-flag token; it must actually exist.
    let script = args.iter().rev().find(|a| !a.starts_with('-'))?;
    if !std::path::Path::new(script).is_file() {
        return None;
    }

    let local_node = shim_dir.join("node.exe");
    let node = if local_node.is_file() {
        local_node
    } else {
        which::which("node").ok()?
    };

    Some((node, args))
}

/// Kills the CLI's whole process tree when dropped while still armed — i.e.
/// when the run future is cancelled (Stop button aborts the task, a caller's
/// timeout fires). `kill_on_drop` alone only terminates the direct child and
/// orphans the shell tools / helpers the CLI spawned. Disarmed once the child
/// has been reaped normally. Must be declared AFTER the `Child` so it drops
/// FIRST (Windows' `taskkill /T` needs the root alive to walk the tree).
struct TreeKill {
    pid: Option<u32>,
}

impl TreeKill {
    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for TreeKill {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            crate::sys::kill_process_tree(pid);
        }
    }
}

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
    /// Native session continuity (see [`crate::agents::cli_sessions`]). Builds
    /// the argv that RESUMES the CLI's own session `id` with `ctx.prompt` as
    /// the new user message only (no folded history). `None` = the CLI has no
    /// headless resume Cortex knows how to drive; every turn folds history.
    pub resume_args: Option<fn(&LaunchCtx, &str) -> Vec<String>>,
    /// Extract the CLI's native session/thread id from one parsed stdout JSON
    /// event (Claude `system/init` → `session_id`, Codex `thread.started` →
    /// `thread_id`). Required alongside `resume_args` for resume to engage.
    pub native_session_id: Option<fn(&Value) -> Option<String>>,
    /// Whether native resume is ON when `CORTEX_CLI_NATIVE_RESUME` is unset.
    pub resume_default_on: bool,
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
        let home = crate::paths::home_dir()?;
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

    async fn run(&self, req: ChatRequest, tx: mpsc::Sender<AgentEvent>) -> anyhow::Result<()> {
        let spec = self.spec;
        let id = spec.id;

        let Some(bin) = spec.discover() else {
            let _ = tx
                .send(AgentEvent::Started {
                    agent_id: id.into(),
                    run_id: None,
                })
                .await;
            let _ = tx
                .send(AgentEvent::Error {
                    message: format!(
                        "`{}` CLI not found. {} ({})",
                        spec.tag, spec.install_hint, spec.install_url
                    ),
                })
                .await;
            let _ = tx
                .send(AgentEvent::Done {
                    total_tokens: None,
                    run_id: None,
                })
                .await;
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
            .or_else(crate::paths::home_dir)
            .unwrap_or_else(std::env::temp_dir);

        // Native session continuity: resume the CLI's own session (sending only
        // the new message) instead of re-folding the transcript, when the spec
        // supports it, the toggle allows it, and a matching id is stored for
        // this Cortex session (same adapter + model + cwd — see
        // `cli_sessions::lookup`). An empty history means a fresh conversation:
        // any stale id for this session is dropped so the CLI starts clean.
        let resume_on = spec.resume_args.is_some()
            && spec.native_session_id.is_some()
            && cli_sessions::resume_enabled(
                id,
                std::env::var(cli_sessions::ENV_TOGGLE).ok().as_deref(),
                spec.resume_default_on,
            );
        let cwd_key = cwd.to_string_lossy().into_owned();
        let mut resume_id: Option<String> = None;
        if resume_on {
            if req.history.is_empty() {
                cli_sessions::forget(&req.session_id);
            } else {
                resume_id = cli_sessions::lookup(&req.session_id, id, &model, &cwd_key);
            }
        }

        // Announce the run immediately; structured streams may carry a real
        // session id later, but the UI wants a Started promptly.
        let _ = tx
            .send(AgentEvent::Started {
                agent_id: id.into(),
                run_id: None,
            })
            .await;

        // At most two child runs: a resume attempt (if any) that falls back
        // ONCE to the classic full-history fold when it fails before producing
        // any output, then the fold itself.
        loop {
            let resuming = resume_id.is_some();
            // Resume: only the new user turn. Fold: history rendered in-band.
            let prompt = if resuming {
                req.message.clone()
            } else {
                build_prompt(&req)
            };
            let ctx = LaunchCtx {
                prompt: &prompt,
                model: &model,
                req: &req,
            };
            let args = match (resume_id.as_deref(), spec.resume_args) {
                (Some(nid), Some(build)) => build(&ctx, nid),
                _ => (spec.headless_args)(&ctx),
            };

            let outcome = match run_child(spec, &bin, &cwd, &args, &tx, resuming).await {
                Ok(o) => o,
                Err(message) => {
                    let _ = tx.send(AgentEvent::Error { message }).await;
                    let _ = tx
                        .send(AgentEvent::Done {
                            total_tokens: None,
                            run_id: None,
                        })
                        .await;
                    return Ok(());
                }
            };

            if resuming && outcome.should_fallback() {
                tracing::warn!(
                    target: "local_cli",
                    tag = spec.tag,
                    session = %req.session_id,
                    "native session resume failed before any output; falling back to folded history"
                );
                cli_sessions::forget(&req.session_id);
                resume_id = None;
                continue;
            }

            // Release anything the gate withheld (terminal Error/Done), in order.
            for evt in outcome.held {
                let _ = tx.send(evt).await;
            }

            // Remember the CLI's session id for the next turn. Only after a
            // terminal result: a run that died without one may not have left a
            // resumable session behind.
            if resume_on && outcome.saw_result {
                if let Some(nid) = outcome.native_id.as_deref() {
                    cli_sessions::remember(&req.session_id, id, nid, &model, &cwd_key);
                }
            }
            return Ok(());
        }
    }
}

/// What one child process run produced, as seen by [`run_child`].
#[derive(Debug, Default)]
struct ChildOutcome {
    /// A terminal in-band result (Claude `result`, Codex `turn.completed`,
    /// Gemini's single object) was seen — it already emitted Done.
    saw_result: bool,
    /// The process exited non-zero (or could not be waited on).
    exited_bad: bool,
    /// At least one Token / Reasoning / ToolCall / ToolResult / FileEdit /
    /// ApprovalRequest was forwarded — the UI has seen real output.
    produced_output: bool,
    /// The first `Error` message seen before any output, if any.
    first_error: Option<String>,
    /// The CLI's native session/thread id, if the spec parses one.
    native_id: Option<String>,
    /// Terminal events (Error / Done) withheld from the caller while a resume
    /// attempt could still fall back. Empty when not gating.
    held: Vec<AgentEvent>,
}

impl ChildOutcome {
    /// A resume attempt is abandoned in favour of the folded-history spawn
    /// when it failed BEFORE producing any output: a non-zero exit, or an
    /// in-band error that looks like "no such session". A rate-limit or API
    /// error with output already streamed is a real answer, not a stale id.
    fn should_fallback(&self) -> bool {
        if self.produced_output {
            return false;
        }
        self.exited_bad
            || self
                .first_error
                .as_deref()
                .is_some_and(cli_sessions::looks_like_stale_session)
    }
}

/// Sits between the per-line parsers and the caller's channel. Always records
/// what flowed through (for [`ChildOutcome`]); when `gate` is on it withholds
/// terminal Error/Done events until either real output appears (then they are
/// flushed in order) or the run ends (then the caller decides: flush, or drop
/// them and fall back).
struct EventGate<'a> {
    tx: &'a mpsc::Sender<AgentEvent>,
    gate: bool,
    out: ChildOutcome,
}

impl<'a> EventGate<'a> {
    fn new(tx: &'a mpsc::Sender<AgentEvent>, gate: bool) -> Self {
        Self {
            tx,
            gate,
            out: ChildOutcome::default(),
        }
    }

    async fn push(&mut self, evt: AgentEvent) {
        match &evt {
            AgentEvent::Token { .. }
            | AgentEvent::Reasoning { .. }
            | AgentEvent::ToolCall { .. }
            | AgentEvent::ToolResult { .. }
            | AgentEvent::FileEdit { .. }
            | AgentEvent::ApprovalRequest { .. } => self.out.produced_output = true,
            AgentEvent::Error { message } => {
                if !self.out.produced_output && self.out.first_error.is_none() {
                    self.out.first_error = Some(message.clone());
                }
            }
            _ => {}
        }
        if self.gate {
            if !self.out.produced_output
                && matches!(evt, AgentEvent::Error { .. } | AgentEvent::Done { .. })
            {
                self.out.held.push(evt);
                return;
            }
            // Real output after withheld terminal events: release them first so
            // ordering is preserved (this also means no fallback any more).
            for held in std::mem::take(&mut self.out.held) {
                let _ = self.tx.send(held).await;
            }
        }
        let _ = self.tx.send(evt).await;
    }

    /// Move everything a bounded local channel has buffered through the gate.
    async fn drain(&mut self, rx: &mut mpsc::Receiver<AgentEvent>) {
        while let Ok(evt) = rx.try_recv() {
            self.push(evt).await;
        }
    }
}

/// Spawn `bin args` in `cwd`, stream its stdout through the spec's parser into
/// `tx` (via an [`EventGate`]), reap it and report what happened. `Err` is a
/// spawn failure message (the caller emits Error + Done). The `Started` event
/// is the caller's job.
async fn run_child(
    spec: &CliSpec,
    bin: &std::path::Path,
    cwd: &std::path::Path,
    args: &[String],
    tx: &mpsc::Sender<AgentEvent>,
    gate: bool,
) -> Result<ChildOutcome, String> {
    // Build argv individually — the user message is a single arg, never a
    // shell string, so it can't break out / inject.
    //
    // On Windows, npm-installed CLIs resolve to `.cmd`/`.bat` shims. Going
    // through `cmd.exe` hits its 8191-char command-line limit as soon as
    // the prompt carries a context prefix (project rules + repo map), and
    // cmd.exe can't carry a multi-line argument at all. So we parse the
    // shim to recover the underlying `node.exe <script>` invocation and
    // call THAT via CreateProcess (32767-char limit, newline-safe). If the
    // shim can't be parsed we hand the shim itself to `Command::new`:
    // std runs `.cmd`/`.bat` through cmd.exe with proper escaping (Rust ≥
    // 1.77.2) and refuses arguments it can't escape safely — never a
    // hand-built `cmd /C <shim> <prompt>` string, which a prompt containing
    // `&`, `|` or `"` could break out of. Every child is spawned without a
    // console window (CREATE_NO_WINDOW) so nothing flashes on the desktop.
    #[cfg(windows)]
    let mut shim_fallback = false;
    let mut cmd = {
        #[cfg(windows)]
        {
            let ext = bin
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            let is_shim = matches!(ext.as_deref(), Some("cmd") | Some("bat"));
            let direct = if is_shim { resolve_cmd_shim(bin) } else { None };
            match direct {
                Some((node_exe, pre_args)) => {
                    let mut c = crate::sys::tokio_no_window(node_exe);
                    c.args(&pre_args).args(args);
                    c
                }
                None => {
                    shim_fallback = is_shim;
                    let mut c = crate::sys::tokio_no_window(bin);
                    c.args(args);
                    c
                }
            }
        }
        #[cfg(not(windows))]
        {
            let mut c = crate::sys::tokio_no_window(bin);
            c.args(args);
            // Own process group, so a Stop can take the CLI's own tool
            // subprocesses down with it (see `TreeKill`).
            c.process_group(0);
            c
        }
    };
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            #[cfg(windows)]
            let hint = if shim_fallback {
                format!(
                    " — `{}` is an npm .cmd shim Cortex could not resolve to node.exe, \
                     and cmd.exe cannot carry a multi-line prompt. Reinstall the CLI \
                     (`npm i -g …`) or use its native installer.",
                    bin.display()
                )
            } else {
                String::new()
            };
            #[cfg(not(windows))]
            let hint = String::new();
            return Err(format!("failed to spawn `{}`: {e}{hint}", spec.tag));
        }
    };
    // Declared after `child` so it drops first when the future is cancelled.
    let mut tree = TreeKill { pid: child.id() };

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

    // The parsers write into a small local channel which is drained through
    // the gate after every line (each line yields at most a handful of
    // events, far below the capacity, so the parsers never block on it).
    let (ltx, mut lrx) = mpsc::channel::<AgentEvent>(64);
    let mut gate = EventGate::new(tx, gate);

    // Parse stdout keyed by the spec's output_kind. Streaming kinds parse
    // line-by-line; GeminiJson buffers the whole object and parses once.
    let mut last_rate_limit: Option<Value> = None;
    if let Some(stdout) = child.stdout.take() {
        if spec.output_kind == OutputKind::GeminiJson {
            // Buffer the entire stdout, then parse the single JSON object.
            use tokio::io::AsyncReadExt;
            let mut buf = String::new();
            let mut rdr = BufReader::new(stdout);
            let _ = rdr.read_to_string(&mut buf).await;
            if handle_gemini_json(&buf, &ltx).await {
                gate.out.saw_result = true;
            }
            gate.drain(&mut lrx).await;
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
                        let Ok(json) = serde_json::from_str::<Value>(line) else {
                            continue; // skip non-JSON noise
                        };
                        // Capture rate-limit events (out-of-band).
                        if json.get("type").and_then(Value::as_str) == Some("rate_limit_event") {
                            if let Some(info) = json.get("rate_limit_info") {
                                last_rate_limit = Some(info.clone());
                            }
                            continue;
                        }
                        if let Some(parse) = spec.native_session_id {
                            if let Some(nid) = parse(&json) {
                                gate.out.native_id = Some(nid);
                            }
                        }
                        if let EventOutcome::Result = handle_claude_event(&json, &ltx).await {
                            gate.out.saw_result = true;
                        }
                    }
                    OutputKind::CodexJsonl => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let Ok(json) = serde_json::from_str::<Value>(line) else {
                            continue; // skip non-JSON noise
                        };
                        if let Some(parse) = spec.native_session_id {
                            if let Some(nid) = parse(&json) {
                                gate.out.native_id = Some(nid);
                            }
                        }
                        if let EventOutcome::Result = handle_codex_event(&json, &ltx).await {
                            gate.out.saw_result = true;
                        }
                    }
                    OutputKind::PlainTextStream => {
                        // Each line is forwarded verbatim (newline preserved)
                        // as a Token. Done is emitted once at EOF below.
                        let _ = ltx
                            .send(AgentEvent::Token {
                                delta: format!("{line}\n"),
                            })
                            .await;
                    }
                    // Handled above by the buffer-all branch.
                    OutputKind::GeminiJson => unreachable!(),
                }
                gate.drain(&mut lrx).await;
            }
        }
    }
    drop(ltx);
    gate.drain(&mut lrx).await;

    // Best-effort: persist the latest rate-limit info for the dashboard.
    if let Some(info) = last_rate_limit.clone() {
        tokio::task::spawn_blocking(move || persist_claude_limit(&info));
    }

    // Reap the process and inspect its exit status. Once it has exited on
    // its own there is no tree left to kill.
    let status = child.wait().await;
    tree.disarm();
    let stderr_tail = stderr_task.await.unwrap_or_default();

    // If the process failed and we never got a terminal result, surface
    // stderr. For PlainTextStream there is no in-band result, so any
    // non-zero exit reports here.
    let exited_bad = matches!(&status, Ok(s) if !s.success()) || status.is_err();
    gate.out.exited_bad = exited_bad;
    if exited_bad && !gate.out.saw_result {
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
        gate.push(AgentEvent::Error { message: msg }).await;
    }

    // Always close with a Done if a terminal result didn't already emit one.
    if !gate.out.saw_result {
        gate.push(AgentEvent::Done {
            total_tokens: None,
            run_id: None,
        })
        .await;
    }

    Ok(gate.out)
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
                                    .send(AgentEvent::Token {
                                        delta: text.to_string(),
                                    })
                                    .await;
                            }
                        }
                        "thinking_delta" => {
                            if let Some(text) = delta
                                .and_then(|d| d.get("thinking"))
                                .and_then(Value::as_str)
                            {
                                let _ = tx
                                    .send(AgentEvent::Reasoning {
                                        text: text.to_string(),
                                    })
                                    .await;
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_start" => {
                    let block = event.get("content_block");
                    let is_tool = block.and_then(|b| b.get("type")).and_then(Value::as_str)
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
                .send(AgentEvent::Done {
                    total_tokens,
                    run_id: None,
                })
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
                            .send(AgentEvent::Token {
                                delta: text.to_string(),
                            })
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
                            .send(AgentEvent::Reasoning {
                                text: text.to_string(),
                            })
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
                .send(AgentEvent::Done {
                    total_tokens,
                    run_id: None,
                })
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
            .send(AgentEvent::Token {
                delta: trimmed.to_string(),
            })
            .await;
        let _ = tx
            .send(AgentEvent::Done {
                total_tokens: None,
                run_id: None,
            })
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
            .send(AgentEvent::Token {
                delta: text.to_string(),
            })
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
        .send(AgentEvent::Done {
            total_tokens,
            run_id: None,
        })
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
    let Some(home) = crate::paths::home_dir() else {
        return;
    };
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

    let Ok(bytes) = serde_json::to_vec_pretty(&out) else {
        return;
    };
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
                ChatTurn {
                    role: "user".into(),
                    content: "earlier".into(),
                    agent: None,
                },
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

    // ---- Windows npm `.cmd` shim parsing (pure; runs on every platform) ----

    const MODERN_SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n";

    #[test]
    fn parses_modern_cmd_shim_to_script_path() {
        let args = parse_cmd_shim_invocation(MODERN_SHIM, "C:\\Users\\me\\AppData\\Roaming\\npm\\")
            .expect("modern shim parses");
        assert_eq!(
            args,
            vec!["C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js"]
        );
    }

    #[test]
    fn parses_legacy_cmd_shim_and_keeps_node_flags() {
        let legacy = "@IF EXIST \"%~dp0\\node.exe\" (\n  \"%~dp0\\node.exe\" --no-warnings \"%~dp0\\node_modules\\codex\\bin\\codex.js\" %*\n) ELSE (\n  node --no-warnings \"%~dp0\\node_modules\\codex\\bin\\codex.js\" %*\n)\n";
        let args = parse_cmd_shim_invocation(legacy, "D:\\npm").expect("legacy shim parses");
        assert_eq!(
            args,
            vec![
                "--no-warnings",
                "D:\\npm\\node_modules\\codex\\bin\\codex.js"
            ]
        );
    }

    #[test]
    fn rejects_files_that_are_not_cmd_shims() {
        // No `%*` forwarder at all.
        assert!(parse_cmd_shim_invocation("@echo off\r\necho hi\r\n", "C:\\x").is_none());
        // Forwarder present but no node program token.
        assert!(parse_cmd_shim_invocation("python \"%~dp0\\tool.py\" %*", "C:\\x").is_none());
        // Unexpanded batch variable left in an argument → fall back to the shim.
        assert!(parse_cmd_shim_invocation("node \"%SOMEWHERE%\\cli.js\" %*", "C:\\x").is_none());
        // Only flags, no script.
        assert!(parse_cmd_shim_invocation("node --version %*", "C:\\x").is_none());
    }

    // ---- Native resume: fallback decision + event gate ----

    #[test]
    fn fallback_only_when_nothing_was_streamed() {
        // Non-zero exit with no output → fall back to the fold.
        let o = ChildOutcome {
            exited_bad: true,
            ..Default::default()
        };
        assert!(o.should_fallback());
        // A "no such session" error with no output → fall back.
        let o = ChildOutcome {
            first_error: Some("No conversation found with session ID: x".into()),
            ..Default::default()
        };
        assert!(o.should_fallback());
        // An unrelated in-band error (rate limit) is a real answer, not a stale id.
        let o = ChildOutcome {
            first_error: Some("429 rate limit".into()),
            ..Default::default()
        };
        assert!(!o.should_fallback());
        // Anything already streamed to the UI → never fall back.
        let o = ChildOutcome {
            exited_bad: true,
            produced_output: true,
            first_error: Some("session not found".into()),
            ..Default::default()
        };
        assert!(!o.should_fallback());
        assert!(!ChildOutcome::default().should_fallback());
    }

    #[tokio::test]
    async fn gate_withholds_terminal_events_until_output_or_end() {
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(16);
        {
            let mut gate = EventGate::new(&tx, true);
            gate.push(AgentEvent::Error {
                message: "session not found".into(),
            })
            .await;
            gate.push(AgentEvent::Done {
                total_tokens: None,
                run_id: None,
            })
            .await;
            // Nothing reached the caller; both are held; fallback is due.
            assert!(rx.try_recv().is_err());
            assert_eq!(gate.out.held.len(), 2);
            assert_eq!(gate.out.first_error.as_deref(), Some("session not found"));
            assert!(gate.out.should_fallback());
        }
        // Output after a withheld error flushes it first, in order.
        {
            let mut gate = EventGate::new(&tx, true);
            gate.push(AgentEvent::Error {
                message: "warn".into(),
            })
            .await;
            gate.push(AgentEvent::Token { delta: "hi".into() }).await;
            assert!(matches!(rx.try_recv(), Ok(AgentEvent::Error { .. })));
            assert!(matches!(rx.try_recv(), Ok(AgentEvent::Token { .. })));
            assert!(gate.out.held.is_empty());
            assert!(gate.out.produced_output);
            assert!(!gate.out.should_fallback());
        }
        // Ungated: everything passes straight through.
        {
            let mut gate = EventGate::new(&tx, false);
            gate.push(AgentEvent::Done {
                total_tokens: None,
                run_id: None,
            })
            .await;
            assert!(matches!(rx.try_recv(), Ok(AgentEvent::Done { .. })));
            assert!(gate.out.held.is_empty());
        }
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
