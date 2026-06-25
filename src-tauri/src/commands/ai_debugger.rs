//! AI debugger. Given an error from one of five sources, pulls the message +
//! optional stack, extracts a `path:line` reference when present, slurps a
//! ~16 KiB window around it, and asks the gateway for `{root_cause, suggested_fix,
//! code_patch, confidence}`. Same streaming-collect + timeout pattern as
//! [`super::explain`] / [`super::refactor_suggester`]; parse-or-fallback so a
//! malformed model response still yields a usable `DebugResult`.
//!
//! User flow: `/fix` (default `recent_crash`) / `/debug` (alias). The modal
//! lets the user switch source or paste manually. Advisory only — no disk
//! writes; the user owns the final edit.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::observability::crash;
use crate::observability::tracing_store::TracingStore;

/// 16 KiB code window — function-level slice around the error site.
const FILE_WINDOW_BYTES: usize = 16 * 1024;
/// ±30 lines around the error line ≈ a typical function plus padding.
const CONTEXT_LINES: usize = 60;
/// 45s wall clock — same headroom as `/explain`; debugging is heavier than refactor.
const TIMEOUT: Duration = Duration::from_secs(45);

const SYSTEM_PROMPT: &str = "You are an AI debugger. Given an error message, \
optional stack trace, and the surrounding source code (when available), \
identify the bug and propose a minimal fix. Respond with ONLY a single JSON \
object containing exactly these keys: \
`root_cause` (one sentence — what is actually broken), \
`suggested_fix` (one sentence — what the user should change), \
`code_patch` (a unified diff — use `--- a/<path>` / `+++ b/<path>` headers \
when you have a path, otherwise an empty string), \
`confidence` (a number 0.0-1.0 — be conservative when you only see the error \
text without code). Do not wrap the JSON in fences or prose.";

#[derive(Debug, Deserialize)]
pub struct DebugErrorArgs {
    pub project_root: String,
    pub error_source: String,
    #[serde(default)]
    #[allow(dead_code)] // reserved — future per-row debugging from the issue/crash panels
    pub error_id: Option<String>,
    /// Required for `manual` / `chat_error`; ignored for the other sources.
    #[serde(default)]
    pub error_text: Option<String>,
    /// Optional stack — only used for `manual` / `chat_error`.
    #[serde(default)]
    pub error_stack: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DebugResult {
    pub error_source: String,
    pub error_summary: String,
    pub root_cause: String,
    pub suggested_fix: String,
    pub code_patch: String,
    pub confidence: f64,
    pub source_path: Option<String>,
    pub source_line: Option<usize>,
    pub generated_unix_ms: i64,
}

/// Internal: the raw error we pulled from the chosen source.
struct ResolvedError {
    summary: String,
    message: String,
    stack: Option<String>,
}

#[tauri::command]
pub async fn debug_error(
    args: DebugErrorArgs,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<DebugResult, String> {
    let resolved = resolve_error(&args, &store)?;
    let (source_path, source_line, code_window) =
        extract_code_context(&args.project_root, &resolved);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = build_user_prompt(
        &resolved.message,
        resolved.stack.as_deref(),
        source_path.as_deref(),
        source_line,
        code_window.as_deref(),
    );

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.2),
    };

    let raw = run_with_timeout(client, req).await?;
    let parsed = parse_response(&raw);

    Ok(DebugResult {
        error_source: args.error_source,
        error_summary: resolved.summary,
        root_cause: parsed.root_cause,
        suggested_fix: parsed.suggested_fix,
        code_patch: parsed.code_patch,
        confidence: parsed.confidence,
        source_path,
        source_line,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn resolve_error(args: &DebugErrorArgs, store: &TracingStore) -> Result<ResolvedError, String> {
    match args.error_source.as_str() {
        "recent_crash" => {
            let conn = store.shared_connection();
            let row = crash::recent_crashes(&conn, 1).map_err(|e| e.to_string())?
                .into_iter().next()
                .ok_or_else(|| "no recent crashes — nothing to debug".to_string())?;
            let summary = format!("[crash:{}] {}", row.kind, truncate_one_line(&row.message, 120));
            Ok(ResolvedError { summary, message: row.message, stack: row.stack })
        }
        "recent_issue" => {
            let row = store.recent_issues(1).map_err(|e| e.to_string())?
                .into_iter().next()
                .ok_or_else(|| "no recent issues — nothing to debug".to_string())?;
            let class = row.error_class.as_deref().unwrap_or("issue");
            let summary = format!("[{class}] {}", truncate_one_line(&row.message, 120));
            Ok(ResolvedError { summary, message: row.message, stack: None })
        }
        "last_test_failure" => {
            let path = test_failure_path()
                .ok_or_else(|| "could not resolve ~/.cortex".to_string())?;
            if !path.is_file() {
                return Err("no recent test failure recorded yet — run /test first".into());
            }
            let blob = fs::read_to_string(&path)
                .map_err(|e| format!("read {} failed: {e}", path.display()))?;
            parse_test_failure(&blob)
        }
        "chat_error" | "manual" => {
            let text = args.error_text.as_deref().map(str::trim).unwrap_or("");
            if text.is_empty() {
                return Err(format!(
                    "no error text supplied for source={} — paste an error first",
                    args.error_source,
                ));
            }
            let summary = format!("[{}] {}", args.error_source, truncate_one_line(text, 120));
            Ok(ResolvedError {
                summary,
                message: text.to_string(),
                stack: args.error_stack.clone(),
            })
        }
        other => Err(format!("unknown error_source: {other}")),
    }
}

fn parse_test_failure(blob: &str) -> Result<ResolvedError, String> {
    // Accept both `TestResult` shape and a flat `{message,stack,location}`.
    let v: serde_json::Value = serde_json::from_str(blob)
        .map_err(|e| format!("last-test-failure.json is not valid JSON: {e}"))?;

    if let Some(failures) = v.get("failures").and_then(|x| x.as_array()) {
        if let Some(first) = failures.first() {
            let name = first.get("name").and_then(|x| x.as_str()).unwrap_or("test");
            let message = first.get("message").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let location = first.get("location").and_then(|x| x.as_str());
            let stack = location.map(|l| format!("at {l}"));
            let summary = format!("[test:{name}] {}", truncate_one_line(&message, 120));
            return Ok(ResolvedError { summary, message, stack });
        }
    }

    // Fallback flat shape.
    let message = v.get("message")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "last-test-failure.json missing `message`".to_string())?
        .to_string();
    let stack = v.get("stack").and_then(|x| x.as_str()).map(str::to_string);
    let summary = format!("[test] {}", truncate_one_line(&message, 120));
    Ok(ResolvedError { summary, message, stack })
}

fn test_failure_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cortex").join("last-test-failure.json"))
}

/// Find `path:line` in message+stack, read a ~CONTEXT_LINES window. Falls back
/// to `None`s when no file is readable — the model still gets the error text.
fn extract_code_context(
    project_root: &str,
    err: &ResolvedError,
) -> (Option<String>, Option<usize>, Option<String>) {
    let haystack = match &err.stack {
        Some(s) => format!("{}\n{}", err.message, s),
        None => err.message.clone(),
    };
    let Some((path_str, line)) = find_path_line(&haystack, project_root) else {
        return (None, None, None);
    };
    let path = PathBuf::from(&path_str);
    let Ok(raw) = fs::read_to_string(&path) else {
        return (Some(path_str), Some(line), None);
    };
    let window = slice_window(&raw, line, CONTEXT_LINES, FILE_WINDOW_BYTES);
    (Some(path_str), Some(line), Some(window))
}

/// Scan for `<path>:<line>(:<col>)?` tokens. Prefers paths that resolve under
/// `project_root`; falls back to the first plausible match.
///
/// SECURITY: the haystack is fully user-controlled (manual/chat_error sources
/// paste arbitrary text), and the resolved path is later read and shipped to
/// the Cortex Gateway. We therefore only ever return a *readable* path that is
/// confined to `project_root`. Absolute tokens and `..` traversal that escape
/// the root are rejected so a pasted `/etc/passwd:1` can't exfiltrate files.
fn find_path_line(haystack: &str, project_root: &str) -> Option<(String, usize)> {
    let root = Path::new(project_root);
    let mut best: Option<(String, usize)> = None;
    for token in haystack.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '"' | '\'' | ',')) {
        let token = token.trim_matches(|c: char| matches!(c, '`' | '<' | '>' | '|' | ';'));
        if token.len() < 4 || !token.contains(':') { continue; }
        // rsplitn keeps Windows drive letters intact.
        let parts: Vec<&str> = token.rsplitn(3, ':').collect();
        let (path_part, line_part) = match parts.as_slice() {
            [maybe_col, line_str, path] if maybe_col.parse::<usize>().is_ok() && line_str.parse::<usize>().is_ok() => (*path, *line_str),
            [line_str, path, ..] if line_str.parse::<usize>().is_ok() => (*path, *line_str),
            _ => continue,
        };
        let Ok(line) = line_part.parse::<usize>() else { continue };
        if line == 0 { continue; }
        // Only resolve paths confined to the project root; never read absolute
        // or `..`-escaping tokens supplied in user-controlled error text. An
        // out-of-bounds token is dropped entirely (not even kept as a fallback)
        // so the caller can never `fs::read_to_string` it.
        let Some(confined) = confine_to_root(root, path_part) else { continue };
        if confined.is_file() {
            return Some((confined.to_string_lossy().into_owned(), line));
        }
        if best.is_none() {
            best = Some((confined.to_string_lossy().into_owned(), line));
        }
    }
    best
}

/// Resolve `path_part` against `root`, returning the resolved path only if it
/// stays inside `root`. Rejects absolute paths and any `..` traversal that
/// would escape the project root. Returns `None` when the path is unsafe.
fn confine_to_root(root: &Path, path_part: &str) -> Option<PathBuf> {
    let rel = Path::new(path_part);
    if rel.is_absolute() {
        return None;
    }
    // Prefer canonical containment when both sides resolve on disk (handles
    // symlinks); otherwise fall back to lexical normalization so a non-existent
    // (but in-bounds) path can still be classified before the `is_file` check.
    let candidate = root.join(rel);
    let root_canon = root.canonicalize().ok();
    if let (Some(root_canon), Ok(cand_canon)) = (&root_canon, candidate.canonicalize()) {
        return cand_canon.starts_with(root_canon).then_some(cand_canon);
    }
    // Lexical check: reject if any `..` component pops above the root.
    let mut depth: i32 = 0;
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            // Prefix/RootDir can't appear in a relative path; bail defensively.
            _ => return None,
        }
    }
    Some(candidate)
}

fn slice_window(body: &str, line: usize, ctx: usize, byte_cap: usize) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() { return String::new(); }
    let max_line = lines.len();
    let start = line.saturating_sub(ctx / 2).max(1);
    let end = (line + ctx / 2).min(max_line);
    let mut out = String::new();
    for (i, ln) in lines[start - 1..end].iter().enumerate() {
        // 1-indexed line numbers so the model can refer to them in the patch.
        let lineno = start + i;
        let marker = if lineno == line { ">> " } else { "   " };
        out.push_str(&format!("{marker}{lineno:>5} | {ln}\n"));
        if out.len() > byte_cap {
            out.push_str("[truncated]\n");
            break;
        }
    }
    out
}

fn build_user_prompt(
    message: &str,
    stack: Option<&str>,
    source_path: Option<&str>,
    source_line: Option<usize>,
    code_window: Option<&str>,
) -> String {
    let mut p = format!("--- ERROR ---\n{}\n", message.trim());
    if let Some(s) = stack.map(str::trim).filter(|s| !s.is_empty()) {
        p.push_str(&format!("--- STACK ---\n{s}\n"));
    }
    if let (Some(path), Some(line)) = (source_path, source_line) {
        p.push_str(&format!("--- SOURCE: {path}:{line} ---\n"));
        p.push_str(code_window.unwrap_or("(file could not be read)\n"));
    }
    p.push_str("--- END ---\n\nReturn ONLY the JSON object — root_cause, suggested_fix, code_patch (unified diff or empty string), confidence.");
    p
}

async fn run_with_timeout(
    client: GatewayClient,
    req: ChatCompletionRequest,
) -> Result<String, String> {
    let started = Instant::now();
    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);

    let stream_fut = async move {
        let _ = client.chat_completion_stream(req, tx).await;
    };

    let collect_fut = async {
        let mut buf = String::new();
        while let Some(item) = rx.recv().await {
            match item {
                StreamItem::Delta(s) => buf.push_str(&s),
                StreamItem::Done { .. } => break,
            }
        }
        buf
    };

    match tokio::time::timeout(TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err(format!(
            "debugger timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

struct ParsedDebug {
    root_cause: String,
    suggested_fix: String,
    code_patch: String,
    confidence: f64,
}

/// Permissive parse: strip fence, locate first `{…}` block, pull fields.
/// Falls back to prose-in-`root_cause` with low confidence on malformed JSON.
fn parse_response(raw: &str) -> ParsedDebug {
    let cleaned = strip_fence(raw.trim());
    if let Some(obj) = first_json_object(cleaned) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) {
            return ParsedDebug {
                root_cause: pick_string(&v, "root_cause").unwrap_or_else(|| cleaned.to_string()),
                suggested_fix: pick_string(&v, "suggested_fix").unwrap_or_default(),
                code_patch: pick_string(&v, "code_patch").unwrap_or_default(),
                confidence: v.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.4).clamp(0.0, 1.0),
            };
        }
    }
    ParsedDebug {
        root_cause: cleaned.to_string(),
        suggested_fix: String::new(),
        code_patch: String::new(),
        confidence: 0.2,
    }
}

fn pick_string(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn strip_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // Drop optional language tag on the same line.
        let body = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = body.rfind("```") {
            return body[..end].trim();
        }
    }
    s
}

fn first_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    // Walk to the matching brace; track string state so `}` inside a literal
    // doesn't trip us.
    let (mut depth, mut in_string, mut escape) = (0_i32, false, false);
    for (i, &b) in s.as_bytes().iter().enumerate().skip(start) {
        if escape { escape = false; continue; }
        if in_string {
            match b { b'\\' => escape = true, b'"' => in_string = false, _ => {} }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => { depth -= 1; if depth == 0 { return Some(&s[start..=i]); } }
            _ => {}
        }
    }
    None
}

fn truncate_one_line(s: &str, limit: usize) -> String {
    let single = s.lines().next().unwrap_or("").trim();
    if single.chars().count() <= limit {
        return single.to_string();
    }
    let mut out: String = single.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_path_line_picks_first_existing_in_stack() {
        // Use an obviously-fake path so we exercise the fallback branch.
        let stack = "  at fakeFn (nope/does-not-exist.ts:42:7)";
        let out = find_path_line(stack, "/tmp");
        // Falls back to the plausible-but-not-found token, now confined to root.
        assert_eq!(out, Some(("/tmp/nope/does-not-exist.ts".to_string(), 42)));
    }

    #[test]
    fn find_path_line_handles_no_match() {
        assert_eq!(find_path_line("just a sentence", "/tmp"), None);
    }

    #[test]
    fn find_path_line_never_reads_absolute_tokens() {
        // A real, readable absolute path pasted into user-controlled error text
        // must NOT be resolved/read — only confined project-relative paths are.
        let abs = if cfg!(windows) { "C:\\Windows\\win.ini:1" } else { "/etc/hostname:1" };
        // Root is unrelated; the absolute token resolves on disk but is out of bounds.
        assert_eq!(find_path_line(abs, "/tmp/some-project"), None);
    }

    #[test]
    fn find_path_line_rejects_dotdot_traversal() {
        // `..` that escapes the project root is never returned as a real path.
        let stack = "  at fn (../../../../etc/hostname:1:1)";
        assert_eq!(find_path_line(stack, "/tmp/some-project"), None);
    }

    #[test]
    fn confine_to_root_blocks_absolute_and_escape() {
        let root = Path::new("/tmp/proj");
        assert!(confine_to_root(root, "/etc/passwd").is_none());
        assert!(confine_to_root(root, "../../etc/passwd").is_none());
        // In-bounds relative path is allowed (existence is checked separately).
        assert_eq!(
            confine_to_root(root, "src/main.rs"),
            Some(PathBuf::from("/tmp/proj/src/main.rs"))
        );
    }

    #[test]
    fn slice_window_marks_and_caps() {
        let body = (1..=20).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let out = slice_window(&body, 10, 4, 4096);
        assert!(out.contains(">>    10 | line10"));
        assert!(out.contains("    8 | line8"));
        let big = (1..=500).map(|i| format!("line{i:04}")).collect::<Vec<_>>().join("\n");
        let capped = slice_window(&big, 250, 200, 256);
        assert!(capped.ends_with("[truncated]\n"));
    }

    #[test]
    fn parse_response_extracts_clean_json() {
        let raw = r#"{"root_cause":"x","suggested_fix":"y","code_patch":"--- a\n","confidence":0.8}"#;
        let p = parse_response(raw);
        assert_eq!(p.root_cause, "x");
        assert_eq!(p.code_patch, "--- a\n");
        assert!((p.confidence - 0.8).abs() < 1e-9);
    }

    #[test]
    fn parse_response_unwraps_fenced_json_and_clamps_confidence() {
        let raw = "```json\n{\"root_cause\":\"a\",\"suggested_fix\":\"b\",\"code_patch\":\"\",\"confidence\":7.5}\n```";
        let p = parse_response(raw);
        assert_eq!(p.root_cause, "a");
        assert!(p.confidence <= 1.0);
    }

    #[test]
    fn parse_response_falls_back_on_prose() {
        let p = parse_response("Sorry, I couldn't determine the cause.");
        assert!(p.root_cause.contains("Sorry"));
        assert_eq!(p.code_patch, "");
        assert!(p.confidence <= 0.3);
    }

    #[test]
    fn first_json_object_handles_nested_braces() {
        let s = "noise {\"a\": \"x{y}z\", \"b\": 1} trailing";
        let obj = first_json_object(s).unwrap();
        assert!(obj.contains("x{y}z"));
    }

    #[test]
    fn parse_test_failure_handles_both_shapes() {
        let blob1 = r#"{"failures":[{"name":"my_test","message":"assert failed","location":"src/a.rs:12"}]}"#;
        let r1 = parse_test_failure(blob1).unwrap();
        assert_eq!(r1.message, "assert failed");
        assert_eq!(r1.stack.as_deref(), Some("at src/a.rs:12"));
        let blob2 = r#"{"message":"boom","stack":"at foo.ts:3:1"}"#;
        let r2 = parse_test_failure(blob2).unwrap();
        assert_eq!(r2.message, "boom");
    }

    #[test]
    fn build_user_prompt_assembles_sections() {
        let p = build_user_prompt("boom", Some("at foo.ts:3"), Some("foo.ts"), Some(3), Some("   3 | code\n"));
        assert!(p.contains("--- ERROR ---"));
        assert!(p.contains("--- STACK ---"));
        assert!(p.contains("--- SOURCE: foo.ts:3 ---"));
        let minimal = build_user_prompt("err", None, None, None, None);
        assert!(!minimal.contains("--- STACK ---"));
        assert!(!minimal.contains("--- SOURCE:"));
    }

    #[test]
    fn strip_fence_handles_plain_and_fenced() {
        assert_eq!(strip_fence("hello"), "hello");
        assert_eq!(strip_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
    }

    #[test]
    fn truncate_one_line_caps_long_strings() {
        assert_eq!(truncate_one_line("a\nb", 5), "a");
        let out = truncate_one_line(&"x".repeat(200), 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }
}
