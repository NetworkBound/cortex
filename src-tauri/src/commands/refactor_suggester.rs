//! AI refactor suggester.
//!
//! Reads a single source file from disk (capped at 64 KiB), pipes it through
//! the gateway with a structured prompt asking for 3-5 specific refactors, and
//! returns a parsed array of [`Refactor`] entries the UI can render
//! side-by-side. Mirrors the streaming-collect + timeout pattern from
//! [`super::inline_completion`] and the parse-or-fallback resilience of
//! [`super::session_summary`].
//!
//! The user-facing flow is `/refactor [path]` — the frontend modal calls
//! `suggest_refactors` with the path and an optional intent string ("focus on
//! testability", "minimise allocations", …). Output is intentionally
//! advisory: the modal renders "Apply"/"Copy" buttons but never writes to
//! disk; that hand-off is owned by the editor.

use serde::{Deserialize, Serialize};
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

/// 30s wall clock on the gateway call. Refactor suggestions are a one-shot
/// ask, not interactive, so we give it more headroom than the 5s inline-
/// completion cap but less than a full session summary.
const TIMEOUT: Duration = Duration::from_secs(30);

const SYSTEM_PROMPT: &str = "You are a refactor suggester. Given a source file, \
propose 3-5 specific, high-leverage refactors. Each refactor MUST be returned as \
a JSON object with exactly these keys: \
`name` (short title), `rationale` (1-2 sentence justification), \
`before_snippet` (exact code excerpt to change), `after_snippet` (the proposed \
replacement), `confidence` (a number 0.0-1.0). \
Return ONLY a JSON array of these objects — no preamble, no fences, no trailing prose.";

#[derive(Debug, Deserialize)]
pub struct SuggestRefactorsArgs {
    pub path: String,
    #[serde(default)]
    pub intent: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Refactor {
    pub name: String,
    pub rationale: String,
    pub before_snippet: String,
    pub after_snippet: String,
    pub confidence: f64,
}

#[derive(Debug, Serialize, Clone)]
pub struct RefactorReport {
    pub path: String,
    pub refactors: Vec<Refactor>,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn suggest_refactors(
    args: SuggestRefactorsArgs,
    state: State<'_, AppState>,
) -> Result<RefactorReport, String> {
    let cfg = state.config.read().clone();

    // Confine the requested path to the active project root. Without this, the
    // command will read ANY file the desktop process can access (e.g.
    // ~/.ssh/id_ed25519, ~/.gitea-token) and ship its contents to remote
    // the gateway — an arbitrary-file-read → exfiltration primitive.
    let project_root = cfg
        .default_project_root
        .as_deref()
        .ok_or("no active project root configured")?;
    let path = confine_to_project(&args.path, project_root)?;

    if !path.is_file() {
        return Err(format!("not a file: {}", args.path));
    }

    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("read {} failed: {e}", path.display()))?;
    let body = truncate(raw, FILE_LIMIT_BYTES);
    if body.trim().is_empty() {
        return Err("file is empty — nothing to refactor".into());
    }

    let language = detect_language(&path);
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = build_user_prompt(&args.path, &language, &body, args.intent.as_deref());

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
    let refactors = parse_refactors(&raw);
    Ok(RefactorReport {
        path: args.path,
        refactors,
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
            "refactor suggester timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

fn build_user_prompt(path: &str, language: &str, body: &str, intent: Option<&str>) -> String {
    let mut prompt = format!(
        "Path: {path}\nLanguage: {language}\n\n--- FILE ---\n{body}\n--- END FILE ---\n",
    );
    if let Some(focus) = intent.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        prompt.push_str(&format!("\nUser focus: {focus}\n"));
    }
    prompt.push_str(
        "\nReturn ONLY a JSON array of refactor objects as described in the system prompt.",
    );
    prompt
}

/// Best-effort language label from the file extension. Just steers the model
/// — The gateway will infer if we hand it `plain text`.
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

/// Resolve `requested` and guarantee it lives inside `project_root`,
/// rejecting traversal (`..`), symlink escapes, and sensitive dotfiles.
///
/// We canonicalize both sides so `..` segments and symlinks are collapsed to
/// a real path before the containment check — a plain string `starts_with`
/// on the raw input is trivially bypassed via `../` or a symlink that points
/// outside the tree.
fn confine_to_project(requested: &str, project_root: &Path) -> Result<PathBuf, String> {
    // Reject obviously sensitive names anywhere in the requested path, even
    // before resolution, so a symlink *inside* the project that is named
    // innocuously can't smuggle these through component matching either.
    const DENY_COMPONENTS: &[&str] = &[
        ".ssh",
        ".gitea-token",
        ".git-credentials",
        ".netrc",
        ".aws",
        ".gnupg",
        ".pgpass",
        "id_rsa",
        "id_ed25519",
    ];

    let root = project_root
        .canonicalize()
        .map_err(|e| format!("project root {} unavailable: {e}", project_root.display()))?;

    // Resolve `requested` relative to the project root when it is not
    // absolute, so a bare filename is interpreted within the project.
    let candidate = {
        let p = Path::new(requested);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };

    let resolved = candidate
        .canonicalize()
        .map_err(|e| format!("path {requested} unavailable: {e}"))?;

    if !resolved.starts_with(&root) {
        return Err(format!(
            "path {requested} is outside the active project root"
        ));
    }

    for comp in resolved.components() {
        if let std::path::Component::Normal(os) = comp {
            if let Some(name) = os.to_str() {
                let lower = name.to_ascii_lowercase();
                if DENY_COMPONENTS.iter().any(|d| *d == lower) {
                    return Err(format!("path {requested} references a protected file"));
                }
            }
        }
    }

    Ok(resolved)
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

/// Tolerant parser. Falls back to `[]` rather than failing the whole command
/// when the model returns prose, partial JSON, or a fenced block.
fn parse_refactors(raw: &str) -> Vec<Refactor> {
    let cleaned = strip_fences(raw);
    // Direct parse first — cheapest path when the model behaves.
    if let Ok(out) = serde_json::from_str::<Vec<Refactor>>(&cleaned) {
        return clamp_confidences(out);
    }
    // Salvage: find the outermost `[ ... ]` and try again. Catches the case
    // where the model prepended "Here are the refactors:" despite the
    // prompt forbidding it.
    if let (Some(start), Some(end)) = (cleaned.find('['), cleaned.rfind(']')) {
        if end > start {
            if let Ok(out) = serde_json::from_str::<Vec<Refactor>>(&cleaned[start..=end]) {
                return clamp_confidences(out);
            }
        }
    }
    Vec::new()
}

fn clamp_confidences(mut out: Vec<Refactor>) -> Vec<Refactor> {
    for r in out.iter_mut() {
        if !r.confidence.is_finite() {
            r.confidence = 0.0;
        } else if r.confidence < 0.0 {
            r.confidence = 0.0;
        } else if r.confidence > 1.0 {
            r.confidence = 1.0;
        }
    }
    out
}

fn strip_fences(raw: &str) -> String {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            return after_lang[..end].trim().to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refactors_accepts_clean_json_array() {
        let raw = r#"[{"name":"Extract function","rationale":"Reduces nesting.","before_snippet":"a","after_snippet":"b","confidence":0.9}]"#;
        let out = parse_refactors(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Extract function");
        assert!((out[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn parse_refactors_salvages_with_preamble() {
        let raw = r#"Here are the refactors:
[{"name":"X","rationale":"r","before_snippet":"a","after_snippet":"b","confidence":1.0}]
Hope this helps."#;
        let out = parse_refactors(raw);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn parse_refactors_strips_fenced_block() {
        let raw = "```json\n[{\"name\":\"X\",\"rationale\":\"r\",\"before_snippet\":\"a\",\"after_snippet\":\"b\",\"confidence\":0.5}]\n```";
        let out = parse_refactors(raw);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn parse_refactors_falls_back_to_empty_on_garbage() {
        assert!(parse_refactors("totally not json").is_empty());
        assert!(parse_refactors("").is_empty());
    }

    #[test]
    fn parse_refactors_clamps_out_of_range_confidence() {
        let raw = r#"[{"name":"X","rationale":"r","before_snippet":"a","after_snippet":"b","confidence":2.5},
                      {"name":"Y","rationale":"r","before_snippet":"a","after_snippet":"b","confidence":-0.3}]"#;
        let out = parse_refactors(raw);
        assert_eq!(out.len(), 2);
        assert!((out[0].confidence - 1.0).abs() < 1e-9);
        assert!((out[1].confidence - 0.0).abs() < 1e-9);
    }

    #[test]
    fn detect_language_known_extensions() {
        assert_eq!(detect_language(Path::new("foo.rs")), "Rust");
        assert_eq!(detect_language(Path::new("foo.tsx")), "TypeScript");
        assert_eq!(detect_language(Path::new("foo.py")), "Python");
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
    fn build_user_prompt_includes_focus_when_supplied() {
        let p = build_user_prompt("a.rs", "Rust", "fn x() {}", Some("testability"));
        assert!(p.contains("User focus: testability"));
        assert!(p.contains("Path: a.rs"));
        assert!(p.contains("Language: Rust"));
    }

    #[test]
    fn build_user_prompt_omits_focus_when_blank() {
        let p = build_user_prompt("a.rs", "Rust", "fn x() {}", Some("   "));
        assert!(!p.contains("User focus"));
    }

    #[test]
    fn confine_accepts_file_inside_project() {
        let dir = std::env::temp_dir().join(format!("cortex_confine_ok_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let f = dir.join("main.rs");
        fs::write(&f, "fn main() {}").unwrap();

        let got = confine_to_project("main.rs", &dir).unwrap();
        assert_eq!(got, f.canonicalize().unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn confine_rejects_traversal_outside_project() {
        let base = std::env::temp_dir().join(format!("cortex_confine_esc_{}", std::process::id()));
        let proj = base.join("proj");
        let _ = fs::create_dir_all(&proj);
        // A real file just outside the project root.
        let outside = base.join("secret.txt");
        fs::write(&outside, "top secret").unwrap();

        let err = confine_to_project("../secret.txt", &proj).unwrap_err();
        assert!(err.contains("outside"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn confine_rejects_protected_dotfiles() {
        let proj = std::env::temp_dir().join(format!("cortex_confine_dot_{}", std::process::id()));
        let ssh = proj.join(".ssh");
        let _ = fs::create_dir_all(&ssh);
        let key = ssh.join("id_ed25519");
        fs::write(&key, "PRIVATE KEY").unwrap();

        // Inside the project, but a protected name — must still be rejected.
        let err = confine_to_project(".ssh/id_ed25519", &proj).unwrap_err();
        assert!(err.contains("protected"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(&proj);
    }
}
