use crate::agents::{AgentEvent, ChatRequest, ChatTurn};
use crate::app_state::AppState;
use crate::hooks::{self, events as hook_events, HooksConfig};
use crate::observability::tracing_store::TracingStore;
use crate::orchestrator::{
    self, load_policy, load_tier, tier_allows, trust, ApprovalPolicy, AutoApproveList, Guardrails,
    Risk, SandboxTier,
};
use once_cell::sync::OnceCell;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc;

#[derive(Debug, Deserialize)]
pub struct ChatSendArgs {
    pub session_id: String,
    pub message: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// `"plan"` blocks write/exec-style tools at the event boundary; `"act"`
    /// (default) lets them through. Independent of guardrails — high-risk
    /// guardrail hits are blocked in both modes.
    #[serde(default)]
    pub mode: Option<String>,
    /// Aider-style `/architect` split: phase 1 plans with `planner_model`
    /// (tool calls blocked), phase 2 edits with `editor_model` (normal mode,
    /// plan injected into the prompt). Both phases share `session_id` and
    /// `trace_id`. Defaults to single-phase behavior when `None`/`false`.
    #[serde(default)]
    pub architect_mode: Option<bool>,
    #[serde(default)]
    pub planner_model: Option<String>,
    #[serde(default)]
    pub editor_model: Option<String>,
    /// Image attachments, one per element. Each entry is a full
    /// `data:<mime>;base64,<payload>` URI. The frontend caps at 5 MB each and
    /// 3 per send (`composer-drop.ts::extractImageAttachments`); we
    /// re-validate here defensively before forwarding upstream.
    ///
    /// Anthropic-format image blocks would be prepended to the user message,
    /// but the current `ChatRequest` carries text-only content. We inline an
    /// `<images>` envelope into the message so multi-modal-aware adapters can
    /// pick it up, and log a warning when images are present so non-vision
    /// upstreams don't silently drop them.
    #[serde(default)]
    pub images: Vec<String>,
    /// Per-prompt model override. When set, wins over the global gateway model
    /// (see `gateway_remote.rs` — `req.model` takes precedence). `None` => the
    /// gateway picks ("Auto").
    #[serde(default)]
    pub model: Option<String>,
    /// Per-prompt reasoning-effort override (`minimal | low | medium | high`,
    /// Codex CLI parity). Wins over the global `AppState::config.reasoning_effort`
    /// default; an unrecognized value falls through to that default (see
    /// `orchestrator::reasoning::resolve`). `None` => use the global default.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

/// Hard ceiling per image attachment (after base64 expansion ≈ 5 MB raw).
const IMAGE_BYTES_CAP: usize = 8 * 1024 * 1024;
/// Hard ceiling on the number of attachments forwarded in a single turn.
const IMAGE_COUNT_CAP: usize = 3;

#[derive(Debug, Serialize)]
struct ImageBlock<'a> {
    name: &'a str,
    media_type: &'a str,
    base64: &'a str,
}

/// Strip the `data:<mime>;base64,` prefix and validate caps. Returns
/// `(media_type, base64_payload)`.
fn parse_image_data_uri(uri: &str) -> Result<(String, String), String> {
    if uri.len() > IMAGE_BYTES_CAP {
        return Err(format!(
            "image too large ({} bytes > {} cap)",
            uri.len(),
            IMAGE_BYTES_CAP
        ));
    }
    let rest = uri
        .strip_prefix("data:")
        .ok_or_else(|| "image: missing data: prefix".to_string())?;
    let semi = rest
        .find(';')
        .ok_or_else(|| "image: malformed mime".to_string())?;
    let media_type = rest[..semi].to_string();
    let after_semi = &rest[semi + 1..];
    let comma = after_semi
        .find(',')
        .ok_or_else(|| "image: missing base64 separator".to_string())?;
    let encoding = &after_semi[..comma];
    if encoding != "base64" {
        return Err(format!("image: unsupported encoding '{encoding}'"));
    }
    let payload = after_semi[comma + 1..].to_string();
    if !matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) {
        return Err(format!("image: unsupported media type '{media_type}'"));
    }
    Ok((media_type, payload))
}

/// Build the inlined `<images>...</images>` envelope (one JSON line per image)
/// that vision-aware adapters can lift out. Returns `None` when there are no
/// images so we don't pollute non-vision adapter prompts. Logs a warning so
/// it's visible in `tracing` when upstream support is unknown.
/// Read the project's rules/conventions file (`AGENTS.md`, else `.cortexrules`,
/// else `CLAUDE.md`) from the project root, if present and non-empty. Capped at
/// 8 KiB on a UTF-8 char boundary so a large file can't dominate the context
/// budget. A cross-tool standard for teaching agents project conventions
/// (opencode / Cline / Continue / OpenHands).
fn read_project_rules(root: &std::path::Path) -> Option<String> {
    for name in ["AGENTS.md", ".cortexrules", "CLAUDE.md"] {
        let Ok(content) = std::fs::read_to_string(root.join(name)) else {
            continue;
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        const CAP: usize = 8 * 1024;
        let body = if trimmed.len() > CAP {
            let mut end = CAP;
            while end > 0 && !trimmed.is_char_boundary(end) {
                end -= 1;
            }
            &trimmed[..end]
        } else {
            trimmed
        };
        return Some(format!("# {name}\n{body}"));
    }
    None
}

/// Wave 300 — assemble the context prefix injected ahead of the user's
/// message. Independently-optional blocks: the project rules/conventions
/// file (`<project_rules>`, a cross-tool standard), keyword-triggered knowledge
/// microagents (`<knowledge>`, OpenHands-style), the user's explicit file
/// manifest (`<files>`, aider's `/add`), and a ranked repo-map (`<repo_map>`,
/// aider/Continue-style auto-context). `task` is the raw user
/// message — it personalizes the repo-map ranking toward the files the user
/// is asking about. Returns "" when neither block is present (so the caller
/// leaves the message untouched). The repo-map is capped well under the rules
/// cap so it can never dominate the context budget.
fn build_context_prefix(root: &std::path::Path, task: &str) -> String {
    let mut prefix = String::new();
    if let Some(rules) = read_project_rules(root) {
        prefix.push_str(&format!("<project_rules>\n{rules}\n</project_rules>\n\n"));
    }
    // OpenHands-style knowledge microagents: repo-specific knowledge whose
    // trigger keywords appear in *this* message, injected just after the standing
    // rules so the model sees task-relevant guidance up front.
    if let Some(knowledge) = crate::commands::microagents::build_microagents_block(root, task) {
        prefix.push_str(&knowledge);
    }
    // Aider-style file manifest: files the user explicitly `/add`ed to the chat,
    // ahead of the ranked repo-map so their full contents lead the auto-context.
    if let Some(files) = crate::commands::manifest::build_manifest_block(root) {
        prefix.push_str(&files);
    }
    if let Some(map) = crate::repo_map::build_context_block(root, task, 6 * 1024) {
        prefix.push_str(&format!(
            "<repo_map>\nRanked project files (★ = how central/depended-on a file is; higher first). Use this to locate relevant code before searching.\n{map}</repo_map>\n\n"
        ));
    }
    prefix
}

fn build_images_envelope(images: &[String], message: &str) -> Option<String> {
    if images.is_empty() {
        return None;
    }
    let mut blocks: Vec<String> = Vec::new();
    let mut counted = 0usize;
    for (i, uri) in images.iter().enumerate() {
        if counted >= IMAGE_COUNT_CAP {
            tracing::warn!(
                "chat_send: dropping image attachment #{i} — over per-message cap of {IMAGE_COUNT_CAP}"
            );
            break;
        }
        match parse_image_data_uri(uri) {
            Ok((media_type, base64)) => {
                let block = ImageBlock {
                    name: "attachment",
                    media_type: &media_type,
                    base64: &base64,
                };
                if let Ok(s) = serde_json::to_string(&block) {
                    blocks.push(s);
                    counted += 1;
                }
            }
            Err(why) => {
                tracing::warn!("chat_send: skipping malformed image #{i}: {why}");
            }
        }
    }
    if blocks.is_empty() {
        return None;
    }
    tracing::warn!(
        "chat_send: forwarding {} image attachment(s) inline; vision support is upstream-dependent",
        blocks.len()
    );
    Some(format!(
        "<images>\n{}\n</images>\n\n{}",
        blocks.join("\n"),
        message
    ))
}

/// How long the architect planner phase may run before we give up and fall
/// back to a single-phase editor run (the plan is best-effort UX, not a hard
/// dependency). Generous because reasoning models can be slow to draft a plan.
const PLANNER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Emit a synthetic `token` agent-event to the chat UI under `agent_id`. Used
/// to stream the architect planner's output (and its phase header/separator)
/// into the same assistant bubble the editor phase will continue, so a turn
/// reads as one continuous "plan, then edits" message.
fn emit_chat_token(app: &tauri::AppHandle, session: &str, agent_id: &str, delta: &str) {
    let payload = serde_json::json!({
        "agent_id": agent_id,
        "event": { "type": "token", "delta": delta },
    });
    let _ = app.emit(&format!("agent-event:{}", session), payload);
}

/// Architect phase 1: drive the planner model to completion, streaming its
/// token deltas into the chat UI under `stream_agent_id`, and return the
/// collected plan text. Only `Token` events are forwarded/collected — the
/// planner's own `Started`/`Done`/tool events are swallowed so the *editor*
/// phase owns the turn lifecycle (one continuous assistant bubble, one terminal
/// `Done`). Returns `None` on an empty plan, adapter error, or timeout so the
/// caller cleanly falls back to a single-phase editor run.
async fn run_planner_phase<F>(
    adapter: std::sync::Arc<dyn crate::agents::AgentAdapter>,
    model: String,
    prompt: String,
    reasoning_effort: Option<String>,
    mut on_token: F,
) -> Option<String>
where
    F: FnMut(&str) + Send,
{
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let req = ChatRequest {
        session_id: String::new(),
        message: prompt,
        project_root: None,
        history: Vec::new(),
        model: Some(model),
        reasoning_effort,
    };
    let run_fut = {
        let adapter = adapter.clone();
        async move {
            let _ = adapter.run(req, tx).await;
        }
    };
    let collect_fut = async {
        let mut plan = String::new();
        while let Some(evt) = rx.recv().await {
            if let AgentEvent::Token { delta } = evt {
                on_token(&delta);
                plan.push_str(&delta);
            }
        }
        plan
    };
    let plan = tokio::time::timeout(PLANNER_TIMEOUT, async {
        let (_run, plan) = tokio::join!(run_fut, collect_fut);
        plan
    })
    .await
    .ok()?;
    if plan.trim().is_empty() {
        None
    } else {
        Some(plan)
    }
}

/// Fetch a URL and return its text. Strips HTML tags + collapses whitespace
/// so the model sees readable content, not raw markup. Capped at 60KB.
/// 8s timeout — anything slower probably isn't worth blocking the chat
/// for. Returns None on any error.
fn fetch_url_text(url: &str) -> Option<String> {
    // `reqwest` is async-only in this workspace (no `blocking` feature) —
    // bridge into Tauri's runtime with `block_on`. Caller is on a sync
    // path inside `expand_at_tokens`, so blocking here is fine.
    let url = url.to_string();
    let raw = tauri::async_runtime::block_on(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent("cortex/0.0.2")
            .build()
            .ok()?;
        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() { return None }
        resp.text().await.ok()
    })?;
    let stripped = strip_html(&raw);
    let trimmed: String = stripped.chars().take(60_000).collect();
    Some(trimmed)
}

/// Cheap HTML-to-text: drop `<script>`/`<style>` blocks entirely, strip the
/// rest of the tags, decode the most common entities, collapse whitespace.
/// Not a real parser — good enough for the "give the model a doc page"
/// use case where we just want readable text.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut drop_until: Option<&'static str> = None;
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(tail) = drop_until {
            if html[i..].to_ascii_lowercase().starts_with(tail) {
                i += tail.len();
                drop_until = None;
                continue;
            }
            i += 1;
            continue;
        }
        let c = bytes[i] as char;
        if !in_tag && c == '<' {
            let rest = &html[i..].to_ascii_lowercase();
            if rest.starts_with("<script") { drop_until = Some("</script>"); i += 7; continue; }
            if rest.starts_with("<style") { drop_until = Some("</style>"); i += 6; continue; }
            in_tag = true;
            i += 1;
            continue;
        }
        if in_tag {
            if c == '>' { in_tag = false; out.push(' '); }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    // Cheap entity decode for the common cases.
    let out = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'");
    // Collapse whitespace runs to a single space, keep paragraph breaks.
    let mut compact = String::with_capacity(out.len());
    let mut last_blank = false;
    for line in out.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !last_blank { compact.push('\n'); last_blank = true; }
            continue;
        }
        last_blank = false;
        let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        compact.push_str(&collapsed);
        compact.push('\n');
    }
    compact
}

fn short_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") { return rest.chars().take(40).collect(); }
    if let Some(rest) = url.strip_prefix("http://") { return rest.chars().take(40).collect(); }
    url.chars().take(40).collect()
}

/// Render a `@websearch:<query>` context block from live search results: a
/// scannable numbered lead-list (title · url · trimmed snippet) the model can
/// cite or follow up on with `@web:<url>`. Pure (no network) so the
/// search-results → injected-context connection is unit-testable; the live
/// fetch is proven by `websearch::live_ddg_search_*`. Returns
/// `(attachment, label)`.
fn format_websearch_attachment(query: &str, results: &[crate::websearch::WebResult]) -> (String, String) {
    let mut block = String::new();
    for (i, r) in results.iter().enumerate() {
        let title = if r.title.trim().is_empty() { r.url.clone() } else { r.title.clone() };
        block.push_str(&format!("{}. {}\n   {}\n", i + 1, title, r.url));
        let snip = r.snippet.trim();
        if !snip.is_empty() {
            let snip: String = snip.chars().take(280).collect();
            block.push_str(&format!("   {}\n", snip));
        }
    }
    let attachment = format!(
        "<!-- attached: @websearch:{query} ({} results) -->\n```text\n{block}```",
        results.len(),
    );
    let label = format!("@websearch:{}", query.chars().take(40).collect::<String>());
    (attachment, label)
}

/// Synchronously run the local brain on `message` and return up to 5
/// candidate (path, content, KB) tuples. Reuses the same scoring logic as
/// `local_brain_suggest` but doesn't go through Tauri-command plumbing —
/// callable from inside `expand_at_tokens` without an extra invoke roundtrip.
fn run_brain_inline(
    message: &str,
    project_root: Option<&std::path::Path>,
) -> Option<Vec<(std::path::PathBuf, String, f32)>> {
    let payload = futures::executor::block_on(
        crate::commands::local_brain::local_brain_suggest(
            message.to_string(),
            project_root.map(|p| p.display().to_string()),
        ),
    )
    .ok()?;
    let mut out: Vec<(std::path::PathBuf, String, f32)> = Vec::new();
    for s in payload.suggestions.into_iter().take(5) {
        let p = std::path::PathBuf::from(&s.path);
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        if meta.len() > 200 * 1024 { continue; }
        let Ok(content) = std::fs::read_to_string(&p) else { continue };
        out.push((p, content, (meta.len() as f32) / 1024.0));
    }
    Some(out)
}

/// Directory names never descended into when rendering `@tree` — version
/// control, dependency, and build-output dirs that would swamp the layout with
/// thousands of irrelevant entries.
const TREE_SKIP: &[&str] = &[
    ".git", "node_modules", "target", "dist", "build", ".next", ".turbo",
    ".cache", "out", "coverage", "__pycache__", "vendor", ".venv", ".idea",
    ".vscode", ".svelte-kit",
];

/// Render an indented, ignore-aware directory tree of `root`, descending up to
/// `max_depth` levels and emitting at most `max_entries` rows (truncation is
/// flagged with a trailing note). Hidden entries (dot-prefixed) and the
/// `TREE_SKIP` noise dirs are omitted. Ordering is deterministic — directories
/// before files, each group alphabetical — so the output is stable across runs
/// and unit-testable. Factored out of `resolve_special_token` for that reason.
fn build_tree(root: &std::path::Path, max_depth: usize, max_entries: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut truncated = false;
    build_tree_inner(root, 0, max_depth, max_entries, &mut out, &mut count, &mut truncated);
    if out.is_empty() {
        return "(empty)".to_string();
    }
    if truncated {
        out.push_str(&format!("… (truncated at {max_entries} entries)\n"));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build_tree_inner(
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    max_entries: usize,
    out: &mut String,
    count: &mut usize,
    truncated: &mut bool,
) {
    if depth >= max_depth || *truncated {
        return;
    }
    let mut entries: Vec<(String, bool)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        if name.starts_with('.') || TREE_SKIP.contains(&name.as_str()) {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((name, is_dir));
    }
    // Directories first (so a folder's children render directly beneath it),
    // then files; each group sorted alphabetically for stable output.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (name, is_dir) in entries {
        if *count >= max_entries {
            *truncated = true;
            return;
        }
        out.push_str(&"  ".repeat(depth));
        out.push_str(&name);
        if is_dir {
            out.push('/');
        }
        out.push('\n');
        *count += 1;
        if is_dir {
            build_tree_inner(
                &dir.join(&name),
                depth + 1,
                max_depth,
                max_entries,
                out,
                count,
                truncated,
            );
        }
    }
}

/// Special @-token resolvers — `@diff` runs `git diff HEAD` in the active
/// project, `@status` runs `git status --short`. Both are scoped to the
/// `cwd` argument (the active project root) so cross-project leakage is
/// impossible. Output capped at 50KB to avoid blowing the model's window.
fn resolve_special_token(token: &str, cwd: Option<&std::path::Path>) -> Option<(String, String)> {
    let Some(cwd) = cwd else { return None };
    if !cwd.is_dir() { return None }
    match token {
        "@diff" => {
            let out = crate::sys::no_window("git").arg("diff").arg("HEAD").current_dir(cwd).output().ok()?;
            if !out.status.success() { return None; }
            let s = String::from_utf8_lossy(&out.stdout);
            let trimmed: String = s.chars().take(50_000).collect();
            if trimmed.trim().is_empty() {
                Some(("@diff".into(), "(no uncommitted changes)".into()))
            } else {
                Some(("@diff".into(), trimmed))
            }
        }
        "@status" => {
            let out = crate::sys::no_window("git").arg("status").arg("--short").current_dir(cwd).output().ok()?;
            if !out.status.success() { return None; }
            let s = String::from_utf8_lossy(&out.stdout);
            let trimmed: String = s.chars().take(10_000).collect();
            Some(("@status".into(), if trimmed.trim().is_empty() { "(clean tree)".into() } else { trimmed }))
        }
        "@repomap" => {
            // Aider-style compressed symbol map of the active project —
            // functions, structs, classes, etc. with their file paths and
            // line numbers. Lets the model see the project's structural
            // context without having to read every file. Uses the existing
            // `repo_map_text` formatter.
            let map = crate::repo_map::compute_repo_map(cwd, 500);
            let text = crate::repo_map::format_as_text(&map);
            if text.trim().is_empty() {
                Some(("@repomap".into(), "(no symbols found)".into()))
            } else {
                Some(("@repomap".into(), text.chars().take(60_000).collect()))
            }
        }
        "@cwd" | "@ls" => {
            let mut out = String::new();
            for entry in std::fs::read_dir(cwd).into_iter().flatten().flatten() {
                let p = entry.path();
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                if name.starts_with('.') { continue; }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                out.push_str(&format!("- {}{}\n", name, if is_dir { "/" } else { "" }));
            }
            Some(("@cwd".into(), if out.is_empty() { "(empty)".into() } else { out }))
        }
        s if s == "@tree" || s.starts_with("@tree:") => {
            // `@tree` / `@tree:N` — Continue-style directory-tree context
            // provider. Renders an indented, ignore-aware tree of the active
            // project (depth 2 by default, `@tree:N` tunes it 1..=6) so the
            // model sees the project's *layout* — distinct from `@cwd` (a flat
            // single-level listing) and `@repomap` (symbol-ranked, not
            // structural). Deterministic ordering (dirs first, then files,
            // each alphabetical) keeps the output stable and unit-testable.
            let depth: usize = s
                .strip_prefix("@tree:")
                .and_then(|t| t.parse::<usize>().ok())
                .unwrap_or(2)
                .clamp(1, 6);
            Some(("@tree".into(), build_tree(cwd, depth, 400)))
        }
        s if s.starts_with("@outline:") => {
            // `@outline:<relpath>` — Zed-style file outline: the symbol
            // structure (functions/classes/headings + line numbers +
            // signatures) of a single named file. Distinct from `@repomap`,
            // which ranks *files* across the project; this is the in-file
            // table of contents of the one file the user points at. Path is
            // confined under the project root by `build_outline`.
            let rel = s.strip_prefix("@outline:")?;
            let body = crate::repo_map::build_outline(cwd, rel, 16 * 1024)?;
            Some((format!("@outline:{rel}"), body))
        }
        s if s.starts_with("@folder:") || s.starts_with("@dir:") => {
            // `@folder:<relpath>` (alias `@dir:`) — Continue.dev's folder
            // provider: inline the text files directly inside one folder (one
            // level, not recursive) so the model can read a whole module at
            // once. Distinct from `@file` (a single file), `@tree` (layout
            // only), and `@repomap` (ranked overview). Path-confined under the
            // project root by `build_folder`; output is bounded per-file and
            // overall so a fat folder can't blow the context window.
            let rel = s.splitn(2, ':').nth(1).unwrap_or("").trim();
            if rel.is_empty() {
                return None;
            }
            let body = crate::repo_map::build_folder(cwd, rel, 60 * 1024)?;
            Some((format!("@folder:{rel}"), body))
        }
        s if s.starts_with("@def:") || s.starts_with("@symbol:") => {
            // `@def:<symbol>` (alias `@symbol:`) — "go to definition" as
            // context. Resolves a symbol *name* to its declaration site(s)
            // across the project and injects the code, so the model can read
            // what a named function/type actually does. Distinct from
            // `@outline` (one file's structure), `@grep` (literal text), and
            // `@repomap` (ranked file overview): this jumps to a symbol's
            // definition and shows its body. Project-scoped via the walk in
            // `find_definition`.
            let name = s.splitn(2, ':').nth(1).unwrap_or("").trim();
            if name.is_empty() {
                return None;
            }
            let body = crate::repo_map::find_definition(cwd, name, 16 * 1024)?;
            Some((format!("@def:{name}"), body))
        }
        s if s.starts_with("@refs:")
            || s.starts_with("@callers:")
            || s.starts_with("@uses:") =>
        {
            // `@refs:<symbol>` (aliases `@callers:`/`@uses:`) — Zed's "Find All
            // References": every place a symbol is *used* across the project,
            // the companion to `@def` (where it's declared). Matching is
            // whole-word + case-sensitive on the identifier, which distinguishes
            // it from `@grep` (literal, case-insensitive substring — would match
            // `category` for `cat`) and from `@def` (declaration only).
            // Project-scoped via the walk in `find_references`.
            let name = s.splitn(2, ':').nth(1).unwrap_or("").trim();
            if name.is_empty() {
                return None;
            }
            let body = crate::repo_map::find_references(cwd, name, 16 * 1024)?;
            Some((format!("@refs:{name}"), body))
        }
        s if s.starts_with("@grep:") => {
            // `@grep:<pattern>` — case-insensitive recursive search for
            // `<pattern>` across project source files. Returns up to 50
            // `path:line: snippet` rows so the model can ask to read any
            // specific file. Skips noise dirs and huge files. 5s ceiling.
            let pattern = match s.strip_prefix("@grep:") {
                Some(p) if !p.is_empty() => p.to_lowercase(),
                _ => return None,
            };
            use walkdir::WalkDir;
            const SKIP: &[&str] = &[".git","node_modules","target","dist","build",".next",".cache","out"];
            const EXTS: &[&str] = &["rs","ts","tsx","js","jsx","py","md","go","c","h","cpp","hpp","rb","css","html","yaml","yml","toml","sh"];
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut hits: Vec<String> = Vec::new();
            'outer: for entry in WalkDir::new(cwd).max_depth(6).into_iter().filter_entry(|e| e.file_name().to_str().map(|n| !SKIP.contains(&n)).unwrap_or(true)) {
                if std::time::Instant::now() > deadline { break }
                let Ok(entry) = entry else { continue };
                if !entry.file_type().is_file() { continue }
                let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else { continue };
                if !EXTS.contains(&ext) { continue }
                let Ok(content) = std::fs::read_to_string(entry.path()) else { continue };
                if content.len() > 200_000 { continue }
                let lower = content.to_lowercase();
                if !lower.contains(&pattern) { continue }
                let rel = entry.path().strip_prefix(cwd).unwrap_or(entry.path());
                for (i, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&pattern) {
                        let snippet: String = line.trim().chars().take(120).collect();
                        hits.push(format!("{}:{}: {}", rel.display(), i + 1, snippet));
                        if hits.len() >= 50 { break 'outer; }
                    }
                }
            }
            let body = if hits.is_empty() { format!("(no matches for {})", pattern) } else { hits.join("\n") };
            Some((format!("@grep:{pattern}"), body))
        }
        s if s.starts_with("@blame:") => {
            // `@blame:<relpath>` — `git blame --line-porcelain` simplified
            // to `sha author line` rows. Caps file at 800 lines.
            let rel = s.strip_prefix("@blame:")?;
            // Refuse anything that escapes the cwd.
            if rel.contains("..") { return None; }
            let full = cwd.join(rel);
            if !full.is_file() { return None; }
            let out = crate::sys::no_window("git")
                .args(["blame", "--porcelain", "-L", "1,800", "--"])
                .arg(rel)
                .current_dir(cwd)
                .output()
                .ok()?;
            if !out.status.success() { return None; }
            let raw = String::from_utf8_lossy(&out.stdout);
            let mut lines: Vec<String> = Vec::new();
            let mut sha = String::new();
            let mut author = String::new();
            for ln in raw.lines() {
                if ln.starts_with('\t') {
                    lines.push(format!("{} {:18} | {}", &sha[..sha.len().min(8)], author.chars().take(18).collect::<String>(), ln.trim_start()));
                    if lines.len() > 400 { break }
                } else if ln.starts_with("author ") {
                    author = ln[7..].to_string();
                } else if let Some(s) = ln.split_whitespace().next() {
                    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) { sha = s.to_string(); }
                }
            }
            return Some((format!("@blame:{rel}"), lines.join("\n")));
        }
        s if s.starts_with("@log:") || s == "@log" => {
            let n: usize = s.strip_prefix("@log:")
                .and_then(|t| t.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 200);
            let out = crate::sys::no_window("git")
                .args(["log", "--oneline", "--no-color", "-n"])
                .arg(n.to_string())
                .current_dir(cwd)
                .output()
                .ok()?;
            if !out.status.success() { return None; }
            let s = String::from_utf8_lossy(&out.stdout).into_owned();
            return Some((format!("@log:{n}"), if s.trim().is_empty() { "(no commits)".into() } else { s }));
        }
        "@env" => {
            let mut out = format!("project_root: {}\n", cwd.display());
            if let Ok(head) = crate::sys::no_window("git").arg("rev-parse").arg("HEAD").current_dir(cwd).output() {
                if head.status.success() {
                    out.push_str(&format!("git_head: {}\n", String::from_utf8_lossy(&head.stdout).trim()));
                }
            }
            if let Ok(branch) = crate::sys::no_window("git").arg("symbolic-ref").arg("--short").arg("HEAD").current_dir(cwd).output() {
                if branch.status.success() {
                    out.push_str(&format!("branch: {}\n", String::from_utf8_lossy(&branch.stdout).trim()));
                }
            }
            Some(("@env".into(), out))
        }
        s if s.starts_with("@recent:") || s == "@recent" => {
            // Tunable count via `@recent:N`. Plain `@recent` defaults to 8.
            // Clamped to 1..=50 so the model context stays bounded.
            let n: usize = s.strip_prefix("@recent:")
                .and_then(|t| t.parse::<usize>().ok())
                .unwrap_or(8)
                .clamp(1, 50);
            use walkdir::WalkDir;
            const SKIP: &[&str] = &[".git","node_modules","target","dist","build",".next",".turbo",".cache","out","coverage","__pycache__"];
            const EXTS: &[&str] = &["rs","ts","tsx","js","jsx","py","md","go","java","c","h","cpp","hpp","rb","css","html","yaml","yml","toml","sh"];
            let mut entries: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
            for e in WalkDir::new(cwd).max_depth(5).into_iter().filter_entry(|e| {
                e.file_name().to_str().map(|s| !SKIP.contains(&s)).unwrap_or(true)
            }) {
                let Ok(e) = e else { continue };
                if !e.file_type().is_file() { continue }
                let Some(ext) = e.path().extension().and_then(|x| x.to_str()) else { continue };
                if !EXTS.contains(&ext) { continue }
                let Ok(m) = e.metadata() else { continue };
                let Ok(t) = m.modified() else { continue };
                entries.push((t, e.path().to_path_buf()));
            }
            entries.sort_by(|a, b| b.0.cmp(&a.0));
            entries.truncate(n);
            let mut out = String::new();
            for (t, p) in &entries {
                let age = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
                let ago = if age < 60 { format!("{}s ago", age) }
                    else if age < 3600 { format!("{}m ago", age / 60) }
                    else if age < 86400 { format!("{}h ago", age / 3600) }
                    else { format!("{}d ago", age / 86400) };
                let rel = p.strip_prefix(cwd).unwrap_or(p);
                out.push_str(&format!("- {} ({})\n", rel.display(), ago));
            }
            Some(("@recent".into(), if out.is_empty() { "(no recent edits)".into() } else { out }))
        }
        _ => None,
    }
}

/// `@terminal` / `@terminal:N` — Continue-style terminal context provider.
/// Resolves the tail of the recorded shell-output log under `home`
/// (`<home>/.cortex/last-shell-output.log`) so the model can see what just ran
/// in the user's terminal. Plain `@terminal` returns the last 200 lines;
/// `@terminal:N` tunes the count (clamped 1..=1000). Unlike `@diff`/`@status`
/// this is *not* project-scoped — terminal output is global — so it resolves
/// even without a `project_root`. Returns `(label, body)` or `None` when the
/// token isn't a terminal token or the log is absent/empty. Factored out so the
/// resolution is unit-testable against a temp `home` with no env mutation.
fn resolve_terminal_token(stripped: &str, home: &std::path::Path) -> Option<(String, String)> {
    if stripped != "@terminal" && !stripped.starts_with("@terminal:") {
        return None;
    }
    let n: usize = stripped
        .strip_prefix("@terminal:")
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let tail = crate::commands::context::read_terminal_tail(home, n)?;
    Some(("@terminal".to_string(), tail))
}

/// `@codebase` / `@codebase:N` — Continue-style semantic codebase retrieval.
/// Runs the blended retrieval pipeline (`retrieval::retrieve_blended`) over the
/// active project, using the rest of the user's message (with `@`-tokens
/// stripped) as the query, and injects the top-ranked hits — `path [source]`
/// plus a one-line snippet — so the model gets semantically-relevant code
/// without the user manually grepping. Plain `@codebase` returns the top 12
/// hits; `@codebase:N` tunes the count (clamped 1..=50).
///
/// Distinct from the sibling providers: `@grep:<pat>` matches a *literal*
/// pattern the user types, `@repomap` emits the *whole-project* symbol map, and
/// this ranks the codebase by the *query*. Project-scoped — needs a dir `root`.
/// Returns `(label, body)` or `None` when the token isn't a codebase token, the
/// root isn't a directory, or the query is empty. Factored out so the ranking +
/// formatting is unit-testable against a temp repo with no env mutation.
fn resolve_codebase_token(
    stripped: &str,
    query: &str,
    root: &std::path::Path,
) -> Option<(String, String)> {
    if stripped != "@codebase" && !stripped.starts_with("@codebase:") {
        return None;
    }
    if !root.is_dir() {
        return None;
    }
    let n: usize = stripped
        .strip_prefix("@codebase:")
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(12)
        .clamp(1, 50);
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let hits = crate::retrieval::retrieve_blended(root, query, n);
    if hits.is_empty() {
        return Some(("@codebase".to_string(), "(no relevant code found)".to_string()));
    }
    let mut body = String::new();
    for h in &hits {
        // Collapse the snippet to a single line so each hit is one compact
        // row the model can scan; cap to keep the block bounded.
        let snippet: String = h
            .snippet
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(160)
            .collect();
        body.push_str(&format!("{} [{}] (score {:.2})\n", h.path, h.source, h.score));
        if !snippet.is_empty() {
            body.push_str(&format!("  {snippet}\n"));
        }
    }
    Some(("@codebase".to_string(), body))
}

/// One ranked-and-injectable chunk of project documentation: the file it came
/// from, the nearest markdown heading above it (empty for the file's preamble),
/// and the section body text (used for both scoring and injection).
struct DocSection {
    path: String,
    heading: String,
    text: String,
}

/// Lowercase alphanumeric query terms (length ≥ 3, deduped, capped) used to rank
/// documentation sections. Mirrors the lightweight tokenisation the other
/// providers use — no stemming, just case-folded word matching.
fn doc_query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() < 3 {
            continue;
        }
        let t = raw.to_lowercase();
        if !terms.contains(&t) {
            terms.push(t);
        }
        if terms.len() >= 24 {
            break;
        }
    }
    terms
}

/// Walk `root` for prose documentation files (markdown / rST / text), splitting
/// each into sections at markdown ATX headings (`#`..`######`). The text before
/// the first heading becomes a preamble section (empty heading). Skips the usual
/// build/vendor noise dirs and oversized files so the scan stays bounded.
fn collect_doc_sections(root: &std::path::Path) -> Vec<DocSection> {
    use walkdir::WalkDir;
    const SKIP: &[&str] = &[
        ".git", "node_modules", "target", "dist", "build", ".next", ".turbo", ".cache", "out",
        "coverage", "__pycache__",
    ];
    const EXTS: &[&str] = &["md", "mdx", "markdown", "rst", "txt"];
    let mut sections: Vec<DocSection> = Vec::new();
    for entry in WalkDir::new(root)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|n| !SKIP.contains(&n))
                .unwrap_or(true)
        })
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !EXTS.contains(&ext.to_lowercase().as_str()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if content.len() > 400_000 {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .display()
            .to_string();

        let mut heading = String::new();
        let mut buf = String::new();
        let flush = |sections: &mut Vec<DocSection>, rel: &str, heading: &str, buf: &str| {
            if buf.trim().is_empty() {
                return;
            }
            sections.push(DocSection {
                path: rel.to_string(),
                heading: heading.to_string(),
                text: buf.trim().to_string(),
            });
        };
        for line in content.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('#') {
                // ATX heading: 1..=6 leading '#', then whitespace, then text.
                let hashes = 1 + rest.chars().take_while(|&c| c == '#').count();
                let title = rest.trim_start_matches('#');
                if hashes <= 6 && title.starts_with(char::is_whitespace) {
                    flush(&mut sections, &rel, &heading, &buf);
                    buf.clear();
                    heading = title.trim().to_string();
                    continue;
                }
            }
            buf.push_str(line);
            buf.push('\n');
        }
        flush(&mut sections, &rel, &heading, &buf);
    }
    sections
}

/// Term-frequency score for a documentation section against the query terms.
/// A term appearing in the section's heading is weighted heavily (×5) over a
/// body mention, so a section *about* the query ranks above one that merely
/// name-drops it. Returns 0.0 when no term matches.
fn score_doc_section(sec: &DocSection, terms: &[String]) -> f32 {
    let body = sec.text.to_lowercase();
    let head = sec.heading.to_lowercase();
    let mut score = 0.0_f32;
    for t in terms {
        let tf = body.matches(t.as_str()).count();
        score += tf as f32;
        if head.contains(t.as_str()) {
            score += 5.0;
        }
    }
    score
}

/// `@docs` / `@docs:N` — Continue-style documentation retrieval. Ranks the
/// active project's prose documentation (markdown / rST / text) by the rest of
/// the user's message and injects the most relevant *sections* — each as its
/// `path#heading` location plus the section body (per-section and total capped)
/// — so the model can actually read the project's own docs without the user
/// hunting for the right file. Plain `@docs` returns the top 6 sections;
/// `@docs:N` tunes the count (clamped 1..=30).
///
/// Distinct from the sibling providers: `@codebase` ranks *code* via blended
/// retrieval, `@grep:<pat>` matches a literal pattern, `@repomap` emits the
/// symbol map — this ranks *prose documentation* by the query. Project-scoped —
/// needs a dir `root`. Returns `(label, body)` or `None` when the token isn't a
/// docs token, the root isn't a directory, or the query has no usable terms.
/// Factored out so the section parsing + ranking is unit-testable against a temp
/// repo with no env mutation.
fn resolve_docs_token(
    stripped: &str,
    query: &str,
    root: &std::path::Path,
) -> Option<(String, String)> {
    if stripped != "@docs" && !stripped.starts_with("@docs:") {
        return None;
    }
    if !root.is_dir() {
        return None;
    }
    let n: usize = stripped
        .strip_prefix("@docs:")
        .and_then(|t| t.parse::<usize>().ok())
        .unwrap_or(6)
        .clamp(1, 30);
    let terms = doc_query_terms(query);
    if terms.is_empty() {
        return None;
    }
    let sections = collect_doc_sections(root);
    let mut scored: Vec<(f32, DocSection)> = sections
        .into_iter()
        .map(|s| (score_doc_section(&s, &terms), s))
        .filter(|(sc, _)| *sc > 0.0)
        .collect();
    if scored.is_empty() {
        return Some(("@docs".to_string(), "(no relevant docs found)".to_string()));
    }
    // Highest score first; ties broken by the shorter (more focused) section so
    // a tight matching paragraph outranks a sprawling one with the same score.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.text.len().cmp(&b.1.text.len()))
    });
    scored.truncate(n);

    const PER_SECTION_CAP: usize = 900;
    const TOTAL_CAP: usize = 8_000;
    let mut body = String::new();
    for (sc, sec) in &scored {
        let loc = if sec.heading.is_empty() {
            sec.path.clone()
        } else {
            format!("{}#{}", sec.path, sec.heading)
        };
        let chunk: String = sec.text.chars().take(PER_SECTION_CAP).collect();
        let ellipsis = if sec.text.chars().count() > PER_SECTION_CAP {
            " …"
        } else {
            ""
        };
        body.push_str(&format!("## {loc} (score {sc:.2})\n{chunk}{ellipsis}\n\n"));
        if body.len() >= TOTAL_CAP {
            break;
        }
    }
    Some(("@docs".to_string(), body.trim_end().to_string()))
}

/// Resolve `@problems` (Continue-style compiler diagnostics) for `root` into a
/// `(label, body)` pair. Runs the project's check-only compilers via
/// [`crate::projects::diagnostics::collect`] (cached 30s), orders errors before
/// warnings before notes, and renders one `[severity] path:line — message` row
/// per diagnostic under a count header (16 KiB / `+N more` capped). The body is
/// **always non-empty** — a "no project detected" or "no problems" signal when
/// there's nothing to list — so the model gets a definite answer (and can't
/// hallucinate errors) rather than a silently-dropped token.
fn resolve_problems_token(root: &std::path::Path) -> (String, String) {
    const NAME: &str = "@problems";
    const CAP: usize = 16 * 1024;

    let has_rust = root.join("Cargo.toml").is_file();
    let has_ts = root.join("tsconfig.json").is_file();
    if !has_rust && !has_ts {
        return (
            NAME.to_string(),
            "(no Rust/TypeScript project detected here — @problems runs `cargo check` / `tsc --noEmit`)".to_string(),
        );
    }

    let tools = match (has_rust, has_ts) {
        (true, true) => "cargo check + tsc",
        (true, false) => "cargo check",
        (false, true) => "tsc",
        (false, false) => unreachable!(),
    };

    let mut diags = crate::projects::diagnostics::collect(root);
    if diags.is_empty() {
        return (
            NAME.to_string(),
            format!("No problems — {tools} report no errors or warnings."),
        );
    }

    // Errors first, then warnings, then everything else (notes / cargo
    // failure-notes); stable within a severity so original order is preserved.
    let rank = |s: &str| match s {
        "error" => 0u8,
        "warning" => 1,
        _ => 2,
    };
    diags.sort_by_key(|d| rank(&d.severity));
    let errs = diags.iter().filter(|d| d.severity == "error").count();
    let warns = diags.iter().filter(|d| d.severity == "warning").count();

    let mut out = format!(
        "{} problem(s) from {tools}: {errs} error(s), {warns} warning(s)\n\n",
        diags.len()
    );
    let mut shown = 0usize;
    for d in &diags {
        let loc = if d.line > 0 {
            format!("{}:{}", d.path, d.line)
        } else if d.path.is_empty() {
            "<unknown>".to_string()
        } else {
            d.path.clone()
        };
        let msg = d.message.replace('\n', " ");
        let row = format!("[{}] {loc} — {msg}\n", d.severity);
        if out.len() + row.len() > CAP {
            out.push_str(&format!("… (+{} more, truncated)\n", diags.len() - shown));
            break;
        }
        out.push_str(&row);
        shown += 1;
    }
    (NAME.to_string(), out.trim_end().to_string())
}

/// Split an optional `:L<start>[-L<end>]` suffix off a mention path. This is
/// the syntax the editor's "add selection to chat" emits
/// (`@file:/abs/path.rs:L10-L24`), distinct from the hint-only `:line[:col]`
/// suffix (wave 133) which deliberately does NOT slice — only the explicit
/// `L` form narrows the attachment. Returns the bare path plus the inclusive
/// 1-based range. Malformed specs (L0, reversed, non-numeric) leave the
/// string untouched so resolution falls through gracefully.
fn split_line_range(path_str: &str) -> (&str, Option<(usize, usize)>) {
    let Some((head, tail)) = path_str.rsplit_once(':') else {
        return (path_str, None);
    };
    let Some(spec) = tail.strip_prefix('L') else {
        return (path_str, None);
    };
    let parse = |s: &str| s.parse::<usize>().ok().filter(|n| *n >= 1);
    let range = if let Some((a, b)) = spec.split_once('-') {
        let b = b.strip_prefix('L').unwrap_or(b);
        match (parse(a), parse(b)) {
            (Some(a), Some(b)) if a <= b => Some((a, b)),
            _ => None,
        }
    } else {
        parse(spec).map(|n| (n, n))
    };
    match range {
        Some(r) => (head, Some(r)),
        None => (path_str, None),
    }
}

/// 1-based inclusive line slice, clamped to EOF (past-EOF ranges → empty).
fn slice_lines(content: &str, start: usize, end: usize) -> String {
    content
        .lines()
        .skip(start - 1)
        .take(end - start + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Expand `@memory:<path>`, `@file:<path>`, and bare `@<absolute-path>`
/// tokens in the user's message by appending the referenced file contents
/// as fenced attachment blocks. Tokens that don't resolve (missing file,
/// > 200KB, non-text) are left as-is so the model still sees the intent.
/// Also recognises `@diff` and `@status` — see `resolve_special_token`.
///
/// Format (outer fence is 4 backticks so the nested attachment fence stays
/// literal — a bare inner ``` would end the block and open a stray rustdoc
/// test on whatever doc text follows):
///
/// ````text
/// <original message>
///
/// <!-- attached: @memory:/path/to/file.md (4.2KB) -->
/// ```md
/// <file contents>
/// ```
/// ````
fn expand_at_tokens(message: &str, project_root: Option<&std::path::Path>) -> (String, Vec<String>) {
    // Order of passes inside this function:
    //   1. `@brain` magic — full-message search for top-N brain hits
    //   2. Implicit path mentions (wave 118+) — Aider-style. Token must
    //      contain `/` or `\`, have a known code/.md ext (after stripping
    //      `:line` suffix), resolve under `project_root`, and be under
    //      MAX_FILE_BYTES. Capped at 3.
    //   3. Explicit `@`-tokens — `@diff`, `@status`, `@recent`, `@<abs>`,
    //      `@memory:`, `@file:`, `@frag:`, `@web:`, `@grep:`, etc.
    // Anything not matched falls through and ships to the gateway verbatim.
    const MAX_FILE_BYTES: u64 = 200 * 1024;
    let mut attachments: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // `@brain` magic token — run the local brain on the entire message and
    // auto-attach the top 3 hits. Single-token equivalent of clicking the
    // brain chips manually. Surface as a "📎 brain auto-attached:" header
    // followed by each file's content so the model can see what context
    // was selected for it.
    // `@brain` defaults to top 3; `@brain:N` for N hits (clamped 1..=10).
    let brain_n: Option<usize> = message
        .split_whitespace()
        .find_map(|t| {
            let t = t.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == ')');
            if t == "@brain" { return Some(3); }
            t.strip_prefix("@brain:")
                .and_then(|s| s.parse::<usize>().ok())
                .map(|n| n.clamp(1, 10))
        });
    if let Some(top_n) = brain_n {
        let cleaned: String = message
            .split_whitespace()
            .filter(|t| !t.starts_with("@brain"))
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(picks) = run_brain_inline(&cleaned, project_root) {
            for (path, content, kb) in picks.into_iter().take(top_n) {
                let key = path.display().to_string();
                if !seen.insert(key.clone()) { continue }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("text").to_lowercase();
                attachments.push(format!(
                    "<!-- attached: @brain/{} ({:.1}KB) -->\n```{}\n{}\n```",
                    path.display(), kb, ext, content,
                ));
                let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
                labels.push(format!("@brain/{name}"));
            }
        }
    }

    // Wave 118 — Aider-style implicit path mentions. If the user writes
    // "fix src/auth.rs" or "see `tests/foo.rs:42`" without an explicit
    // `@file:`, scan for path-like tokens that resolve under project_root
    // and auto-attach up to 3. Skips anything already starting with `@`
    // (handled below), anything looking like a URL, and the explicit
    // `path:line` suffix is stripped for resolution but kept on the label.
    let mentioned_start = labels.len();
    if let Some(root) = project_root {
        let mut mentioned_count = 0usize;
        for raw in message.split(|c: char| c.is_whitespace() || c == '(' || c == '[' || c == '"' || c == '\'' || c == '`') {
            if mentioned_count >= 3 { break; }
            if raw.is_empty() || raw.starts_with('@') || raw.starts_with("http://") || raw.starts_with("https://") {
                continue;
            }
            let tok = raw.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == ')' || c == ']' || c == '"' || c == '\'' || c == '`' || c == ':');
            // Path-like: must contain a slash (forward OR backslash) and an
            // extension we recognize as code or markdown. Avoid false-positives
            // like domain.com/path. Reject if it starts with `-` (CLI flag),
            // `/` (absolute), or `\\` (UNC/absolute) — `@file:` handles those.
            if !tok.contains('/') && !tok.contains('\\') { continue; }
            if tok.starts_with('-') || tok.starts_with('/') || tok.starts_with('\\') { continue; }
            // Normalize backslash → forward slash so `candidate.join` works
            // on both WSL and native Windows-style invocations.
            let path_norm = tok.replace('\\', "/");
            // Strip an optional `:line[:col]` suffix BEFORE checking the
            // extension. Without this, `src/auth.rs:42` looks like it ends
            // with `:42`, not `.rs`, and falls through.
            let path_no_line = tok.split(':').next().unwrap_or(tok);
            let lower = path_no_line.to_lowercase();
            let known_ext = [
                ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".kt",
                ".c", ".cc", ".cpp", ".h", ".hpp", ".rb", ".php", ".swift", ".scala",
                ".md", ".toml", ".yaml", ".yml", ".json", ".css", ".scss", ".html",
                ".sh", ".sql", ".proto", ".gradle",
                // Wave 153 — modern-ecosystem coverage.
                ".zig", ".dart", ".elm", ".json5", ".lua", ".nix", ".tf", ".mjs", ".cjs",
                ".astro", ".vue", ".svelte", ".jl", ".ex", ".exs", ".clj", ".hs", ".ml",
            ];
            if !known_ext.iter().any(|e| lower.ends_with(e)) { continue; }
            // Strip optional :line / :line:col suffix when resolving. Use
            // the normalized path (forward slashes) for filesystem lookup;
            // keep the original for the label so the user sees what they
            // actually typed.
            let path_only = path_norm.split(':').next().unwrap_or(&path_norm);
            let candidate = root.join(path_only);
            if !candidate.is_file() { continue; }
            // Respect the file-size cap.
            let too_big = std::fs::metadata(&candidate)
                .map(|m| m.len() > MAX_FILE_BYTES)
                .unwrap_or(true);
            if too_big {
                // Wave 155 — log when a valid mention is skipped because the
                // file is over the 200KB cap. Without this, users wondering
                // "why didn't my mention attach?" have no signal.
                tracing::info!(
                    target: "cortex::chat",
                    path = %candidate.display(),
                    cap_bytes = MAX_FILE_BYTES,
                    "implicit mention skipped — file over MAX_FILE_BYTES"
                );
                continue;
            }
            let key = format!("mention:{}", candidate.display());
            if !seen.insert(key) { continue; }
            let Ok(content) = std::fs::read_to_string(&candidate) else { continue };
            let ext = candidate.extension().and_then(|e| e.to_str()).unwrap_or("text").to_lowercase();
            // Wave 133 — preserve any `:line` or `:line:col` suffix the user
            // typed so the label + attached-block comment carry it (helps the
            // model focus). The actual inlined content is still the whole
            // file — `:line` is hint-only.
            let display = if path_norm != path_only {
                &path_norm
            } else {
                path_only
            };
            attachments.push(format!(
                "<!-- attached: mentioned {} -->\n```{}\n{}\n```",
                display, ext, content,
            ));
            labels.push(format!("mentioned/{display}"));
            mentioned_count += 1;
        }
    }
    // Wave 144 — telemetry. Log how many implicit mentions resolved so the
    // user-visible behavior is auditable from logs without needing to crack
    // open the chat transcript.
    let mentioned_total = labels.len() - mentioned_start;
    if mentioned_total > 0 {
        tracing::info!(
            target: "cortex::chat",
            mentioned = mentioned_total,
            "implicit path mentions auto-attached"
        );
    }

    for tok in message.split(|c: char| c.is_whitespace()) {
        if !tok.starts_with('@') { continue; }
        // Special tokens (@diff, @status) — handled by a separate resolver
        // because they don't refer to a path. Strip trailing punctuation
        // so "fix @diff." still triggers.
        let stripped = tok.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == ')');
        // Wave 189 — @repomap personalized PageRank. Pull identifier-like
        // tokens from the rest of the message so files defining/referencing
        // them rise in the rank. Falls through to the generic
        // resolve_special_token path when no project_root.
        if stripped == "@repomap" {
            if let Some(root) = project_root {
                let mentioned: Vec<String> = message
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|t| t.len() >= 4)
                    .filter(|t| {
                        t.contains('_')
                            || t.chars()
                                .zip(t.chars().skip(1))
                                .any(|(a, b)| a.is_lowercase() && b.is_uppercase())
                    })
                    .map(|t| t.to_string())
                    .take(12)
                    .collect();
                if !mentioned.is_empty() {
                    let map = crate::repo_map::compute_repo_map_personalized(root, 500, &mentioned);
                    let text = crate::repo_map::format_as_text(&map);
                    if !text.trim().is_empty() {
                        let name = "@repomap".to_string();
                        if seen.insert(name.clone()) {
                            attachments.push(format!(
                                "<!-- attached: {name} (personalized: {}) -->\n```text\n{}\n```",
                                mentioned.join(", "),
                                text.chars().take(60_000).collect::<String>(),
                            ));
                            labels.push(name);
                            continue;
                        }
                    }
                }
            }
        }
        // `@terminal` / `@terminal:N` — recorded shell-output tail. Resolved
        // before `resolve_special_token` because it isn't project-scoped (no
        // `cwd` guard) and reads the per-user log under $HOME.
        if stripped == "@terminal" || stripped.starts_with("@terminal:") {
            if seen.insert("@terminal".to_string()) {
                if let Some(home) = dirs::home_dir() {
                    if let Some((name, body)) = resolve_terminal_token(stripped, &home) {
                        let kb = (body.len() as f32) / 1024.0;
                        attachments.push(format!(
                            "<!-- attached: {name} ({kb:.1}KB) -->\n```text\n{body}\n```",
                        ));
                        labels.push(name);
                    }
                }
            }
            continue;
        }
        // `@codebase` / `@codebase:N` — Continue-style semantic retrieval over
        // the active project, ranked by the rest of the message. Resolved here
        // (not via `resolve_special_token`) because it needs the message text
        // as the query and is project-scoped.
        if stripped == "@codebase" || stripped.starts_with("@codebase:") {
            if seen.insert("@codebase".to_string()) {
                if let Some(root) = project_root {
                    // Query = the message minus every `@`-token, so the
                    // provider names don't pollute the retrieval terms.
                    let cb_query: String = message
                        .split_whitespace()
                        .filter(|t| !t.starts_with('@'))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Some((name, body)) = resolve_codebase_token(stripped, &cb_query, root) {
                        let kb = (body.len() as f32) / 1024.0;
                        attachments.push(format!(
                            "<!-- attached: {name} ({kb:.1}KB) -->\n```text\n{body}\n```",
                        ));
                        labels.push(name);
                    }
                }
            }
            continue;
        }
        // `@docs` / `@docs:N` — Continue-style documentation retrieval over the
        // active project's prose docs, ranked by the rest of the message.
        // Resolved here (not via `resolve_special_token`) because it needs the
        // message text as the query and is project-scoped — same shape as
        // `@codebase`. Note the frontend `docs` picker inserts note *paths*
        // (`@memory:`/`@file:` envelopes), so a bare `@docs` only ever reaches
        // here as the retrieval provider — no collision.
        if stripped == "@docs" || stripped.starts_with("@docs:") {
            if seen.insert("@docs".to_string()) {
                if let Some(root) = project_root {
                    let docs_query: String = message
                        .split_whitespace()
                        .filter(|t| !t.starts_with('@'))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if let Some((name, body)) = resolve_docs_token(stripped, &docs_query, root) {
                        let kb = (body.len() as f32) / 1024.0;
                        attachments.push(format!(
                            "<!-- attached: {name} ({kb:.1}KB) -->\n```text\n{body}\n```",
                        ));
                        labels.push(name);
                    }
                }
            }
            continue;
        }
        // `@problems` (aliases `@diagnostics` / `@lint`) — Continue-style
        // compiler diagnostics as context. Runs the project's check-only
        // compilers (`cargo check` / `tsc --noEmit`, cached 30s in
        // `projects::diagnostics`) and injects the current error/warning list
        // so the model can fix them. The frontend has long advertised this
        // provider (the `problems` picker kind + `errors`/`warnings`/`compile`
        // keywords), but the chat @-expansion never resolved a bare
        // `@problems` token — this closes that gap. Project-scoped: a root is
        // required to know which compilers to run. `collect` is synchronous
        // (thread-drained subprocess + 20s budget), so it fits this sync path.
        if stripped == "@problems" || stripped == "@diagnostics" || stripped == "@lint" {
            if seen.insert("@problems".to_string()) {
                if let Some(root) = project_root {
                    let (name, body) = resolve_problems_token(root);
                    let kb = (body.len() as f32) / 1024.0;
                    attachments.push(format!(
                        "<!-- attached: {name} ({kb:.1}KB) -->\n```text\n{body}\n```",
                    ));
                    labels.push(name);
                }
            }
            continue;
        }
        if let Some((name, body)) = resolve_special_token(stripped, project_root) {
            if !seen.insert(name.clone()) { continue; }
            attachments.push(format!(
                "<!-- attached: {name} -->\n```diff\n{body}\n```",
            ));
            labels.push(name);
            continue;
        }
        let body = &tok[1..];
        let body = body.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == ')');
        // `@websearch:<query>` — run a live keyless web search and inline the
        // top ranked results (title · url · snippet). Continue.dev/Cursor's
        // `@web` *search* provider (distinct from `@web:<url>`, which fetches a
        // single known page). Provider-agnostic: results are injected as
        // context at send time, so any model — local Ollama included — gets
        // them without needing a tool channel. Aliases: `search:`/`google:`.
        // Best-effort; blocks the chat send for up to ~15s on the DDG fetch.
        if let Some(query) = body
            .strip_prefix("websearch:")
            .or_else(|| body.strip_prefix("search:"))
            .or_else(|| body.strip_prefix("google:"))
        {
            let query = query.trim();
            if query.is_empty() { continue; }
            let key = format!("websearch:{}", query.to_lowercase());
            if !seen.insert(key.clone()) { continue; }
            // Cap at 6 results so the block stays a scannable lead-list, not a
            // wall — the user can `@web:<url>` any hit to read it in full.
            let results = tauri::async_runtime::block_on(crate::websearch::search(query, 6))
                .unwrap_or_default();
            if results.is_empty() { continue; }
            let (attachment, label) = format_websearch_attachment(query, &results);
            attachments.push(attachment);
            labels.push(label);
            continue;
        }
        // `@web:<url>` — fetch URL, strip HTML, inline as text. Aider's
        // `/web` pattern. Best-effort; blocks the chat send for up to 8s.
        if let Some(url) = body.strip_prefix("web:") {
            if !url.starts_with("http://") && !url.starts_with("https://") { continue; }
            let key = format!("web:{}", url);
            if !seen.insert(key.clone()) { continue; }
            if let Some(text) = fetch_url_text(url) {
                let kb = (text.len() as f32) / 1024.0;
                attachments.push(format!(
                    "<!-- attached: @web:{url} ({kb:.1}KB) -->\n```text\n{text}\n```",
                ));
                labels.push(format!("@web:{}", short_url(url)));
            }
            continue;
        }

        // `@frag:<name>` — reusable prompt snippet from
        // `~/.cortex/fragments/<name>.md`. Codex/Cursor pattern. Sanitise
        // name to `[a-z0-9_-]+` so users can't path-traverse out of the
        // fragments dir.
        if let Some(name) = body.strip_prefix("frag:") {
            let safe: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            if safe.is_empty() { continue; }
            let Some(home) = dirs::home_dir() else { continue };
            let path = home.join(".cortex").join("fragments").join(format!("{safe}.md"));
            if !path.is_file() { continue; }
            let key = format!("frag:{safe}");
            if !seen.insert(key.clone()) { continue; }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            attachments.push(format!(
                "<!-- attached: @frag:{safe} -->\n```md\n{content}\n```",
            ));
            labels.push(format!("@frag:{safe}"));
            continue;
        }
        let path_str = if let Some(rest) = body.strip_prefix("memory:") { rest }
            else if let Some(rest) = body.strip_prefix("file:") { rest }
            else { body };
        // `@file:/abs/path.rs:L10-L24` — selection mentions from the editor's
        // "add selection to chat" attach ONLY the named lines.
        let (path_str, line_range) = split_line_range(path_str);
        let path = std::path::PathBuf::from(path_str);
        if !path.is_absolute() { continue; }
        let range_suffix = match line_range {
            Some((a, b)) if a == b => format!(":L{a}"),
            Some((a, b)) => format!(":L{a}-L{b}"),
            None => String::new(),
        };
        // Key includes the range so two different slices of one file can both
        // attach (a whole-file mention still dedups against itself).
        let key = format!("{}{}", path.display(), range_suffix);
        if !seen.insert(key.clone()) { continue; }
        let Ok(meta) = std::fs::metadata(&path) else { continue; };
        if !meta.is_file() { continue; }
        if meta.len() > MAX_FILE_BYTES { continue; }
        let Ok(content) = std::fs::read_to_string(&path) else { continue; };
        let content = match line_range {
            Some((a, b)) => slice_lines(&content, a, b),
            None => content,
        };
        // A range entirely past EOF slices to nothing — leave the token
        // unresolved (the model still sees the user's intent) instead of
        // attaching an empty block.
        if line_range.is_some() && content.is_empty() { continue; }
        let kb = (content.len() as f32) / 1024.0;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let fence = if ext.is_empty() { "text".to_string() } else { ext };
        attachments.push(format!(
            "<!-- attached: {tok} ({kb:.1}KB) -->\n```{fence}\n{content}\n```",
        ));
        // Use the basename in the label so the toast stays compact;
        // tooltip on the toast can show the full path.
        let base = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(key.clone());
        labels.push(format!("{base}{range_suffix}"));
    }
    if attachments.is_empty() { return (message.to_string(), labels); }
    (format!("{}\n\n{}", message, attachments.join("\n\n")), labels)
}

#[allow(dead_code)]
fn format_plan_prompt(plan: &str, user_message: &str) -> String {
    let plan = plan.trim();
    format!(
        "<plan>\n{plan}\n</plan>\n\nApply the plan above to satisfy this request:\n\n{user_message}"
    )
}

/// Process-wide last-known mode set from the UI. Used as a fallback when a
/// `chat_send` invocation forgets to thread `mode` through (e.g. legacy
/// callers, or background restart paths). Defaults to `"act"`.
static CURRENT_MODE: OnceCell<Mutex<String>> = OnceCell::new();

fn current_mode_cell() -> &'static Mutex<String> {
    CURRENT_MODE.get_or_init(|| Mutex::new("act".to_string()))
}

fn read_current_mode() -> String {
    current_mode_cell()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| "act".to_string())
}

/// Process-wide set of session ids we've already fired `SessionStart` for.
/// Backstops the "fire SessionStart on the first `chat_send` per session"
/// rule without needing per-session state in `AppState`.
static SEEN_SESSIONS: OnceCell<Mutex<HashSet<String>>> = OnceCell::new();

fn seen_sessions_cell() -> &'static Mutex<HashSet<String>> {
    SEEN_SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// True when this is the first time we've seen `session_id` in the
/// current process. Inserts on first call so the next call returns false.
fn is_new_session(session_id: &str) -> bool {
    let cell = seen_sessions_cell();
    match cell.lock() {
        Ok(mut g) => g.insert(session_id.to_string()),
        // Poisoned lock: treat as "already seen" so we don't double-fire.
        Err(_) => false,
    }
}

/// Fire a per-event lifecycle hook (PreToolUse / PostToolUse / Stop /
/// PermissionRequest / Notification) without blocking the agent stream.
///
/// These fire from inside the streamed agent event loop where stalling on a
/// slow hook would back up token delivery, so this is intentionally
/// fire-and-forget — exactly the SESSION_START contract: a block result is
/// logged for visibility but never aborts the run. Returns immediately if no
/// hook is registered for `event_name` so the hot path stays cheap.
fn fire_hook_detached(
    hooks: &std::sync::Arc<HooksConfig>,
    event_name: &'static str,
    payload: serde_json::Value,
) {
    if !hooks.has(event_name) {
        return;
    }
    let hooks = hooks.clone();
    tauri::async_runtime::spawn(async move {
        let r = hooks::fire_event(&hooks, event_name, &payload).await;
        if r.is_blocked() {
            tracing::info!(
                event = event_name,
                reason = r.block_reason().unwrap_or("<none>"),
                "lifecycle hook returned block (ignored, observational only)"
            );
        }
    });
}

/// Regex matching write/exec-style tool names that Plan mode blocks.
/// Mirrors the guardrails compile-once-and-reuse pattern.
fn plan_blocklist() -> &'static Regex {
    static RE: OnceCell<Regex> = OnceCell::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(write|edit|bash|shell|exec|patch|file_edit|run_)")
            .expect("plan-mode regex is valid")
    })
}

/// Returns Some(message) if this tool call should be blocked under the given
/// mode. Plan mode blocks anything matching the write/exec regex; both modes
/// always block High-risk guardrail hits.
fn maybe_block_tool_call(
    mode: &str,
    name: &str,
    args_json: &str,
    guardrails: &Guardrails,
) -> Option<String> {
    if let Some((Risk::High, reason)) = guardrails.evaluate(name, args_json) {
        return Some(format!(
            "guardrails: tool '{name}' blocked (high risk: {reason})"
        ));
    }
    if mode.eq_ignore_ascii_case("plan") && plan_blocklist().is_match(name) {
        return Some(format!("plan mode: tool '{name}' is blocked"));
    }
    None
}

/// Sandbox-tier gate. Runs *before* `maybe_block_tool_call` (which handles
/// guardrails + plan/act). Tier is deny-bias: a tier rejection wins over any
/// approval rule that would otherwise allow the call.
fn maybe_block_by_tier(
    tier: SandboxTier,
    name: &str,
    args_json: &str,
    project_root: Option<&Path>,
) -> Option<String> {
    match tier_allows(tier, name, args_json, project_root) {
        Ok(()) => None,
        Err(reason) => Some(format!("sandbox: blocked by tier — {reason}")),
    }
}

/// Deny-bias re-gate for an *auto-approve* of an `ApprovalRequest`. Returns
/// `Some(reason)` if the sandbox tier or guardrails forbid the action (so the
/// auto-approve must be suppressed and the request forwarded to the user), or
/// `None` if it may be silently approved.
///
/// This exists because the tier/guardrail gates in the agent event loop only
/// run on `AgentEvent::ToolCall`, whereas an auto-approve (from the explicit
/// allowlist or the `never`/`untrusted` approval policy) acts on the *distinct*
/// `AgentEvent::ApprovalRequest`. Without re-running the gates here, a trusted
/// project pinned to e.g. `ReadOnly` with `policy = "never"` would silently
/// approve a write/exec the tier forbids — violating the documented
/// "auto-approve ⊆ tier-allowed" invariant (sandbox is deny-bias). Mirrors the
/// ToolCall gate order exactly: tier first, then guardrails/plan-mode.
fn auto_approve_blocked_by_gates(
    tier: SandboxTier,
    mode: &str,
    tool_name: &str,
    payload_json: &str,
    guardrails: &Guardrails,
    project_root: Option<&Path>,
) -> Option<String> {
    maybe_block_by_tier(tier, tool_name, payload_json, project_root)
        .or_else(|| maybe_block_tool_call(mode, tool_name, payload_json, guardrails))
}

#[derive(Debug, Serialize)]
pub struct ChatSendResult {
    pub session_id: String,
    pub picked_agents: Vec<String>,
    pub routing_reason: String,
    /// Display labels for any @-tokens the backend resolved into inline
    /// attachments (e.g. `["foo.md","@diff","@brain/bar.rs"]`). Empty when
    /// the user's message had no resolvable tokens. The frontend uses this
    /// to push a toast so the user can see exactly what got attached.
    pub attachments: Vec<String>,
}

#[tauri::command]
pub async fn chat_send(
    args: ChatSendArgs,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ChatSendResult, String> {
    let session_id = args.session_id.clone();
    let project_root = args.project_root.as_ref().map(PathBuf::from);
    let mode = args
        .mode
        .clone()
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| m == "plan" || m == "act")
        .unwrap_or_else(read_current_mode);
    // Persist the latest mode for any callers that omit it.
    if let Ok(mut g) = current_mode_cell().lock() {
        *g = mode.clone();
    }
    // Trust gate (Codex-style explicit opt-in). For untrusted projects we:
    //   * Refuse to load `.cortex/danger.toml` — fall back to built-in
    //     guardrail defaults so a malicious repo can't whitelist patterns.
    //   * Force the sandbox tier to `ReadOnly` regardless of
    //     `.cortex/sandbox.toml`.
    // `.cortex/approvals.toml` loading lives downstream; the forced ReadOnly
    // tier makes write-class approvals moot anyway.
    let is_trusted = project_root
        .as_deref()
        .map(trust::is_trusted)
        .unwrap_or(false);
    let guardrails = std::sync::Arc::new(if is_trusted {
        project_root
            .as_deref()
            .map(Guardrails::load)
            .unwrap_or_else(Guardrails::defaults)
    } else {
        Guardrails::defaults()
    });
    let sandbox_tier = if is_trusted {
        project_root
            .as_deref()
            .map(load_tier)
            .unwrap_or_default()
    } else {
        SandboxTier::ReadOnly
    };
    // Approval policy (Codex-style "when do we pause to ask?" axis). For an
    // untrusted project we pin it to `OnRequest` (ask for everything) regardless
    // of any on-disk `.cortex/approval-policy.toml`, mirroring the forced
    // ReadOnly tier above — a malicious repo must not be able to silence the
    // approval prompts.
    let approval_policy = if is_trusted {
        project_root
            .as_deref()
            .map(load_policy)
            .unwrap_or_default()
    } else {
        ApprovalPolicy::OnRequest
    };
    let hooks_config = std::sync::Arc::new(
        project_root
            .as_deref()
            .map(HooksConfig::load)
            .unwrap_or_default(),
    );
    // Global auto-approve allowlist (~/.cortex/auto-approve.json). Loaded
    // once per chat_send so a freshly-added entry takes effect on the next
    // turn without restarting the app.
    let auto_approve = std::sync::Arc::new(AutoApproveList::load());

    // SessionStart: fire once per session id, before anything else.
    // Result is observational only — we don't block chat on it.
    if is_new_session(&session_id) && hooks_config.has(hook_events::SESSION_START) {
        let payload = serde_json::json!({
            "event": hook_events::SESSION_START,
            "session_id": session_id,
            "project_root": project_root,
        });
        let r = hooks::fire_event(&hooks_config, hook_events::SESSION_START, &payload).await;
        if r.is_blocked() {
            tracing::info!(
                session_id = %session_id,
                "hook SessionStart returned block (ignored, observational only)"
            );
        }
    }

    // UserPromptSubmit: blocking. If a hook rejects, surface the reason
    // to the UI as an error event and abort before dispatching agents.
    if hooks_config.has(hook_events::USER_PROMPT_SUBMIT) {
        let payload = serde_json::json!({
            "event": hook_events::USER_PROMPT_SUBMIT,
            "session_id": session_id,
            "project_root": project_root,
            "message": args.message,
            "mode": mode,
        });
        let r = hooks::fire_event(&hooks_config, hook_events::USER_PROMPT_SUBMIT, &payload).await;
        if r.is_blocked() {
            let reason = r
                .block_reason()
                .unwrap_or("blocked by UserPromptSubmit hook")
                .to_string();
            let _ = app.emit(
                &format!("agent-event:{}", session_id),
                serde_json::json!({
                    "type": "error",
                    "message": format!("hook: {reason}"),
                }),
            );
            return Err(format!("UserPromptSubmit hook blocked: {reason}"));
        }
    }

    // Wrap the user message in an `<images>` envelope when attachments are
    // present. Vision-aware adapters can lift the JSON blocks out; text-only
    // adapters see them as inline `<images>` tags (noisy but non-fatal — the
    // warning in `build_images_envelope` makes the upstream-compatibility
    // gap visible in `tracing`).
    // Massive-brain @-token expansion. The composer can splice tokens like
    // `@memory:/abs/path.md`, `@file:/abs/path`, or `@/abs/path` (when the
    // brain returns paths directly). Without expansion these reach the
    // model as opaque strings — the file contents never get attached.
    // Replace each token with an inline attachment block so the model
    // actually sees what the user pointed at.
    let (expanded, attachment_labels) = expand_at_tokens(&args.message, project_root.as_deref());
    // Wave 278 — single-line summary of expansion result for log scanning.
    if !attachment_labels.is_empty() {
        tracing::info!(
            target: "cortex::chat",
            count = attachment_labels.len(),
            labels = ?attachment_labels.iter().take(8).collect::<Vec<_>>(),
            "chat_send attachments expanded"
        );
    }
    // Prepend the project's rules/conventions file (AGENTS.md, else
    // .cortexrules, else CLAUDE.md) and a ranked repo-map (aider/Continue
    // -style auto-context) when a project is open — cross-tool standards that
    // teach the agent project conventions and let it locate relevant code
    // without blind searching. Additive: no project => unchanged message;
    // both blocks individually optional and byte-capped so they can't
    // dominate the context budget.
    let expanded = match project_root.as_deref() {
        Some(root) => {
            let prefix = build_context_prefix(root, &args.message);
            if prefix.is_empty() {
                expanded
            } else {
                format!("{prefix}{expanded}")
            }
        }
        None => expanded,
    };

    let effective_message = build_images_envelope(&args.images, &expanded)
        .unwrap_or(expanded);

    // Auto mode: when the user picked no model, choose one by task complexity
    // (deterministic, no network). The chosen reason is surfaced on the route
    // event so the UI can show what Auto picked. An explicit pick passes through
    // unchanged. The registry read guard is scoped tight so it's dropped before
    // the routing read below.
    // Continue.dev-style per-project model roles (`.cortex/model-roles.toml`):
    // a low-precedence default model per logical role (chat / planner / editor).
    // Loaded once and consulted below — an explicit pick or `/architect *_model=`
    // override always wins, so a project that never opts in is unaffected.
    let model_roles = project_root
        .as_deref()
        .map(crate::commands::model_roles::load_model_roles)
        .unwrap_or_default();

    let mut auto_reason: Option<String> = None;
    let mut effective_model: Option<String> = match args.model.clone() {
        // Canonicalize the picked model: a short alias (`opus`, `gpt5`) or a
        // mixed-case id resolves to the canonical id the adapter/gateway expects;
        // an unknown slug (Ollama, live-discovered gateway model) passes through
        // unchanged. This is the single point where the model reaching both
        // `route()` and the adapter is normalized.
        Some(m) if !m.trim().is_empty() => Some(orchestrator::aliases::resolve_model(&m)),
        _ => {
            // No explicit per-turn pick. Prefer the configured **chat** role
            // default (Continue.dev) over Auto-selection — it's the project's
            // pinned chat model. Still canonicalized through the alias catalog.
            match model_roles.chat.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(role_model) => {
                    let resolved = orchestrator::aliases::resolve_model(role_model);
                    auto_reason = Some(format!("chat-role default ({resolved})"));
                    Some(resolved)
                }
                None => {
                    let reg = state.registry.read();
                    match orchestrator::auto_select_model(&args.message, &args.history, &reg) {
                        Some((m, r)) => {
                            auto_reason = Some(r);
                            Some(m)
                        }
                        None => None,
                    }
                }
            }
        }
    };

    // Resolve the per-request reasoning effort: the composer's per-prompt pick
    // wins over the global config default; an unrecognized value falls through.
    let effective_effort = orchestrator::reasoning::resolve(
        args.reasoning_effort.as_deref(),
        state.config.read().reasoning_effort.as_deref(),
    );

    // Aider-style `/architect` two-phase split. When active, resolve the two
    // phase models and point routing + dispatch at the *editor* model — the
    // planner runs as a separate pre-step below. `planner_model_id` is read by
    // that pre-step; both fall back to the user's pick (then a built-in default)
    // so architect mode never silently swaps a model the user explicitly chose.
    let architect_active = orchestrator::architect::is_active(args.architect_mode);
    let planner_model_id = if architect_active {
        // Pass the *explicit* per-turn pick (`args.model`) — not the derived
        // `effective_model`, which may itself be an Auto-selected or configured
        // chat-role model — so a configured planner/editor role isn't shadowed by
        // the chat role. Precedence per phase: `/architect *_model=` override →
        // explicit pick → configured planner/editor role → built-in default.
        let explicit_pick = args.model.as_deref();
        let planner = orchestrator::architect::planner_model(
            args.planner_model.as_deref(),
            explicit_pick,
            model_roles.planner.as_deref(),
        );
        let editor = orchestrator::architect::editor_model(
            args.editor_model.as_deref(),
            explicit_pick,
            model_roles.editor.as_deref(),
        );
        effective_model = Some(editor);
        Some(planner)
    } else {
        None
    };

    let req = ChatRequest {
        session_id: session_id.clone(),
        message: effective_message.clone(),
        project_root: project_root.clone(),
        history: args.history.clone(),
        model: effective_model.clone(),
        reasoning_effort: effective_effort.clone(),
    };

    // Routing + adapter resolution happen inside a block so the non-`Send`
    // registry read guard is fully dropped before the architect planner
    // `.await` below (an explicit `drop()` isn't enough for the async Send
    // analysis — the guard must leave lexical scope).
    let (decision, picked, reason, agents, planner_adapter) = {
        let registry = state.registry.read();
        let decision = orchestrator::route(&req, &registry, args.agent.clone());
        let picked = decision.agents.clone();
        // When Auto picked a model, append its reason so the UI shows the choice.
        let reason = match &auto_reason {
            Some(auto) => format!("{} · {}", decision.reason, auto),
            None => decision.reason.clone(),
        };
        let agents = orchestrator::resolve(&decision, &registry);
        // Resolve the architect planner's adapter while we still hold the guard.
        // Route a bare request by the planner model — the same routing the editor
        // went through — and grab the registered adapter instance. `None` (no
        // matching/available adapter) makes the planner phase a no-op and we fall
        // back to a single-phase run.
        let planner_adapter = planner_model_id.as_deref().and_then(|m| {
            let preq = ChatRequest {
                session_id: String::new(),
                message: String::new(),
                project_root: None,
                history: Vec::new(),
                model: Some(m.to_string()),
                reasoning_effort: None,
            };
            let pdec = orchestrator::route(&preq, &registry, None);
            pdec.agents.into_iter().next().and_then(|id| registry.get(&id))
        });
        (decision, picked, reason, agents, planner_adapter)
    };

    if agents.is_empty() {
        return Err(format!("no agents available for routing: {}", decision.reason));
    }

    let _ = app.emit(
        &format!("agent-event:{}", session_id),
        serde_json::json!({
            "type": "orchestrator_route",
            "agents": picked,
            "reason": reason,
        }),
    );

    let trace_id = ulid::Ulid::new().to_string();
    if let Some(store) = app.try_state::<TracingStore>() {
        let _ = store.record_chat_turn(&trace_id, &session_id, &args.message, &picked);
    }

    let session_for_task = session_id.clone();
    let app_handle = app.clone();

    // Re-apply the `<images>` envelope to the orchestrator's stripped message
    // so dispatched agents still see attachments after `@`-mentions / kind
    // prefixes have been peeled off.
    let stripped_with_images =
        build_images_envelope(&args.images, &decision.stripped_message)
            .unwrap_or_else(|| decision.stripped_message.clone());

    // Architect phase 1 (planner). When active, run the planner model to
    // completion — streaming its plan into the editor agent's assistant bubble
    // so the turn reads as "plan, then edits" — then inject the plan into the
    // message the editor (phase 2, the normal dispatch below) receives. A
    // missing adapter / empty plan / timeout leaves `dispatched_message`
    // unchanged, degrading cleanly to a single-phase editor run.
    let stream_agent_id = picked.first().cloned().unwrap_or_else(|| "agent".to_string());
    let mut dispatched_message = stripped_with_images.clone();
    if architect_active {
        if let (Some(planner_id), Some(adapter)) =
            (planner_model_id.as_deref(), planner_adapter.clone())
        {
            emit_chat_token(
                &app,
                &session_id,
                &stream_agent_id,
                &format!("**🏛️ Planning** with `{planner_id}`…\n\n"),
            );
            let plan = run_planner_phase(
                adapter,
                planner_id.to_string(),
                orchestrator::architect::plan_instruction(&stripped_with_images),
                effective_effort.clone(),
                |delta| emit_chat_token(&app, &session_id, &stream_agent_id, delta),
            )
            .await;
            if let Some(plan) = plan {
                emit_chat_token(&app, &session_id, &stream_agent_id, "\n\n---\n\n");
                dispatched_message =
                    orchestrator::architect::inject_plan(&plan, &stripped_with_images);
            }
        }
    }

    // Focus-chain prompt contract (commands::focus_chain). Injected POST-
    // routing on purpose: its wording ("steps", "checklist") must not perturb
    // `auto_select_model`'s complexity keywords or the route itself. Every
    // adapter sees it, which is what lets the FocusChain panel populate
    // regardless of provider — the streamed reply is scanned for the fenced
    // checklist in the event loop below.
    let dispatched_message = format!(
        "{}\n{}",
        crate::commands::focus_chain::FOCUS_CHAIN_CONTRACT,
        dispatched_message
    );

    for agent in agents {
        let req_clone = ChatRequest {
            session_id: session_for_task.clone(),
            message: dispatched_message.clone(),
            project_root: project_root.clone(),
            history: args.history.clone(),
            model: effective_model.clone(),
            reasoning_effort: effective_effort.clone(),
        };
        let session = session_for_task.clone();
        let app_for_agent = app_handle.clone();
        // Captured separately so the agent.run span can record the effective
        // model: `req_clone` is moved into the run task below and can't be read
        // back here.
        let model_for_run = effective_model.clone();
        let trace_id = trace_id.clone();
        let mode_for_agent = mode.clone();
        let guardrails_for_agent = guardrails.clone();
        let tier_for_agent = sandbox_tier;
        let project_root_for_agent = project_root.clone();
        // Per-event hooks (PreToolUse / PostToolUse / Stop / PermissionRequest
        // / Notification) fire from inside the agent event loop below. They are
        // observational/fire-and-forget — matching the SESSION_START contract,
        // a slow or failing hook must never stall the streamed agent output, so
        // each fires in a detached task and any block result is logged only.
        let hooks_for_agent = hooks_config.clone();
        let auto_approve_for_agent = auto_approve.clone();
        let approval_policy_for_agent = approval_policy;
        let state_for_agent = state.inner().clone();

        tauri::async_runtime::spawn(async move {
            let agent_id = agent.descriptor().id.clone();
            let (tx, mut rx) = mpsc::channel(64);
            // Detects ```focus-chain checklists in the streamed text (the
            // prompt contract injected above) and replays them as the
            // synthetic `update_focus_chain` tool call — see the Token/Done
            // arm at the bottom of the loop.
            let mut focus_scanner = crate::commands::focus_chain::FocusChainScanner::new();

            let agent_for_run = agent.clone();
            let run_handle = tauri::async_runtime::spawn(async move {
                let _ = agent_for_run.run(req_clone, tx).await;
            });

            let span_id = ulid::Ulid::new().to_string();
            if let Some(store) = app_for_agent.try_state::<TracingStore>() {
                let _ = store.start_agent_run(
                    &span_id,
                    &trace_id,
                    &session,
                    &agent_id,
                    model_for_run.as_deref(),
                );
            }

            while let Some(evt) = rx.recv().await {
                // Sandbox tier (deny-bias) → guardrails/plan-mode filter:
                // rewrite blocked tool calls into Error events before they
                // reach the UI or trace store. Tier is checked FIRST so a
                // deny here overrides any approval rule that might allow.
                let evt = if let AgentEvent::ToolCall { name, args, .. } = &evt {
                    let args_json = serde_json::to_string(args).unwrap_or_default();
                    if let Some(msg) = maybe_block_by_tier(
                        tier_for_agent,
                        name,
                        &args_json,
                        project_root_for_agent.as_deref(),
                    ) {
                        AgentEvent::Error { message: msg }
                    } else if let Some(msg) = maybe_block_tool_call(
                        &mode_for_agent,
                        name,
                        &args_json,
                        &guardrails_for_agent,
                    ) {
                        AgentEvent::Error { message: msg }
                    } else {
                        evt
                    }
                } else {
                    evt
                };

                // Auto-approve allowlist short-circuit. When the agent emits an
                // `ApprovalRequest` whose `(tool, payload)` matches a saved
                // glob, we silently POST `approve_run(once)` back to the gateway and
                // never forward the event to the UI. Emits an
                // `ApprovalResolved` so the trace store still sees a paired
                // request/response.
                let evt = if let AgentEvent::ApprovalRequest {
                    run_id,
                    tool,
                    request,
                    ..
                } = &evt
                {
                    let tool_name = tool.as_deref().unwrap_or("");
                    let payload_json = serde_json::to_string(request).unwrap_or_default();
                    // An approval request can be resolved without prompting the
                    // user in two ways:
                    //   1. an explicit auto-approve allowlist entry matches, or
                    //   2. the project's approval policy auto-approves it
                    //      (`never` → everything that already passed; `untrusted`
                    //      → provably read-only only; `on-request` → never here).
                    let choice = if auto_approve_for_agent.matches(tool_name, request) {
                        Some("auto-approved")
                    } else if approval_policy_for_agent.auto_approves(
                        tool_name,
                        &payload_json,
                        project_root_for_agent.as_deref(),
                    ) {
                        Some("policy-approved")
                    } else {
                        None
                    };
                    // Deny-bias re-gate. The tier/guardrail gates earlier in
                    // this loop only fire on `ToolCall` events, but an
                    // `ApprovalRequest` is a *distinct* event — so an
                    // auto-approve here (allowlist OR `policy = "never"`) would
                    // otherwise skip the prompt for a write/exec the active tier
                    // forbids (e.g. a trusted project pinned to ReadOnly). Re-run
                    // the SAME gates (tier first, then guardrails/plan-mode) on
                    // the request payload so an auto-approve can only ever skip
                    // the prompt for an already-allowed action — upholding the
                    // documented "auto-approve ⊆ tier-allowed" invariant. A
                    // blocked action falls through to the normal user prompt (and
                    // its own ToolCall stays independently tier-blocked).
                    let choice = choice.filter(|_| {
                        match auto_approve_blocked_by_gates(
                            tier_for_agent,
                            &mode_for_agent,
                            tool_name,
                            &payload_json,
                            &guardrails_for_agent,
                            project_root_for_agent.as_deref(),
                        ) {
                            None => true,
                            Some(reason) => {
                                tracing::info!(
                                    "auto-approve suppressed for run {run_id} tool '{tool_name}': {reason} — forwarding approval request to user"
                                );
                                false
                            }
                        }
                    });
                    if let Some(choice) = choice {
                        let run_id_owned = run_id.clone();
                        let cfg = state_for_agent.config.read().clone();
                        let api_key = AppState::get_gateway_api_key().unwrap_or_default();
                        let client = crate::gateway::client::GatewayClient::new(
                            cfg.gateway_base_url,
                            api_key,
                        );
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = client
                                .approve_run(&run_id_owned, "once", None, None, None)
                                .await
                            {
                                tracing::warn!(
                                    "auto-approve POST failed for run {}: {e}",
                                    run_id_owned
                                );
                            }
                        });
                        AgentEvent::ApprovalResolved {
                            run_id: run_id.clone(),
                            choice: choice.into(),
                        }
                    } else {
                        evt
                    }
                } else {
                    evt
                };

                // Per-event lifecycle hooks. Fire on the *resolved* event so
                // matchers see the same tool name the UI/trace store records
                // (after tier/guardrail rewrites and auto-approve). All are
                // fire-and-forget so a slow hook can't stall the token stream.
                match &evt {
                    AgentEvent::ToolCall { name, args, .. } => {
                        fire_hook_detached(
                            &hooks_for_agent,
                            hook_events::PRE_TOOL_USE,
                            serde_json::json!({
                                "event": hook_events::PRE_TOOL_USE,
                                "session_id": session,
                                "agent_id": agent_id,
                                "tool_name": name,
                                "tool_args": args,
                            }),
                        );
                    }
                    AgentEvent::ToolResult { name, ok, summary, duration_ms } => {
                        fire_hook_detached(
                            &hooks_for_agent,
                            hook_events::POST_TOOL_USE,
                            serde_json::json!({
                                "event": hook_events::POST_TOOL_USE,
                                "session_id": session,
                                "agent_id": agent_id,
                                "tool_name": name,
                                "ok": ok,
                                "summary": summary,
                                "duration_ms": duration_ms,
                            }),
                        );
                    }
                    AgentEvent::ApprovalRequest { run_id, tool, .. } => {
                        fire_hook_detached(
                            &hooks_for_agent,
                            hook_events::PERMISSION_REQUEST,
                            serde_json::json!({
                                "event": hook_events::PERMISSION_REQUEST,
                                "session_id": session,
                                "agent_id": agent_id,
                                "run_id": run_id,
                                "tool_name": tool,
                            }),
                        );
                    }
                    AgentEvent::Error { message } => {
                        fire_hook_detached(
                            &hooks_for_agent,
                            hook_events::NOTIFICATION,
                            serde_json::json!({
                                "event": hook_events::NOTIFICATION,
                                "session_id": session,
                                "agent_id": agent_id,
                                "message": message,
                            }),
                        );
                    }
                    _ => {}
                }

                // Focus-chain scan. Runs on the streamed text and re-emits any
                // completed ```focus-chain block as the `update_focus_chain`
                // tool call the frontend handler + FocusChain panel consume.
                // Emitted directly — NOT routed through the tier/guardrail
                // gates above — because nothing executes: it's a UI state
                // update derived from text the model already streamed, and the
                // tier gate would wrongly block it for untrusted (ReadOnly-
                // pinned) projects. Ordered before the source event's emit so
                // the chain update lands ahead of `done` (the frontend
                // finalizes the assistant bubble on `done`).
                let scanned = match &evt {
                    AgentEvent::Token { delta } => focus_scanner.feed(delta),
                    AgentEvent::Done { .. } => focus_scanner.finish(),
                    _ => None,
                };
                if let Some(items) = scanned {
                    let done_count = items.iter().filter(|t| t.done).count();
                    let preview = format!("{done_count}/{} steps done", items.len());
                    let fc_evt = AgentEvent::ToolCall {
                        name: "update_focus_chain".to_string(),
                        args: serde_json::json!({ "items": items }),
                        preview: Some(preview),
                    };
                    let _ = app_for_agent.emit(
                        &format!("agent-event:{}", session),
                        serde_json::json!({ "agent_id": agent_id, "event": fc_evt }),
                    );
                    if let Some(store) = app_for_agent.try_state::<TracingStore>() {
                        let _ = store.record_event(&span_id, &fc_evt);
                    }
                }

                let payload = serde_json::json!({
                    "agent_id": agent_id,
                    "event": evt,
                });
                let _ = app_for_agent.emit(&format!("agent-event:{}", session), payload);
                if let Some(store) = app_for_agent.try_state::<TracingStore>() {
                    let _ = store.record_event(&span_id, &evt);
                }
            }

            // Stop: the agent run has drained its event channel (the `Done`
            // path). Fire once per agent run, observational only.
            fire_hook_detached(
                &hooks_for_agent,
                hook_events::STOP,
                serde_json::json!({
                    "event": hook_events::STOP,
                    "session_id": session,
                    "agent_id": agent_id,
                }),
            );

            if let Some(store) = app_for_agent.try_state::<TracingStore>() {
                let _ = store.finish_agent_run(&span_id);
            }

            let _ = run_handle.await;
        });
    }

    Ok(ChatSendResult {
        session_id,
        picked_agents: picked,
        routing_reason: reason,
        attachments: attachment_labels,
    })
}

#[derive(Debug, Deserialize)]
pub struct ApproveRunArgs {
    pub run_id: String,
    pub choice: String,
    /// Replacement for the original tool args. Used when the UI surfaces an
    /// editable command on a `bash`/`shell` approval and the user changes
    /// it before clicking Approve. Forwarded to the gateway verbatim.
    #[serde(default)]
    pub edited_payload: Option<serde_json::Value>,
    /// 0-based hunk indices to apply (diff-shaped approvals). `None` keeps
    /// the legacy "apply the whole patch" behavior.
    #[serde(default)]
    pub accepted_hunks: Option<Vec<u32>>,
}

#[tauri::command]
pub async fn approve_run(args: ApproveRunArgs, state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = crate::gateway::client::GatewayClient::new(cfg.gateway_base_url, api_key);
    client
        .approve_run(
            &args.run_id,
            &args.choice,
            args.edited_payload.clone(),
            args.accepted_hunks.clone(),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct StopRunArgs {
    pub run_id: String,
}

#[tauri::command]
pub async fn stop_run(args: StopRunArgs, state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = crate::gateway::client::GatewayClient::new(cfg.gateway_base_url, api_key);
    client.stop_run(&args.run_id).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist the current Plan/Act mode in a process-wide cell. The frontend
/// calls this on toggle so any `chat_send` invocation that omits `mode`
/// (legacy callers, omnibar, etc.) still respects the user's choice.
#[tauri::command]
pub async fn set_current_mode(mode: String) -> Result<(), String> {
    let normalized = mode.trim().to_ascii_lowercase();
    if normalized != "plan" && normalized != "act" {
        return Err(format!("invalid mode '{mode}': expected 'plan' or 'act'"));
    }
    let cell = current_mode_cell();
    let mut g = cell.lock().map_err(|e| format!("mode lock poisoned: {e}"))?;
    *g = normalized;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter::{AgentCapability, AgentDescriptor};
    use async_trait::async_trait;

    /// A no-network adapter that streams a fixed body in two token chunks then
    /// `Done`, so the architect planner-phase collection can be asserted
    /// offline.
    struct PlannerStub {
        chunks: Vec<&'static str>,
    }

    #[async_trait]
    impl crate::agents::AgentAdapter for PlannerStub {
        fn descriptor(&self) -> AgentDescriptor {
            AgentDescriptor {
                id: "planner-stub".into(),
                label: "planner-stub".into(),
                description: String::new(),
                capabilities: vec![AgentCapability::Chat],
                available: true,
            }
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn run(
            &self,
            _req: ChatRequest,
            tx: mpsc::Sender<AgentEvent>,
        ) -> anyhow::Result<()> {
            for c in &self.chunks {
                let _ = tx.send(AgentEvent::Token { delta: (*c).to_string() }).await;
            }
            let _ = tx
                .send(AgentEvent::Done { total_tokens: Some(7), run_id: None })
                .await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn planner_phase_collects_and_streams_tokens() {
        let adapter: std::sync::Arc<dyn crate::agents::AgentAdapter> =
            std::sync::Arc::new(PlannerStub { chunks: vec!["1. step\n", "2. step"] });
        let mut streamed = String::new();
        let plan = run_planner_phase(
            adapter,
            "ollama:test".into(),
            "plan this".into(),
            None,
            |d| streamed.push_str(d),
        )
        .await;
        // The collected plan and the streamed deltas both reconstruct the body.
        assert_eq!(plan.as_deref(), Some("1. step\n2. step"));
        assert_eq!(streamed, "1. step\n2. step");
    }

    #[tokio::test]
    async fn planner_phase_empty_body_yields_none() {
        // A planner that emits only whitespace → None so the caller falls back
        // to a single-phase editor run.
        let adapter: std::sync::Arc<dyn crate::agents::AgentAdapter> =
            std::sync::Arc::new(PlannerStub { chunks: vec!["   ", "\n"] });
        let plan = run_planner_phase(adapter, "m".into(), "p".into(), None, |_| {}).await;
        assert!(plan.is_none());
    }

    /// Live two-phase exercise against a running local Ollama. Ignored by
    /// default (needs `ollama serve` + a pulled model). Run with:
    ///   `cargo test --lib commands::chat::tests::architect_live_ollama -- --ignored`
    /// Proves the real planner adapter (the exact `run_planner_phase` path) draws
    /// a non-empty plan from a live model and that `inject_plan` produces a
    /// well-formed editor prompt carrying both the plan and the original request.
    #[tokio::test]
    #[ignore]
    async fn architect_live_ollama() {
        use crate::agents::ollama::OllamaAgent;
        let model = "ollama:llama3.2:1b";
        let adapter: std::sync::Arc<dyn crate::agents::AgentAdapter> = std::sync::Arc::new(
            OllamaAgent::new("http://127.0.0.1:11434".into(), "llama3.2:1b".into()),
        );
        let user_msg = "Add a /undo slash command that restores the last checkpoint.";
        let plan = run_planner_phase(
            adapter,
            model.into(),
            orchestrator::architect::plan_instruction(user_msg),
            None,
            |_| {},
        )
        .await
        .expect("live planner should return a non-empty plan");
        assert!(plan.len() > 20, "plan too short: {plan:?}");
        let editor_prompt = orchestrator::architect::inject_plan(&plan, user_msg);
        assert!(editor_prompt.contains("<plan>"));
        assert!(editor_prompt.contains(user_msg));
        eprintln!("LIVE PLAN ({} chars):\n{plan}", plan.len());
    }

    #[test]
    fn plan_mode_blocks_write_tools_and_allows_reads() {
        let g = Guardrails::defaults();
        for t in ["write_file", "edit_file", "shell_exec", "bash", "patch", "run_command"] {
            assert!(maybe_block_tool_call("plan", t, "{}", &g).is_some(), "{t}");
        }
        for t in ["read_file", "list_dir", "search"] {
            assert!(maybe_block_tool_call("plan", t, "{}", &g).is_none(), "{t}");
        }
    }

    #[test]
    fn act_mode_allows_write_tools_but_high_risk_always_blocks() {
        let g = Guardrails::defaults();
        assert!(maybe_block_tool_call("act", "write_file", "{}", &g).is_none());
        let hit = maybe_block_tool_call("act", "shell_exec", r#"{"cmd":"rm -rf /"}"#, &g);
        assert!(hit.as_ref().is_some_and(|m| m.contains("high risk")));
        // Plan mode: high-risk wins over plan-mode message.
        let hit = maybe_block_tool_call("plan", "shell_exec", r#"{"cmd":"rm -rf /"}"#, &g);
        assert!(hit.as_ref().is_some_and(|m| m.contains("high risk")));
    }

    #[test]
    fn tier_read_only_blocks_write_tools() {
        let hit = maybe_block_by_tier(SandboxTier::ReadOnly, "write_file", "{}", None);
        assert!(hit.is_some());
        assert!(hit.unwrap().starts_with("sandbox: blocked by tier"));
    }

    #[test]
    fn tier_read_only_allows_read_tools() {
        assert!(maybe_block_by_tier(SandboxTier::ReadOnly, "read_file", "{}", None).is_none());
    }

    #[test]
    fn tier_danger_full_access_allows_everything() {
        assert!(
            maybe_block_by_tier(
                SandboxTier::DangerFullAccess,
                "shell_exec",
                r#"{"cmd":"rm -rf /tmp"}"#,
                None
            )
            .is_none()
        );
    }

    // --- ApprovalPolicy auto-approve re-gate (deferred HIGH finding) ---------
    //
    // These exercise the deny-bias re-gate that closes the `policy = "never"`
    // fail-open: a policy auto-approve may only ever SKIP A PROMPT for an
    // action the tier + guardrails already allow. `would_auto_approve` mirrors
    // chat_send's ApprovalRequest short-circuit (the policy path) end-to-end:
    // the policy decides intent, then `auto_approve_blocked_by_gates` re-checks.

    fn would_auto_approve(
        policy: ApprovalPolicy,
        tier: SandboxTier,
        mode: &str,
        tool: &str,
        payload: &str,
        g: &Guardrails,
        root: Option<&Path>,
    ) -> bool {
        if !policy.auto_approves(tool, payload, root) {
            return false; // policy itself wouldn't auto-approve → user is asked
        }
        auto_approve_blocked_by_gates(tier, mode, tool, payload, g, root).is_none()
    }

    #[test]
    fn never_policy_cannot_auto_approve_writes_a_restrictive_tier_forbids() {
        let g = Guardrails::defaults();
        // The fail-open path: the policy ALONE approves a write under `never`.
        assert!(
            ApprovalPolicy::Never.auto_approves("write_file", r#"{"path":"a.txt"}"#, None),
            "policy alone approves the write (this is what was fail-open)"
        );
        // But the re-gate catches it: a write/exec under a ReadOnly tier is
        // forbidden, so the auto-approve is suppressed and the user is asked.
        assert!(auto_approve_blocked_by_gates(
            SandboxTier::ReadOnly,
            "act",
            "write_file",
            r#"{"path":"a.txt"}"#,
            &g,
            None
        )
        .is_some());
        assert!(!would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::ReadOnly,
            "act",
            "write_file",
            r#"{"path":"a.txt"}"#,
            &g,
            None
        ));
        // A destructive shell command under read-only is likewise re-gated.
        assert!(!would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::ReadOnly,
            "act",
            "shell_exec",
            r#"{"cmd":"rm -rf build"}"#,
            &g,
            None
        ));
    }

    #[test]
    fn never_policy_still_auto_approves_reads_under_read_only() {
        let g = Guardrails::defaults();
        // A read under ReadOnly is tier-allowed → never still auto-approves it.
        assert!(would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::ReadOnly,
            "act",
            "read_file",
            "{}",
            &g,
            None
        ));
        // A provably read-only shell command, too.
        assert!(would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::ReadOnly,
            "act",
            "shell_exec",
            r#"{"cmd":"git status"}"#,
            &g,
            None
        ));
    }

    #[test]
    fn never_policy_auto_approves_in_root_write_but_re_gates_out_of_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let g = Guardrails::defaults();
        let inside = format!(r#"{{"path":"{}/foo.txt"}}"#, root.display());
        // Default WorkspaceWrite + never: an in-root write IS allowed → approved.
        assert!(would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::WorkspaceWrite,
            "act",
            "write_file",
            &inside,
            &g,
            Some(root)
        ));
        // ...but an out-of-root write is re-gated to a prompt.
        assert!(!would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::WorkspaceWrite,
            "act",
            "write_file",
            r#"{"path":"/etc/passwd"}"#,
            &g,
            Some(root)
        ));
    }

    #[test]
    fn never_policy_re_gate_still_blocks_high_risk_at_full_access() {
        let g = Guardrails::defaults();
        // DangerFullAccess allows everything at the tier, but a HIGH-risk
        // guardrail hit is still re-checked and suppresses the auto-approve.
        let block = auto_approve_blocked_by_gates(
            SandboxTier::DangerFullAccess,
            "act",
            "shell_exec",
            r#"{"cmd":"rm -rf /"}"#,
            &g,
            None,
        );
        assert!(block.as_ref().is_some_and(|m| m.contains("high risk")));
        assert!(!would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::DangerFullAccess,
            "act",
            "shell_exec",
            r#"{"cmd":"rm -rf /"}"#,
            &g,
            None
        ));
        // A benign exec at full access is still auto-approved.
        assert!(would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::DangerFullAccess,
            "act",
            "shell_exec",
            r#"{"cmd":"cargo build"}"#,
            &g,
            None
        ));
    }

    #[test]
    fn never_policy_re_gate_respects_plan_mode() {
        let g = Guardrails::defaults();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let inside = format!(r#"{{"path":"{}/foo.txt"}}"#, root.display());
        // Plan mode blocks write/exec tools: never must not auto-approve a write.
        assert!(!would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::WorkspaceWrite,
            "plan",
            "write_file",
            &inside,
            &g,
            Some(root)
        ));
        // The same write in act mode (in-root) IS auto-approved.
        assert!(would_auto_approve(
            ApprovalPolicy::Never,
            SandboxTier::WorkspaceWrite,
            "act",
            "write_file",
            &inside,
            &g,
            Some(root)
        ));
    }

    #[test]
    fn on_request_and_untrusted_unchanged_by_re_gate() {
        let g = Guardrails::defaults();
        // OnRequest never auto-approves to begin with — still asks even at full access.
        assert!(!would_auto_approve(
            ApprovalPolicy::OnRequest,
            SandboxTier::DangerFullAccess,
            "act",
            "read_file",
            "{}",
            &g,
            None
        ));
        // Untrusted only intends to approve provably read-only actions, which the
        // re-gate also allows under ReadOnly — its safe behavior is unchanged.
        assert!(would_auto_approve(
            ApprovalPolicy::Untrusted,
            SandboxTier::ReadOnly,
            "act",
            "read_file",
            "{}",
            &g,
            None
        ));
        assert!(!would_auto_approve(
            ApprovalPolicy::Untrusted,
            SandboxTier::ReadOnly,
            "act",
            "write_file",
            r#"{"path":"a.txt"}"#,
            &g,
            None
        ));
    }

    #[test]
    fn images_envelope_none_when_empty() {
        assert!(build_images_envelope(&[], "hi").is_none());
    }

    #[test]
    fn images_envelope_wraps_message_when_present() {
        let imgs = vec!["data:image/png;base64,AAAA".to_string()];
        let out = build_images_envelope(&imgs, "describe this").unwrap();
        assert!(out.starts_with("<images>\n"));
        assert!(out.contains("\"media_type\":\"image/png\""));
        assert!(out.ends_with("describe this"));
    }

    #[test]
    fn images_envelope_rejects_bad_mime_and_falls_back() {
        let imgs = vec!["data:image/bmp;base64,AAAA".to_string()];
        // Single invalid attachment yields None — message passes through unchanged.
        assert!(build_images_envelope(&imgs, "hi").is_none());
    }

    #[test]
    fn parse_image_data_uri_happy_path() {
        let (mime, body) = parse_image_data_uri("data:image/jpeg;base64,/9j/QkM=").unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(body, "/9j/QkM=");
    }

    #[test]
    fn parse_image_data_uri_rejects_non_base64_encoding() {
        let err = parse_image_data_uri("data:image/png;utf8,xxx").unwrap_err();
        assert!(err.contains("unsupported encoding"));
    }

    #[test]
    fn read_project_rules_prefers_agents_md_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        // No file → None.
        assert!(super::read_project_rules(dir.path()).is_none());
        // CLAUDE.md present but AGENTS.md wins when both exist.
        std::fs::write(dir.path().join("CLAUDE.md"), "claude rules").unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "agents rules").unwrap();
        let out = super::read_project_rules(dir.path()).unwrap();
        assert!(out.contains("AGENTS.md") && out.contains("agents rules"));
        assert!(!out.contains("claude rules"));
        // Large file is capped on a char boundary.
        std::fs::remove_file(dir.path().join("AGENTS.md")).unwrap();
        std::fs::remove_file(dir.path().join("CLAUDE.md")).unwrap();
        std::fs::write(dir.path().join(".cortexrules"), "é".repeat(10_000)).unwrap();
        let capped = super::read_project_rules(dir.path()).unwrap();
        assert!(capped.len() <= 8 * 1024 + 32);
    }

    // Wave 300 — context prefix combines project rules + ranked repo-map.
    #[test]
    fn build_context_prefix_combines_rules_and_repo_map() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // No project files at all => empty prefix (nothing to inject).
        assert_eq!(super::build_context_prefix(root, "hi"), "");

        // Rules file alone => just the <project_rules> block.
        std::fs::write(root.join("CLAUDE.md"), "be concise").unwrap();
        let rules_only = super::build_context_prefix(root, "hi");
        assert!(rules_only.contains("<project_rules>") && rules_only.contains("be concise"));
        assert!(!rules_only.contains("<repo_map>"), "no source yet => no repo-map: {rules_only}");

        // Add source => both blocks present, repo-map carries a ranked file.
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub struct Widget {}\npub fn build() {}\n").unwrap();
        let both = super::build_context_prefix(root, "explain Widget");
        assert!(both.contains("<project_rules>"), "rules block missing: {both}");
        assert!(both.contains("<repo_map>"), "repo-map block missing: {both}");
        assert!(both.contains("src/lib.rs"), "repo-map should list the source file: {both}");
        // Rules come before the repo-map.
        assert!(both.find("<project_rules>").unwrap() < both.find("<repo_map>").unwrap());
    }

    // Aider `/add` — a manifested file injects a <files> block, between rules
    // and the repo-map, carrying the file's live contents.
    #[test]
    fn build_context_prefix_includes_manifest_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("CLAUDE.md"), "be concise").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn token_marker() {}\n").unwrap();
        // Nothing added yet => no <files> block.
        let before = super::build_context_prefix(root, "hi");
        assert!(!before.contains("<files>"), "no manifest => no files block: {before}");
        // Add the file to the chat manifest.
        crate::commands::manifest::add_paths(root, &["src/lib.rs".into()]).unwrap();
        let after = super::build_context_prefix(root, "hi");
        assert!(after.contains("<files>"), "manifest file should inject a <files> block: {after}");
        assert!(after.contains("token_marker"), "block should carry the file's contents: {after}");
        // Ordering: rules → files → repo-map.
        let r = after.find("<project_rules>").unwrap();
        let f = after.find("<files>").unwrap();
        let m = after.find("<repo_map>").unwrap();
        assert!(r < f && f < m, "expected rules < files < repo_map: {after}");
    }

    // OpenHands knowledge microagents — a triggered microagent injects a
    // <knowledge> block, ordered right after <project_rules>; an untriggered
    // message leaves it out.
    #[test]
    fn build_context_prefix_includes_triggered_microagents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("CLAUDE.md"), "be concise").unwrap();
        let ma = root.join(".cortex").join("microagents");
        std::fs::create_dir_all(&ma).unwrap();
        std::fs::write(
            ma.join("payments.md"),
            "---\nname: Payments\ntriggers:\n  - payment\n---\nAlways use the idempotency key.",
        )
        .unwrap();
        // A message without the trigger word => no <knowledge> block.
        let miss = super::build_context_prefix(root, "what is the weather?");
        assert!(!miss.contains("<knowledge>"), "untriggered => no knowledge block: {miss}");
        assert!(miss.contains("<project_rules>"), "rules still present: {miss}");
        // A message with the trigger word => the knowledge is injected, after rules.
        let hit = super::build_context_prefix(root, "how do I refund a payment?");
        assert!(hit.contains("<knowledge>"), "trigger should inject knowledge: {hit}");
        assert!(hit.contains("idempotency key"), "block carries the microagent body: {hit}");
        let r = hit.find("<project_rules>").unwrap();
        let k = hit.find("<knowledge>").unwrap();
        assert!(r < k, "expected rules < knowledge: {hit}");
    }

    // Wave 152 — implicit-mention regression tests for `expand_at_tokens`.

    #[test]
    fn expand_at_tokens_picks_up_implicit_path_mention() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let auth = src.join("auth.rs");
        std::fs::write(&auth, "// authentication module").unwrap();

        let msg = "fix the bug in src/auth.rs please";
        let (out, labels) = expand_at_tokens(msg, Some(dir.path()));
        assert!(out.contains("// authentication module"), "expanded body missing");
        assert!(labels.iter().any(|l| l.starts_with("mentioned/src/auth.rs")), "label missing: {labels:?}");
    }

    #[test]
    fn expand_at_tokens_caps_implicit_mentions_at_three() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            std::fs::write(src.join(name), format!("// {name}")).unwrap();
        }

        let msg = "review src/a.rs and src/b.rs and src/c.rs and src/d.rs";
        let (_out, labels) = expand_at_tokens(msg, Some(dir.path()));
        let mentions: Vec<_> = labels.iter().filter(|l| l.starts_with("mentioned/")).collect();
        assert!(mentions.len() <= 3, "too many mentions: {mentions:?}");
    }

    #[test]
    fn expand_at_tokens_ignores_urls_and_at_prefixed() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("auth.rs"), "x").unwrap();

        // URL should not trigger; explicit @file: stays as the @-handler's job.
        let msg = "compare https://example.com/auth.rs vs @file:/absolute/path";
        let (_out, labels) = expand_at_tokens(msg, Some(dir.path()));
        assert!(!labels.iter().any(|l| l.contains("example.com")), "URL leaked: {labels:?}");
    }

    #[test]
    fn expand_at_tokens_websearch_empty_query_is_unresolved() {
        // A bare `@websearch:` (or alias) with no query must NOT hit the
        // network and must NOT attach anything — it falls through unresolved.
        // (The live fetch path is covered by websearch::live_ddg_search_*.)
        let dir = tempfile::tempdir().unwrap();
        for msg in ["look up @websearch:", "find @search:  ", "see @google:"] {
            let (_out, labels) = expand_at_tokens(msg, Some(dir.path()));
            assert!(
                !labels.iter().any(|l| l.starts_with("@websearch:")),
                "empty-query websearch should not attach: {labels:?} ({msg})"
            );
        }
    }

    #[test]
    fn format_websearch_attachment_renders_results_and_label() {
        use crate::websearch::WebResult;
        let results = vec![
            WebResult {
                title: "The Rust Language".into(),
                url: "https://rust-lang.org/".into(),
                snippet: "A language empowering everyone.".into(),
            },
            // Empty title falls back to the URL; empty snippet is omitted.
            WebResult { title: "  ".into(), url: "https://docs.rs/".into(), snippet: "".into() },
        ];
        let (attachment, label) = format_websearch_attachment("rust lang", &results);
        assert!(attachment.contains("@websearch:rust lang (2 results)"));
        assert!(attachment.contains("1. The Rust Language"));
        assert!(attachment.contains("https://rust-lang.org/"));
        assert!(attachment.contains("A language empowering everyone."));
        // Fallback title for the empty-title result.
        assert!(attachment.contains("2. https://docs.rs/"));
        assert!(attachment.contains("```text"));
        assert_eq!(label, "@websearch:rust lang");
    }

    // Editor↔agent loop — `@file:<abs>:L<start>-L<end>` selection mentions.

    #[test]
    fn split_line_range_parses_ranges_and_rejects_malformed() {
        assert_eq!(split_line_range("/a/b.rs:L10-L24"), ("/a/b.rs", Some((10, 24))));
        assert_eq!(split_line_range("/a/b.rs:L10-24"), ("/a/b.rs", Some((10, 24))));
        assert_eq!(split_line_range("/a/b.rs:L7"), ("/a/b.rs", Some((7, 7))));
        // Malformed specs leave the string untouched.
        assert_eq!(split_line_range("/a/b.rs:L0"), ("/a/b.rs:L0", None));
        assert_eq!(split_line_range("/a/b.rs:L9-L3"), ("/a/b.rs:L9-L3", None));
        assert_eq!(split_line_range("/a/b.rs:Lfoo"), ("/a/b.rs:Lfoo", None));
        // Plain paths / hint-only line suffixes are untouched.
        assert_eq!(split_line_range("/a/b.rs"), ("/a/b.rs", None));
        assert_eq!(split_line_range("/a/b.rs:42"), ("/a/b.rs:42", None));
        assert_eq!(split_line_range("C:\\a\\b.rs"), ("C:\\a\\b.rs", None));
    }

    #[test]
    fn slice_lines_is_one_based_inclusive_and_clamped() {
        let body = "one\ntwo\nthree\nfour";
        assert_eq!(slice_lines(body, 2, 3), "two\nthree");
        assert_eq!(slice_lines(body, 1, 1), "one");
        assert_eq!(slice_lines(body, 3, 99), "three\nfour");
        assert_eq!(slice_lines(body, 10, 12), "");
    }

    #[test]
    fn expand_at_tokens_slices_explicit_file_mention_with_range() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("code.rs");
        std::fs::write(&f, "line1\nline2\nline3\nline4\nline5").unwrap();

        let msg = format!("explain @file:{}:L2-L4 please", f.display());
        let (out, labels) = expand_at_tokens(&msg, None);
        assert!(out.contains("line2\nline3\nline4"), "sliced body missing:\n{out}");
        assert!(!out.contains("line1"), "range leaked preceding lines:\n{out}");
        assert!(!out.contains("line5"), "range leaked following lines:\n{out}");
        assert!(labels.iter().any(|l| l == "code.rs:L2-L4"), "label missing range: {labels:?}");
    }

    #[test]
    fn expand_at_tokens_range_past_eof_leaves_token_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("short.rs");
        std::fs::write(&f, "only line").unwrap();

        let msg = format!("see @file:{}:L10-L20", f.display());
        let (out, labels) = expand_at_tokens(&msg, None);
        assert!(labels.is_empty(), "past-EOF range should not attach: {labels:?}");
        assert!(!out.contains("<!-- attached"), "no attachment expected:\n{out}");
    }

    #[test]
    fn expand_at_tokens_whole_file_mention_still_works_with_ranges_present() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("code.rs");
        std::fs::write(&f, "alpha\nbeta").unwrap();

        // Two slices of the same file both attach (range is part of the dedup
        // key); the bare mention attaches the whole file alongside.
        let msg = format!(
            "compare @file:{p}:L1 and @file:{p}:L2 with @file:{p}",
            p = f.display()
        );
        let (out, labels) = expand_at_tokens(&msg, None);
        assert_eq!(labels.len(), 3, "labels: {labels:?}");
        assert!(out.contains("```rs\nalpha\n```"), "L1 slice missing:\n{out}");
        assert!(out.contains("```rs\nbeta\n```"), "L2 slice missing:\n{out}");
        assert!(out.contains("```rs\nalpha\nbeta\n```"), "whole file missing:\n{out}");
    }

    #[test]
    fn build_tree_is_deterministic_and_nested() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/net")).unwrap();
        std::fs::write(root.join("src/net/retry.rs"), "x").unwrap();
        std::fs::write(root.join("src/main.rs"), "x").unwrap();
        std::fs::write(root.join("Cargo.toml"), "x").unwrap();
        // Noise + hidden are skipped.
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/bin"), "x").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".env"), "x").unwrap();

        let tree = build_tree(root, 6, 400);
        // Dirs sort before files; children render beneath their parent.
        let expected = "src/\n  net/\n    retry.rs\n  main.rs\nCargo.toml\n";
        assert_eq!(tree, expected, "tree:\n{tree}");
        assert!(!tree.contains("target"), "noise dir leaked: {tree}");
        assert!(!tree.contains(".git"), "hidden dir leaked: {tree}");
        assert!(!tree.contains(".env"), "hidden file leaked: {tree}");

        // Same inputs → byte-identical output (deterministic ordering).
        assert_eq!(build_tree(root, 6, 400), tree);
    }

    #[test]
    fn build_tree_respects_depth_and_entry_cap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("a/b/c/deep.rs"), "x").unwrap();
        std::fs::write(root.join("a/top.rs"), "x").unwrap();

        // depth 1 shows only top-level.
        let d1 = build_tree(root, 1, 400);
        assert_eq!(d1, "a/\n", "depth-1: {d1}");
        // depth 2 reaches a's children but not b's.
        let d2 = build_tree(root, 2, 400);
        assert!(d2.contains("b/") && d2.contains("top.rs"), "depth-2: {d2}");
        assert!(!d2.contains("deep.rs"), "depth-2 went too deep: {d2}");

        // Entry cap truncates and flags it.
        for i in 0..10 {
            std::fs::write(root.join(format!("f{i}.rs")), "x").unwrap();
        }
        let capped = build_tree(root, 1, 3);
        assert!(capped.contains("truncated"), "cap not flagged: {capped}");
        assert_eq!(capped.lines().filter(|l| !l.contains("truncated")).count(), 3);
    }

    #[test]
    fn build_tree_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(build_tree(dir.path(), 3, 400), "(empty)");
    }

    #[test]
    fn expand_at_tokens_tree_attaches_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let (out, labels) = expand_at_tokens("explain the layout @tree", Some(dir.path()));
        assert!(labels.iter().any(|l| l == "@tree"), "missing @tree label: {labels:?}");
        assert!(out.contains("src/"), "tree body missing from expansion: {out}");
        assert!(out.contains("main.rs"), "tree body missing file: {out}");

        // `@tree` resolves only when a project root is present (project-scoped).
        let (_o, labels_none) = expand_at_tokens("@tree", None);
        assert!(!labels_none.iter().any(|l| l == "@tree"), "tree resolved without root");
    }

    #[test]
    fn expand_at_tokens_outline_attaches_file_symbols() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/api.rs"),
            "pub struct Client {}\npub fn connect() -> Client { Client {} }\n",
        )
        .unwrap();

        let (out, labels) =
            expand_at_tokens("outline @outline:src/api.rs please", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "@outline:src/api.rs"),
            "missing @outline label: {labels:?}"
        );
        assert!(out.contains("src/api.rs · rust"), "outline header missing: {out}");
        assert!(out.contains("pub fn connect()"), "outline symbol missing: {out}");

        // Project-scoped: no root → token ships verbatim, nothing attached.
        let (_o, labels_none) = expand_at_tokens("@outline:src/api.rs", None);
        assert!(
            !labels_none.iter().any(|l| l.starts_with("@outline")),
            "outline resolved without root"
        );
    }

    /// The Git @-menu (at-vocab.ts `fetchGit`) inserts `@log` / `@blame:<file>`
    /// / `@env` tokens; this locks the backend contract those rely on — each
    /// resolves to real content inside a real git repo, and is project-scoped
    /// (no root → unresolved, never a dead token). Skips silently if `git`
    /// isn't on PATH so the suite stays hermetic on a bare CI box.
    #[test]
    fn resolve_git_provider_tokens_from_menu() {
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            eprintln!("skipping resolve_git_provider_tokens_from_menu: git not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(root.join("hello.rs"), "fn main() {}\n").unwrap();
        run(&["add", "hello.rs"]);
        run(&["commit", "-q", "-m", "seed commit"]);

        // @log — recent commits oneline.
        let (label, body) = resolve_special_token("@log", Some(root)).expect("@log unresolved");
        assert_eq!(label, "@log:20");
        assert!(body.contains("seed commit"), "@log missing commit: {body}");

        // @env — root + HEAD + branch orientation block.
        let (elabel, ebody) = resolve_special_token("@env", Some(root)).expect("@env unresolved");
        assert_eq!(elabel, "@env");
        assert!(ebody.contains("project_root:"), "@env missing root: {ebody}");
        assert!(ebody.contains("git_head:"), "@env missing head: {ebody}");

        // @blame:<file> — per-line authorship; the seeded author shows up.
        let (blabel, bbody) =
            resolve_special_token("@blame:hello.rs", Some(root)).expect("@blame unresolved");
        assert_eq!(blabel, "@blame:hello.rs");
        assert!(bbody.contains("Test"), "@blame missing author: {bbody}");
        assert!(bbody.contains("fn main()"), "@blame missing source line: {bbody}");

        // Path-confinement: a `..` escape is refused.
        assert!(
            resolve_special_token("@blame:../escape", Some(root)).is_none(),
            "@blame allowed a .. escape"
        );

        // Project-scoped: no root → unresolved (never a dead token).
        assert!(resolve_special_token("@log", None).is_none(), "@log resolved without root");
        assert!(resolve_special_token("@env", None).is_none(), "@env resolved without root");
    }

    #[test]
    fn expand_at_tokens_folder_inlines_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/api.rs"), "pub fn connect() {}\n").unwrap();
        std::fs::write(dir.path().join("src/util.ts"), "export const x = 1;\n").unwrap();

        let (out, labels) =
            expand_at_tokens("read @folder:src for me", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "@folder:src"),
            "missing @folder label: {labels:?}"
        );
        assert!(out.contains("src · 2 files"), "folder header missing: {out}");
        assert!(out.contains("pub fn connect()"), "api.rs body missing: {out}");
        assert!(out.contains("export const x"), "util.ts body missing: {out}");

        // Alias `@dir:` resolves the same way.
        let (_o2, labels_dir) = expand_at_tokens("@dir:src", Some(dir.path()));
        assert!(
            labels_dir.iter().any(|l| l == "@folder:src"),
            "@dir alias didn't resolve: {labels_dir:?}"
        );

        // Project-scoped: no root → token ships verbatim, nothing attached.
        let (_o, labels_none) = expand_at_tokens("@folder:src", None);
        assert!(
            !labels_none.iter().any(|l| l.starts_with("@folder")),
            "folder resolved without root"
        );
    }

    #[test]
    fn expand_at_tokens_def_attaches_symbol_definition() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/api.rs"),
            "pub struct Client {}\npub fn connect() -> Client { Client {} }\n",
        )
        .unwrap();

        let (out, labels) = expand_at_tokens("explain @def:connect please", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "@def:connect"),
            "missing @def label: {labels:?}"
        );
        assert!(out.contains("connect · 1 definition"), "def header missing: {out}");
        assert!(out.contains("// src/api.rs:2  (fn)"), "def site missing: {out}");
        assert!(out.contains("pub fn connect()"), "def body missing: {out}");

        // The `@symbol:` alias resolves through the same arm.
        let (_o2, labels_alias) = expand_at_tokens("@symbol:connect", Some(dir.path()));
        assert!(
            labels_alias.iter().any(|l| l == "@def:connect"),
            "@symbol alias did not resolve: {labels_alias:?}"
        );

        // Project-scoped: no root → token ships verbatim, nothing attached.
        let (_o3, labels_none) = expand_at_tokens("@def:connect", None);
        assert!(
            !labels_none.iter().any(|l| l.starts_with("@def")),
            "def resolved without root"
        );
    }

    #[test]
    fn expand_at_tokens_refs_attaches_all_use_sites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/api.rs"),
            "pub fn connect() {}\nfn run() { connect(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn boot() { connect(); }\n",
        )
        .unwrap();

        let (out, labels) = expand_at_tokens("where is @refs:connect used", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "@refs:connect"),
            "missing @refs label: {labels:?}"
        );
        // 3 whole-word references (1 decl + 2 call sites) across 2 files.
        assert!(
            out.contains("connect · 3 references in 2 files"),
            "refs header missing: {out}"
        );
        assert!(out.contains("(def)"), "decl marker missing: {out}");
        assert!(out.contains("connect();"), "use site missing: {out}");

        // The `@callers:` alias resolves through the same arm.
        let (_o2, labels_alias) = expand_at_tokens("@callers:connect", Some(dir.path()));
        assert!(
            labels_alias.iter().any(|l| l == "@refs:connect"),
            "@callers alias did not resolve: {labels_alias:?}"
        );

        // Project-scoped: no root → token ships verbatim, nothing attached.
        let (_o3, labels_none) = expand_at_tokens("@refs:connect", None);
        assert!(
            !labels_none.iter().any(|l| l.starts_with("@refs")),
            "refs resolved without root"
        );
    }

    #[test]
    fn expand_at_tokens_problems_wiring_and_aliases() {
        // Empty dir = no Rust/TS project → fast path, no compiler invoked.
        let dir = tempfile::tempdir().unwrap();
        let (out, labels) = expand_at_tokens("any @problems here", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "@problems"),
            "missing @problems label: {labels:?}"
        );
        assert!(
            out.contains("no Rust/TypeScript project detected"),
            "no-project signal missing: {out}"
        );

        // The `@diagnostics` / `@lint` aliases resolve through the same arm.
        let (_o2, labels_diag) = expand_at_tokens("@diagnostics", Some(dir.path()));
        assert!(
            labels_diag.iter().any(|l| l == "@problems"),
            "@diagnostics alias did not resolve: {labels_diag:?}"
        );
        let (_o3, labels_lint) = expand_at_tokens("@lint", Some(dir.path()));
        assert!(
            labels_lint.iter().any(|l| l == "@problems"),
            "@lint alias did not resolve: {labels_lint:?}"
        );

        // Project-scoped: no root → token ships verbatim, nothing attached.
        let (_o4, labels_none) = expand_at_tokens("@problems", None);
        assert!(
            !labels_none.iter().any(|l| l.starts_with("@problems")),
            "problems resolved without root"
        );
    }

    /// End-to-end: a real `cargo check` over a temp crate with a deliberate
    /// type error must surface through `@problems`. The crate has no
    /// dependencies, so the check is fast (~0.2s) and needs no network.
    #[test]
    fn expand_at_tokens_problems_surfaces_real_cargo_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"probtest\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        // `let x: u32 = "..."` → E0308 mismatched types.
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    let x: u32 = \"not a number\";\n    println!(\"{}\", x);\n}\n",
        )
        .unwrap();

        let (out, labels) = expand_at_tokens("fix @problems please", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "@problems"),
            "missing @problems label: {labels:?}"
        );
        assert!(
            out.contains("mismatched types"),
            "real cargo error not surfaced: {out}"
        );
        assert!(
            out.contains("[error]") && out.contains("src/main.rs"),
            "error row/location missing: {out}"
        );
    }

    #[test]
    fn expand_at_tokens_personalized_repomap() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("hot.rs"), "pub fn processOrder() {}\n").unwrap();
        std::fs::write(src.join("cold.rs"), "pub fn unrelated() {}\n").unwrap();

        let msg = "refactor processOrder @repomap";
        let (out, labels) = expand_at_tokens(msg, Some(dir.path()));
        // @repomap should be attached and the attachment comment should
        // list the personalize term so the model knows what was prioritized.
        assert!(labels.iter().any(|l| l == "@repomap"), "missing @repomap label: {labels:?}");
        assert!(
            out.contains("personalized: processOrder"),
            "personalize comment missing from expansion: {}",
            out.lines()
                .filter(|l| l.contains("repomap"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn expand_at_tokens_preserves_line_suffix_in_label() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("auth.rs"), "x").unwrap();

        let (_out, labels) = expand_at_tokens("see src/auth.rs:42", Some(dir.path()));
        assert!(
            labels.iter().any(|l| l == "mentioned/src/auth.rs:42"),
            "line suffix missing: {labels:?}"
        );
    }

    #[test]
    fn resolve_terminal_token_reads_tail_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".cortex")).unwrap();
        let log = home.join(".cortex").join("last-shell-output.log");
        let body: String = (1..=5).map(|n| format!("out {n}\n")).collect();
        std::fs::write(&log, &body).unwrap();

        // Plain @terminal resolves to (label, full body).
        let (name, content) = resolve_terminal_token("@terminal", home).expect("resolved");
        assert_eq!(name, "@terminal");
        assert!(content.contains("out 5"), "body missing tail: {content}");

        // @terminal:N tails to N lines.
        let (_n, tail) = resolve_terminal_token("@terminal:2", home).expect("resolved");
        assert_eq!(tail, "out 4\nout 5");

        // A bogus count falls back to the default (still resolves), and a
        // non-terminal token is rejected.
        assert!(resolve_terminal_token("@terminal:abc", home).is_some());
        assert!(resolve_terminal_token("@diff", home).is_none());

        // Absent log → None even for a valid terminal token.
        let empty = tempfile::tempdir().unwrap();
        assert!(resolve_terminal_token("@terminal", empty.path()).is_none());
    }

    #[test]
    fn expand_at_tokens_attaches_terminal_output() {
        // Point $HOME at a temp dir holding a recorded shell log so the
        // end-to-end token→attachment path is exercised, not just the helper.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cortex")).unwrap();
        std::fs::write(
            dir.path().join(".cortex").join("last-shell-output.log"),
            "cargo build\nerror[E0433]: failed to resolve\n",
        )
        .unwrap();

        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let (out, labels) = expand_at_tokens("why did this fail @terminal", None);
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert!(labels.iter().any(|l| l == "@terminal"), "label missing: {labels:?}");
        assert!(out.contains("error[E0433]"), "terminal output not inlined: {out}");
        assert!(out.contains("<!-- attached: @terminal"), "attachment header missing: {out}");
    }

    #[test]
    fn resolve_codebase_token_ranks_relevant_files() {
        // A repo where one file clearly matches the query and another doesn't;
        // the blended retrieval should surface the matching file with its
        // source tag and a snippet, and reject non-codebase tokens.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("auth.rs"),
            "pub fn authenticate_user(token: &str) -> bool { token == \"ok\" }\n",
        )
        .unwrap();
        std::fs::write(src.join("paint.rs"), "pub fn render_pixels() {}\n").unwrap();

        let (name, body) =
            resolve_codebase_token("@codebase", "fix authenticate_user", dir.path())
                .expect("resolved");
        assert_eq!(name, "@codebase");
        assert!(
            body.contains("auth.rs"),
            "expected the matching file in the body: {body}"
        );
        // Body rows carry a bracketed source tag (symbol/memory/recent).
        assert!(
            body.contains('[') && body.contains(']'),
            "expected a source tag in the body: {body}"
        );

        // Non-codebase tokens and an empty query are rejected; a non-dir root
        // resolves to None rather than panicking.
        assert!(resolve_codebase_token("@diff", "x", dir.path()).is_none());
        assert!(resolve_codebase_token("@codebase", "   ", dir.path()).is_none());
        assert!(resolve_codebase_token("@codebase", "x", &dir.path().join("nope")).is_none());
    }

    #[test]
    fn resolve_codebase_token_clamps_count_and_handles_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..30 {
            std::fs::write(
                src.join(format!("mod{i}.rs")),
                format!("pub fn helper{i}() {{}}\npub struct Type{i};\n"),
            )
            .unwrap();
        }

        // `@codebase:3` clamps the result set — at most 3 hit rows (each hit is
        // one `path [source]` line; snippet lines are indented with two spaces).
        let (_n, body) = resolve_codebase_token("@codebase:3", "helper", dir.path())
            .expect("resolved");
        let hit_rows = body
            .lines()
            .filter(|l| !l.starts_with("  ") && !l.trim().is_empty())
            .count();
        assert!(hit_rows <= 3, "expected ≤3 hit rows, got {hit_rows}: {body}");

        // A query with no plausible match still resolves (to a placeholder),
        // never None, so the model gets explicit "nothing found" signal.
        let res = resolve_codebase_token(
            "@codebase",
            "zzqqx_nonexistent_identifier_xyzzy",
            dir.path(),
        );
        assert!(res.is_some(), "no-match should resolve to a placeholder");
    }

    #[test]
    fn expand_at_tokens_attaches_codebase_retrieval() {
        // End-to-end: a typed @codebase in a real repo attaches a ranked block
        // and labels it, using the rest of the message (minus @-tokens) as the
        // query.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("checkout.rs"),
            "pub fn process_checkout(cart: &Cart) -> Receipt { todo!() }\n",
        )
        .unwrap();

        let (out, labels) =
            expand_at_tokens("where is process_checkout handled @codebase", Some(dir.path()));
        assert!(labels.iter().any(|l| l == "@codebase"), "label missing: {labels:?}");
        assert!(out.contains("<!-- attached: @codebase"), "attachment header missing: {out}");
        assert!(out.contains("checkout.rs"), "ranked file not inlined: {out}");
    }

    #[test]
    fn resolve_docs_token_ranks_relevant_section() {
        // A repo with two doc files; the section whose heading matches the query
        // should rank first and its body content should be injected.
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("guide.md"),
            "# Project Guide\n\nIntro prose.\n\n\
             ## Authentication\n\nUsers sign in with a bearer token issued by the gateway.\n\n\
             ## Deployment\n\nShip the container to the cluster.\n",
        )
        .unwrap();
        std::fs::write(
            docs.join("style.md"),
            "# Style\n\nUse two-space indentation everywhere.\n",
        )
        .unwrap();

        let (name, body) =
            resolve_docs_token("@docs", "how does authentication work", dir.path())
                .expect("resolved");
        assert_eq!(name, "@docs");
        // The Authentication section's location header is emitted and its prose
        // body is inlined (so the model can actually read the docs).
        assert!(
            body.contains("guide.md#Authentication"),
            "expected the authentication section location: {body}"
        );
        assert!(
            body.contains("bearer token issued by the gateway"),
            "expected the section body inlined: {body}"
        );
        // The heading-matching section ranks ahead of the unrelated style file.
        let auth_pos = body.find("#Authentication").unwrap();
        let style_pos = body.find("style.md").unwrap_or(usize::MAX);
        assert!(auth_pos < style_pos, "authentication should rank first: {body}");

        // Non-docs tokens, an empty query, and a non-dir root all resolve to None.
        assert!(resolve_docs_token("@codebase", "auth", dir.path()).is_none());
        assert!(resolve_docs_token("@docs", "a b", dir.path()).is_none()); // terms <3 chars
        assert!(resolve_docs_token("@docs", "auth", &dir.path().join("nope")).is_none());
    }

    #[test]
    fn resolve_docs_token_clamps_count_and_handles_no_match() {
        let dir = tempfile::tempdir().unwrap();
        // Several doc files each with a section mentioning the query term.
        for i in 0..10 {
            std::fs::write(
                dir.path().join(format!("note{i}.md")),
                format!("# Note {i}\n\nThis covers the widget subsystem in detail.\n"),
            )
            .unwrap();
        }

        // `@docs:2` clamps to at most 2 injected sections (each section starts
        // with a `## ` location header).
        let (_n, body) = resolve_docs_token("@docs:2", "widget subsystem", dir.path())
            .expect("resolved");
        let section_headers = body.lines().filter(|l| l.starts_with("## ")).count();
        assert!(
            section_headers <= 2,
            "expected ≤2 sections, got {section_headers}: {body}"
        );

        // A query with no documentation match still resolves to an explicit
        // placeholder (never None), so the model gets a "nothing found" signal.
        let res = resolve_docs_token("@docs", "zzqqx_nonexistent_topic_xyzzy", dir.path());
        assert!(res.is_some(), "no-match should resolve to a placeholder");
        assert!(
            res.unwrap().1.contains("no relevant docs"),
            "expected the no-match placeholder"
        );
    }

    #[test]
    fn expand_at_tokens_attaches_docs_retrieval() {
        // End-to-end: a typed @docs in a real repo attaches a ranked doc block
        // and labels it, using the rest of the message (minus @-tokens) as the
        // query.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# MyApp\n\n## Installation\n\nRun `cargo install myapp` to install the binary.\n\n\
             ## Usage\n\nInvoke `myapp run` to start.\n",
        )
        .unwrap();

        let (out, labels) =
            expand_at_tokens("what is the installation process @docs", Some(dir.path()));
        assert!(labels.iter().any(|l| l == "@docs"), "label missing: {labels:?}");
        assert!(out.contains("<!-- attached: @docs"), "attachment header missing: {out}");
        assert!(
            out.contains("README.md#Installation"),
            "ranked doc section not inlined: {out}"
        );
        assert!(
            out.contains("cargo install myapp"),
            "doc body not inlined: {out}"
        );
    }
}
