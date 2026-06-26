//! AI unit test generator.
//!
//! Reads a single source file (capped at 64 KiB), optionally narrows it down
//! to a single function by name, then pipes the result through the gateway with a
//! framework-aware system prompt asking for a happy-path test, two edge
//! cases, and one error case. Returns the generated test code plus a
//! suggested on-disk location so the frontend modal can offer a one-click
//! save. Mirrors the streaming-collect + timeout pattern from
//! [`super::doc_gen`] / [`super::refactor_suggester`].
//!
//! User flow is `/gentest [path] [::function-name]`. The command is
//! read-only; persisting the tests is the caller's job
//! (`save_file_text(suggested_test_path, test_code)`).

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// 64 KiB cap on the source blob we send the model. Same budget as doc_gen —
/// keeps even monolithic files inside context limits.
const FILE_LIMIT_BYTES: usize = 64 * 1024;

/// 45s gateway wall-clock. Test generation has to reason about the call
/// surface AND write fresh test scaffolding, so it's at least as heavy as
/// doc generation.
const TIMEOUT: Duration = Duration::from_secs(45);

const SYSTEM_PROMPT_TEMPLATE: &str =
    "You are a test generator. Given the {LANGUAGE} code below, generate \
{FRAMEWORK} unit tests covering: happy path, 2 edge cases, and 1 error case. \
Return ONLY the test code — no preamble, no fences.";

#[derive(Debug, Serialize, Clone)]
pub struct TestGenResult {
    pub path: String,
    pub function_name: Option<String>,
    pub language: String,
    pub framework: String,
    pub test_code: String,
    pub suggested_test_path: PathBuf,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn generate_tests(
    path: String,
    function_name: Option<String>,
    framework: Option<String>,
    state: State<'_, AppState>,
) -> Result<TestGenResult, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("not a file: {path}"));
    }

    let raw = fs::read_to_string(&p)
        .map_err(|e| format!("read {} failed: {e}", p.display()))?;

    let language = detect_language(&p);
    let resolved_fn = function_name
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let fw = resolve_framework(framework.as_deref(), &language, &p)?;

    // If a specific function was requested, narrow the blob down to its
    // signature + body. Falls back to the full file on extraction miss so
    // the user still gets *something* useful.
    let scoped = match resolved_fn.as_deref() {
        Some(name) => extract_function(&raw, &language, name).unwrap_or(raw.clone()),
        None => raw.clone(),
    };
    let body = truncate(scoped, FILE_LIMIT_BYTES);
    if body.trim().is_empty() {
        return Err("file (or function) is empty — nothing to test".into());
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let system_prompt = SYSTEM_PROMPT_TEMPLATE
        .replace("{LANGUAGE}", &language)
        .replace("{FRAMEWORK}", &fw);
    let user_prompt = build_user_prompt(&path, &language, &fw, resolved_fn.as_deref(), &body);

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
    let test_code = sanitize(&raw_out);
    if test_code.trim().is_empty() {
        return Err("The gateway returned empty test code".into());
    }

    let suggested_test_path = suggest_test_path(&p, &language, &fw);

    Ok(TestGenResult {
        path,
        function_name: resolved_fn,
        language,
        framework: fw,
        test_code,
        suggested_test_path,
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
            "test generator timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

fn build_user_prompt(
    path: &str,
    language: &str,
    framework: &str,
    function_name: Option<&str>,
    body: &str,
) -> String {
    let scope = match function_name {
        Some(name) => format!("Target function: `{name}` (scope your tests to this function).\n"),
        None => String::new(),
    };
    format!(
        "Path: {path}\nLanguage: {language}\nFramework: {framework}\n{scope}\n\
         --- CODE ---\n{body}\n--- END CODE ---\n\n\
         Return ONLY the {framework} test code. No fences, no preamble.",
    )
}

/// Resolve the framework, honouring an explicit user pick, falling back to a
/// language-aware default. For JS/TS we sniff `package.json` for an existing
/// jest setup before defaulting to vitest.
fn resolve_framework(
    explicit: Option<&str>,
    language: &str,
    file_path: &Path,
) -> Result<String, String> {
    let pick = explicit
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "auto");
    if let Some(name) = pick {
        return match name.as_str() {
            "cargo" | "rust" => Ok("cargo".into()),
            "vitest" => Ok("vitest".into()),
            "jest" => Ok("jest".into()),
            "mocha" => Ok("mocha".into()),
            "pytest" | "python" => Ok("pytest".into()),
            other => Err(format!("framework not supported: {other}")),
        };
    }
    match language {
        "Rust" => Ok("cargo".into()),
        "TypeScript" | "JavaScript" => Ok(detect_node_framework(file_path).unwrap_or("vitest".into())),
        "Python" => Ok("pytest".into()),
        _ => Err(format!("framework not supported for {language}")),
    }
}

/// Walk upwards from `file_path` looking for a `package.json`; when found,
/// return "jest" if it's listed (dep map or scripts.test), otherwise fall
/// through. The caller defaults to vitest.
fn detect_node_framework(file_path: &Path) -> Option<String> {
    let mut cur = file_path.parent();
    for _ in 0..8 {
        let dir = cur?;
        let pkg = dir.join("package.json");
        if pkg.is_file() {
            let raw = fs::read_to_string(&pkg).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let mut hay = String::new();
            for key in ["dependencies", "devDependencies", "peerDependencies"] {
                if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
                    for k in obj.keys() {
                        hay.push_str(k);
                        hay.push(' ');
                    }
                }
            }
            if let Some(scripts) = v.get("scripts").and_then(|x| x.as_object()) {
                if let Some(t) = scripts.get("test").and_then(|x| x.as_str()) {
                    hay.push_str(t);
                }
            }
            if hay.contains("jest") {
                return Some("jest".into());
            }
            if hay.contains("vitest") {
                return Some("vitest".into());
            }
            return None;
        }
        cur = dir.parent();
    }
    None
}

/// Build a sensible target file for the generated tests:
/// - rust:   `<crate>/tests/<stem>_gen_test.rs` if path is in a `src/` tree;
///   otherwise leave it adjacent (inline `#[cfg(test)]` style — caller decides).
/// - ts/js:  `__tests__/<stem>.test.ts` when a sibling `__tests__/` already
///   exists or no sibling at all; falls back to a sibling `.test.ts`.
/// - py:     `tests/test_<stem>.py` rooted at the nearest ancestor that
///   already has a `tests/` directory; otherwise a sibling
///   `test_<stem>.py`.
fn suggest_test_path(file_path: &Path, language: &str, framework: &str) -> PathBuf {
    let stem = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string();
    let parent = file_path.parent().unwrap_or_else(|| Path::new("."));

    match language {
        "Rust" => {
            // Walk up looking for a `src/` segment; the crate root is its parent.
            let mut crate_root: Option<&Path> = None;
            let mut cur = Some(parent);
            while let Some(dir) = cur {
                if dir.file_name().and_then(|s| s.to_str()) == Some("src") {
                    crate_root = dir.parent();
                    break;
                }
                cur = dir.parent();
            }
            match crate_root {
                Some(root) => root.join("tests").join(format!("{stem}_gen_test.rs")),
                None => parent.join(format!("{stem}_gen_test.rs")),
            }
        }
        "TypeScript" | "JavaScript" => {
            let ext = if matches!(framework, "jest" | "vitest" | "mocha") {
                if matches!(language, "TypeScript") { "ts" } else { "js" }
            } else {
                "ts"
            };
            let tests_dir = parent.join("__tests__");
            if tests_dir.is_dir() {
                tests_dir.join(format!("{stem}.test.{ext}"))
            } else {
                parent.join(format!("{stem}.test.{ext}"))
            }
        }
        "Python" => {
            // Look up to 4 levels up for an existing `tests/` directory.
            let mut cur = Some(parent);
            for _ in 0..4 {
                if let Some(dir) = cur {
                    let candidate = dir.join("tests");
                    if candidate.is_dir() {
                        return candidate.join(format!("test_{stem}.py"));
                    }
                    cur = dir.parent();
                } else {
                    break;
                }
            }
            parent.join(format!("test_{stem}.py"))
        }
        _ => parent.join(format!("{stem}.test.txt")),
    }
}

/// Best-effort function extraction. Returns the function's signature + body
/// when a match is found; otherwise `None` so the caller can fall back to
/// the full file. Heuristic — handles the common shapes only.
fn extract_function(source: &str, language: &str, name: &str) -> Option<String> {
    match language {
        "Rust" => extract_braced(source, &format!("fn {name}"), '{', '}'),
        "TypeScript" | "JavaScript" => extract_braced(source, &format!("function {name}"), '{', '}')
            .or_else(|| extract_braced(source, &format!("const {name}"), '{', '}'))
            .or_else(|| extract_braced(source, &format!("export function {name}"), '{', '}'))
            .or_else(|| extract_braced(source, &format!("{name} ="), '{', '}')),
        "Python" => extract_python_def(source, name),
        _ => None,
    }
}

/// Find `needle` in `source` such that the character immediately following the
/// match is not an identifier-continuation character (alphanumeric or `_`).
/// This prevents matching a name that is merely a prefix of a longer
/// identifier (e.g. searching for `fn foo` should not match `fn foobar`).
/// Returns the byte offset of the match start.
fn find_at_word_boundary(source: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = source[from..].find(needle) {
        let start = from + rel;
        let next = source[start + needle.len()..]
            .chars()
            .next();
        match next {
            Some(c) if c.is_alphanumeric() || c == '_' => {
                // False match inside a longer identifier; keep looking.
                from = start + needle.len();
            }
            _ => return Some(start),
        }
    }
    None
}

/// Find `needle`, then balance `open`/`close` braces from the first `open`
/// after the needle to capture the function body. Returns the slice from
/// needle start through the matching close brace.
///
/// The match must end on a word boundary so a name that is a substring of a
/// longer identifier (e.g. `foo` inside `foobar`) does not extract the wrong
/// function.
fn extract_braced(source: &str, needle: &str, open: char, close: char) -> Option<String> {
    let start = find_at_word_boundary(source, needle)?;
    let after = &source[start..];
    let open_idx = after.find(open)?;
    let mut depth = 0i32;
    let mut end_offset = None;
    for (i, ch) in after[open_idx..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                end_offset = Some(open_idx + i + ch.len_utf8());
                break;
            }
        }
    }
    let end = end_offset?;
    Some(after[..end].to_string())
}

/// Python's `def name(` followed by an indented body. We grab from the def
/// line through the first dedent (or EOF).
fn extract_python_def(source: &str, name: &str) -> Option<String> {
    let needle = format!("def {name}(");
    let start = source.find(&needle)?;
    let rest = &source[start..];
    let mut lines = rest.lines();
    let header = lines.next()?;
    let mut out = String::new();
    out.push_str(header);
    out.push('\n');
    // The body indent is "any whitespace deeper than the def line's indent".
    let def_indent = header.len() - header.trim_start().len();
    for line in lines {
        if line.trim().is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= def_indent {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

/// Best-effort language label from the file extension. Mirrors the small
/// language table in [`super::doc_gen`].
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
        "rb" => "Ruby",
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

/// Strip a single outer fenced block + a "Here is …" lead-in. Same shape as
/// [`super::doc_gen::sanitize`].
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
        assert_eq!(detect_language(Path::new("foo.ts")), "TypeScript");
        assert_eq!(detect_language(Path::new("foo.py")), "Python");
        assert_eq!(detect_language(Path::new("foo.go")), "Go");
    }

    #[test]
    fn resolve_framework_defaults() {
        assert_eq!(
            resolve_framework(None, "Rust", Path::new("foo.rs")).unwrap(),
            "cargo"
        );
        assert_eq!(
            resolve_framework(None, "Python", Path::new("foo.py")).unwrap(),
            "pytest"
        );
    }

    #[test]
    fn resolve_framework_honours_user_override() {
        assert_eq!(
            resolve_framework(Some("jest"), "Rust", Path::new("foo.rs")).unwrap(),
            "jest"
        );
        assert_eq!(
            resolve_framework(Some("AUTO"), "Rust", Path::new("foo.rs")).unwrap(),
            "cargo"
        );
        assert!(resolve_framework(Some("klingon"), "Rust", Path::new("foo.rs")).is_err());
    }

    #[test]
    fn resolve_framework_rejects_unsupported_language() {
        assert!(resolve_framework(None, "Go", Path::new("foo.go")).is_err());
    }

    #[test]
    fn extract_rust_function_returns_signature_and_body() {
        let src = "fn other() {}\n\nfn target(x: u32) -> u32 {\n    x + 1\n}\n\nfn another() {}\n";
        let out = extract_function(src, "Rust", "target").unwrap();
        assert!(out.contains("fn target"));
        assert!(out.contains("x + 1"));
        assert!(!out.contains("fn another"));
    }

    #[test]
    fn extract_python_function_uses_indent() {
        let src = "def a():\n    return 1\n\ndef target(x):\n    if x > 0:\n        return x\n    return 0\n\ndef b():\n    pass\n";
        let out = extract_function(src, "Python", "target").unwrap();
        assert!(out.starts_with("def target"));
        assert!(out.contains("return x"));
        assert!(!out.contains("def b("));
    }

    #[test]
    fn suggest_path_rust_with_src() {
        let p = Path::new("/repo/mycrate/src/lib.rs");
        let out = suggest_test_path(p, "Rust", "cargo");
        assert_eq!(
            out,
            PathBuf::from("/repo/mycrate/tests/lib_gen_test.rs")
        );
    }

    #[test]
    fn suggest_path_rust_without_src() {
        let p = Path::new("/tmp/foo.rs");
        let out = suggest_test_path(p, "Rust", "cargo");
        assert_eq!(out, PathBuf::from("/tmp/foo_gen_test.rs"));
    }

    #[test]
    fn suggest_path_typescript_sibling() {
        let p = Path::new("/repo/src/utils.ts");
        let out = suggest_test_path(p, "TypeScript", "vitest");
        // No __tests__/ dir exists on the filesystem here.
        assert_eq!(out, PathBuf::from("/repo/src/utils.test.ts"));
    }

    #[test]
    fn suggest_path_python_sibling_when_no_tests_dir() {
        let p = Path::new("/tmp/myproj/util.py");
        let out = suggest_test_path(p, "Python", "pytest");
        assert_eq!(out, PathBuf::from("/tmp/myproj/test_util.py"));
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
        let raw = "```rust\n#[test]\nfn it_works() {}\n```";
        assert_eq!(sanitize(raw), "#[test]\nfn it_works() {}");
    }

    #[test]
    fn sanitize_strips_here_is_preamble() {
        let raw = "Here is the test:\n#[test]\nfn it_works() {}";
        assert_eq!(sanitize(raw), "#[test]\nfn it_works() {}");
    }

    #[test]
    fn build_user_prompt_includes_metadata() {
        let p = build_user_prompt("a.rs", "Rust", "cargo", Some("foo"), "fn foo() {}");
        assert!(p.contains("Path: a.rs"));
        assert!(p.contains("Language: Rust"));
        assert!(p.contains("Framework: cargo"));
        assert!(p.contains("Target function: `foo`"));
        assert!(p.contains("fn foo() {}"));
    }

    #[test]
    fn build_user_prompt_omits_function_scope_when_none() {
        let p = build_user_prompt("a.py", "Python", "pytest", None, "def foo(): pass");
        assert!(!p.contains("Target function"));
    }
}
