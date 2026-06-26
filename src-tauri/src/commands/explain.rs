//! AI code explainer.
//!
//! Reads a single source file from disk (capped at 64 KiB), optionally slices
//! it to a `[line_start, line_end]` range, then pipes the result through
//! the gateway with an audience-aware prompt and returns markdown the UI renders
//! via react-markdown. Mirrors the streaming-collect + timeout pattern from
//! [`super::doc_gen`] / [`super::refactor_suggester`].
//!
//! The user-facing flow is `/explain [path]` (defaults to the editor path,
//! whole file) and `/why [start:end]` (defaults to editor path, specific
//! lines). The frontend modal re-fires the command when the audience radio
//! or line-range inputs change, so this entry point is intentionally cheap
//! beyond the gateway call itself.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Hard cap on the source blob we send to the model. 64 KiB keeps even a
/// large monolithic file under context limits without blowing latency.
const FILE_LIMIT_BYTES: usize = 64 * 1024;

/// 45s wall clock on the gateway call. An explanation can include a step-by-
/// step walkthrough so it's heavier than commit-message generation but
/// lighter than full doc rewriting.
const TIMEOUT: Duration = Duration::from_secs(45);

const SYSTEM_PROMPT_TEMPLATE: &str = "You are a code explainer for a {AUDIENCE} reader. \
Given this {LANGUAGE} code, return: \
(a) a 1-sentence summary of what it does, \
(b) a step-by-step explanation, \
(c) any non-obvious tricks or gotchas, \
(d) related concepts the reader should know. \
Return as markdown.";

#[derive(Debug, Serialize, Clone)]
pub struct ExplainResult {
    pub path: String,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
    pub language: String,
    pub audience: String,
    pub markdown: String,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn explain_code(
    path: String,
    line_start: Option<usize>,
    line_end: Option<usize>,
    audience: Option<String>,
    state: State<'_, AppState>,
) -> Result<ExplainResult, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("not a file: {path}"));
    }

    let raw = fs::read_to_string(&p)
        .map_err(|e| format!("read {} failed: {e}", p.display()))?;
    let body_full = truncate(raw, FILE_LIMIT_BYTES);
    let (snippet, resolved_start, resolved_end) =
        slice_lines(&body_full, line_start, line_end);
    if snippet.trim().is_empty() {
        return Err("selected range is empty — nothing to explain".into());
    }

    let language = detect_language(&p);
    let resolved_audience = resolve_audience(audience.as_deref());

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let system_prompt = SYSTEM_PROMPT_TEMPLATE
        .replace("{AUDIENCE}", &resolved_audience)
        .replace("{LANGUAGE}", &language);
    let user_prompt = build_user_prompt(
        &path,
        &language,
        resolved_start,
        resolved_end,
        &snippet,
    );

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: system_prompt },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.3),
    };

    let raw_out = run_with_timeout(client, req).await?;
    let markdown = sanitize(&raw_out);
    if markdown.trim().is_empty() {
        return Err("The gateway returned an empty explanation".into());
    }

    Ok(ExplainResult {
        path,
        line_start: resolved_start,
        line_end: resolved_end,
        language,
        audience: resolved_audience,
        markdown,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
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
            "explainer timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

fn build_user_prompt(
    path: &str,
    language: &str,
    line_start: Option<usize>,
    line_end: Option<usize>,
    body: &str,
) -> String {
    let range_label = match (line_start, line_end) {
        (Some(s), Some(e)) => format!("Lines: {s}-{e}\n"),
        (Some(s), None) => format!("Lines: {s}-…\n"),
        _ => String::new(),
    };
    format!(
        "Path: {path}\nLanguage: {language}\n{range_label}\
         --- CODE ---\n{body}\n--- END CODE ---\n\n\
         Return ONLY markdown. Use the four-section structure (summary, \
         step-by-step, gotchas, related concepts).",
    )
}

/// Slice the source into `[line_start, line_end]` (1-indexed, inclusive).
/// Returns the snippet plus the *actually used* line numbers (clamped to the
/// file length so the UI can echo them back). `None`/`None` returns the
/// whole body and `None` line numbers.
fn slice_lines(
    body: &str,
    line_start: Option<usize>,
    line_end: Option<usize>,
) -> (String, Option<usize>, Option<usize>) {
    if line_start.is_none() && line_end.is_none() {
        return (body.to_string(), None, None);
    }
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return (body.to_string(), None, None);
    }
    let max_line = lines.len();
    let start = line_start.unwrap_or(1).max(1).min(max_line);
    let end = line_end.unwrap_or(max_line).max(start).min(max_line);
    let snippet = lines[start - 1..end].join("\n");
    (snippet, Some(start), Some(end))
}

/// Fold the user-supplied audience into a canonical key. `None`, empty, or
/// unrecognised values fall back to `"beginner"` — the safest default since
/// the explainer can always assume less without harm.
fn resolve_audience(audience: Option<&str>) -> String {
    let key = audience
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match key.as_str() {
        "intermediate" | "mid" => "intermediate".to_string(),
        "expert" | "advanced" | "senior" => "expert".to_string(),
        _ => "beginner".to_string(),
    }
}

/// Best-effort language label from the file extension. Trimmed copy of the
/// table in `commands::doc_gen::detect_language` — kept inline to avoid
/// cross-module coupling on what is logically a per-command lookup.
fn detect_language(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "scala" => "Scala",
        "sh" | "bash" => "Bash",
        "lua" => "Lua",
        "sql" => "SQL",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "" => "plain text",
        other => return other.to_string(),
    }
    .to_string()
}

fn truncate(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[truncated — file exceeded 64 KiB]");
    s
}

/// Light cleanup on model output. Strips a single outer fence and a
/// stray "Here is …" preamble line — leaves the markdown body otherwise
/// untouched so headings / lists / code fences inside survive intact.
fn sanitize(raw: &str) -> String {
    let mut s = raw.trim_end_matches('\n').to_string();
    let trimmed = s.trim_start();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }
    if let Some(rest) = s.strip_prefix("Here is") {
        if let Some(idx) = rest.find('\n') {
            s = rest[idx + 1..].trim_start().to_string();
        }
    }
    s
}

// ----- Save-to-memory ----------------------------------------------------
//
// Writes the explanation markdown into `~/Documents/Cortex Brain/
// explanations/<YYYY-MM-DD>-<slug>.md`. Distinct from `import_to_brain`
// (which targets `imports/`) so the Brain panel can surface explanations as
// their own knowledge category. YAML frontmatter records the source path
// + line range so downstream walkers don't have to re-derive them.

#[derive(Debug, Serialize)]
pub struct ExplainSaveResult {
    pub written_path: PathBuf,
    pub bytes: usize,
}

#[tauri::command]
pub async fn save_explanation(
    path: String,
    line_start: Option<usize>,
    line_end: Option<usize>,
    language: String,
    audience: String,
    markdown: String,
) -> Result<ExplainSaveResult, String> {
    let brain_root = brain_dir().ok_or_else(|| "could not resolve ~/Documents".to_string())?;
    let target_dir = brain_root.join("explanations");
    fs::create_dir_all(&target_dir)
        .map_err(|e| format!("create explanations dir failed: {e}"))?;

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let stem = Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("explanation");
    let range_tag = match (line_start, line_end) {
        (Some(s), Some(e)) => format!("-L{s}-{e}"),
        (Some(s), None) => format!("-L{s}"),
        _ => String::new(),
    };
    let slug = slugify(&format!("{stem}{range_tag}"));
    let filename = format!("{date}-{slug}.md");
    let written_path = target_dir.join(&filename);

    let now_iso = chrono::Utc::now().to_rfc3339();
    let range_yaml = match (line_start, line_end) {
        (Some(s), Some(e)) => format!("line_start: {s}\nline_end: {e}\n"),
        _ => String::new(),
    };
    let frontmatter = format!(
        "---\nkind: explanation\ngenerated_at: {now_iso}\nsource: {}\nlanguage: {}\naudience: {}\n{range_yaml}---\n\n",
        yaml_escape(&path),
        yaml_escape(&language),
        yaml_escape(&audience),
    );
    let body = format!("{frontmatter}{markdown}");
    let bytes = body.as_bytes().len();
    fs::write(&written_path, &body)
        .map_err(|e| format!("write {} failed: {e}", written_path.display()))?;

    Ok(ExplainSaveResult { written_path, bytes })
}

fn brain_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Documents").join("Cortex Brain"))
}

/// Lowercase, ASCII-alnum-and-dash slug capped at 60 chars. Mirrors the
/// helper in `brain_import` — kept local to dodge a cross-module pub export.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 60 {
        out.truncate(60);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "explanation".into()
    } else {
        out
    }
}

/// Minimal YAML scalar escaping — strip newlines, quote when structural
/// characters are present. Good enough for paths/language/audience strings.
fn yaml_escape(input: &str) -> String {
    let cleaned = input.replace(['\n', '\r'], " ");
    let needs_quote = cleaned.chars().any(|c| {
        matches!(c, ':' | '#' | '{' | '}' | '[' | ']' | ',' | '&' | '*' | '!' | '|' | '>' | '\'' | '"' | '%' | '@' | '`')
    });
    if needs_quote {
        let escaped = cleaned.replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_audience_defaults_to_beginner() {
        assert_eq!(resolve_audience(None), "beginner");
        assert_eq!(resolve_audience(Some("")), "beginner");
        assert_eq!(resolve_audience(Some("klingon")), "beginner");
        assert_eq!(resolve_audience(Some("beginner")), "beginner");
    }

    #[test]
    fn resolve_audience_recognises_intermediate_and_expert() {
        assert_eq!(resolve_audience(Some("intermediate")), "intermediate");
        assert_eq!(resolve_audience(Some("MID")), "intermediate");
        assert_eq!(resolve_audience(Some("expert")), "expert");
        assert_eq!(resolve_audience(Some("advanced")), "expert");
        assert_eq!(resolve_audience(Some("Senior")), "expert");
    }

    #[test]
    fn slice_lines_returns_whole_body_when_no_range() {
        let body = "a\nb\nc";
        let (out, s, e) = slice_lines(body, None, None);
        assert_eq!(out, body);
        assert_eq!(s, None);
        assert_eq!(e, None);
    }

    #[test]
    fn slice_lines_clamps_to_file_bounds() {
        let body = "a\nb\nc\nd";
        let (out, s, e) = slice_lines(body, Some(2), Some(3));
        assert_eq!(out, "b\nc");
        assert_eq!(s, Some(2));
        assert_eq!(e, Some(3));
        // Over-shoot end → clamped to last line.
        let (out2, _, e2) = slice_lines(body, Some(3), Some(99));
        assert_eq!(out2, "c\nd");
        assert_eq!(e2, Some(4));
        // Start past EOF → clamped to last line, end snaps to start.
        let (_, s3, e3) = slice_lines(body, Some(99), None);
        assert_eq!(s3, Some(4));
        assert_eq!(e3, Some(4));
    }

    #[test]
    fn slice_lines_handles_only_start() {
        let body = "a\nb\nc\nd";
        let (out, s, e) = slice_lines(body, Some(2), None);
        assert_eq!(out, "b\nc\nd");
        assert_eq!(s, Some(2));
        assert_eq!(e, Some(4));
    }

    #[test]
    fn detect_language_known_extensions() {
        assert_eq!(detect_language(Path::new("foo.rs")), "Rust");
        assert_eq!(detect_language(Path::new("foo.tsx")), "TypeScript");
        assert_eq!(detect_language(Path::new("foo.py")), "Python");
        assert_eq!(detect_language(Path::new("foo.md")), "Markdown");
        assert_eq!(detect_language(Path::new("Makefile")), "plain text");
    }

    #[test]
    fn truncate_caps_long_files() {
        let blob = "x".repeat(FILE_LIMIT_BYTES + 200);
        let out = truncate(blob, FILE_LIMIT_BYTES);
        assert!(out.contains("[truncated"));
        assert!(out.len() < FILE_LIMIT_BYTES + 100);
    }

    #[test]
    fn sanitize_strips_fenced_block() {
        let raw = "```markdown\n# explanation\nstuff\n```";
        assert_eq!(sanitize(raw), "# explanation\nstuff");
    }

    #[test]
    fn sanitize_strips_here_is_preamble() {
        let raw = "Here is the explanation:\n# title\nbody";
        assert_eq!(sanitize(raw), "# title\nbody");
    }

    #[test]
    fn sanitize_passes_through_plain_markdown() {
        let raw = "# title\n\n- item\n";
        assert_eq!(sanitize(raw), "# title\n\n- item");
    }

    #[test]
    fn build_user_prompt_includes_range_when_given() {
        let p = build_user_prompt("a.rs", "Rust", Some(10), Some(20), "fn x() {}");
        assert!(p.contains("Path: a.rs"));
        assert!(p.contains("Language: Rust"));
        assert!(p.contains("Lines: 10-20"));
        assert!(p.contains("fn x() {}"));
    }

    #[test]
    fn build_user_prompt_omits_range_when_absent() {
        let p = build_user_prompt("a.rs", "Rust", None, None, "fn x() {}");
        assert!(!p.contains("Lines:"));
    }

    #[test]
    fn slugify_handles_paths_and_ranges() {
        assert_eq!(slugify("foo-L10-20"), "foo-l10-20");
        assert_eq!(slugify("Cargo.toml"), "cargo-toml");
        assert_eq!(slugify("!!! "), "explanation");
    }

    #[test]
    fn yaml_escape_quotes_problematic_strings() {
        assert_eq!(yaml_escape("plain"), "plain");
        let escaped = yaml_escape("/abs/path: weird");
        assert!(escaped.starts_with('"') && escaped.ends_with('"'));
    }
}
