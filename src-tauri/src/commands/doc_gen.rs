//! AI documentation generator.
//!
//! Reads a single source file from disk (capped at 64 KiB), pipes it through
//! the gateway with a language-aware system prompt asking for inline documentation
//! comments, and returns the original + the documented version so the UI can
//! render them side-by-side. Mirrors the streaming-collect + timeout pattern
//! from [`super::commit_suggest`] / [`super::refactor_suggester`].
//!
//! The user-facing flow is `/docgen [path]`. The frontend modal calls
//! `generate_docs` with the path and an optional `style` arg ("rust",
//! "jsdoc", "python", "markdown", "generic", or omitted to auto-detect from
//! extension). The command is read-only — saving the result back to disk is
//! the caller's responsibility (`save_file_text`), gated by an explicit user
//! action in the modal.

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

/// 45s wall clock on the gateway call. Doc generation has to rewrite the
/// whole file with new comments interleaved, so it's heavier than refactor
/// suggesting — give it a bit more headroom.
const TIMEOUT: Duration = Duration::from_secs(45);

const SYSTEM_PROMPT: &str = "You are a documentation generator. Given the file below, \
generate idiomatic {STYLE_LABEL} docs. Return the FULL file with docs inline. \
Preserve all original code unchanged — only add/edit comments. \
No fences, no preamble.";

#[derive(Debug, Serialize, Clone)]
pub struct DocResult {
    pub path: String,
    pub language: String,
    pub style: String,
    pub original: String,
    pub with_docs: String,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn generate_docs(
    path: String,
    style: Option<String>,
    state: State<'_, AppState>,
) -> Result<DocResult, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("not a file: {path}"));
    }

    let raw = fs::read_to_string(&p)
        .map_err(|e| format!("read {} failed: {e}", p.display()))?;
    let original = raw.clone();
    let body = truncate(raw, FILE_LIMIT_BYTES);
    if body.trim().is_empty() {
        return Err("file is empty — nothing to document".into());
    }

    let language = detect_language(&p);
    let resolved_style = resolve_style(style.as_deref(), &language);
    let style_label = style_label(&resolved_style);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let system_prompt = SYSTEM_PROMPT.replace("{STYLE_LABEL}", style_label);
    let user_prompt = build_user_prompt(&path, &language, &resolved_style, &body);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: system_prompt },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.2),
    };

    let raw_out = run_with_timeout(client, req).await?;
    let with_docs = sanitize(&raw_out);
    if with_docs.trim().is_empty() {
        return Err("The gateway returned an empty document".into());
    }

    Ok(DocResult {
        path,
        language,
        style: resolved_style,
        original,
        with_docs,
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
            "doc generator timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

fn build_user_prompt(path: &str, language: &str, style: &str, body: &str) -> String {
    format!(
        "Path: {path}\nLanguage: {language}\nDoc style: {style}\n\n\
         --- FILE ---\n{body}\n--- END FILE ---\n\n\
         Return ONLY the full documented file body. No fences, no preamble.",
    )
}

/// Resolve the user-supplied style arg into a canonical style key. `None`,
/// `Some("auto")`, or `Some("")` fall back to a language-driven default.
fn resolve_style(style: Option<&str>, language: &str) -> String {
    let trimmed = style.map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
    let pick = match trimmed.as_str() {
        "" | "auto" => default_style_for(language),
        "rust" | "rustdoc" => "rust",
        "ts" | "tsx" | "js" | "jsx" | "javascript" | "typescript" | "jsdoc" => "jsdoc",
        "py" | "python" | "docstring" => "python",
        "md" | "markdown" => "markdown",
        "generic" | "comment" => "generic",
        // Unknown style — fall back to language default rather than failing.
        _ => default_style_for(language),
    };
    pick.to_string()
}

fn default_style_for(language: &str) -> &'static str {
    match language {
        "Rust" => "rust",
        "TypeScript" | "JavaScript" => "jsdoc",
        "Python" => "python",
        "Markdown" => "markdown",
        _ => "generic",
    }
}

fn style_label(style: &str) -> &'static str {
    match style {
        "rust" => "`///` rustdoc comments with usage examples",
        "jsdoc" => "JSDoc with `@param` and `@returns` tags",
        "python" => "PEP 257 docstrings with Args/Returns sections",
        "markdown" => "outline summary headers + section descriptions",
        _ => "a concise header comment + per-section comments",
    }
}

/// Best-effort language label from the file extension. Mirrors the table in
/// `commands::refactor_suggester` so styles map predictably.
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

/// Strip stray fenced blocks or preamble lines the model may emit despite the
/// prompt. Conservative: only peels a single outer fence and a "Here is …"
/// lead-in — the bulk of the file is left untouched.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_known_extensions() {
        assert_eq!(detect_language(Path::new("foo.rs")), "Rust");
        assert_eq!(detect_language(Path::new("foo.tsx")), "TypeScript");
        assert_eq!(detect_language(Path::new("foo.py")), "Python");
        assert_eq!(detect_language(Path::new("foo.md")), "Markdown");
        assert_eq!(detect_language(Path::new("Makefile")), "plain text");
    }

    #[test]
    fn resolve_style_defaults_per_language() {
        assert_eq!(resolve_style(None, "Rust"), "rust");
        assert_eq!(resolve_style(Some("auto"), "TypeScript"), "jsdoc");
        assert_eq!(resolve_style(Some(""), "Python"), "python");
        assert_eq!(resolve_style(None, "Markdown"), "markdown");
        assert_eq!(resolve_style(None, "Go"), "generic");
    }

    #[test]
    fn resolve_style_honours_user_override() {
        assert_eq!(resolve_style(Some("jsdoc"), "Rust"), "jsdoc");
        assert_eq!(resolve_style(Some("PYTHON"), "Rust"), "python");
        // Unknown style → fall back to language default rather than failing.
        assert_eq!(resolve_style(Some("klingon"), "Rust"), "rust");
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
        let raw = "```rust\nfn main() {}\n```";
        assert_eq!(sanitize(raw), "fn main() {}");
    }

    #[test]
    fn sanitize_strips_here_is_preamble() {
        let raw = "Here is the documented file:\nfn main() {}";
        assert_eq!(sanitize(raw), "fn main() {}");
    }

    #[test]
    fn sanitize_passes_through_plain_body() {
        let raw = "/// docs\nfn main() {}\n";
        assert_eq!(sanitize(raw), "/// docs\nfn main() {}");
    }

    #[test]
    fn build_user_prompt_includes_metadata() {
        let p = build_user_prompt("a.rs", "Rust", "rust", "fn x() {}");
        assert!(p.contains("Path: a.rs"));
        assert!(p.contains("Language: Rust"));
        assert!(p.contains("Doc style: rust"));
        assert!(p.contains("fn x() {}"));
    }

    #[test]
    fn style_label_covers_known_styles() {
        assert!(style_label("rust").contains("rustdoc"));
        assert!(style_label("jsdoc").contains("JSDoc"));
        assert!(style_label("python").contains("docstring"));
        assert!(style_label("markdown").contains("outline"));
        assert!(style_label("generic").contains("header"));
    }
}
