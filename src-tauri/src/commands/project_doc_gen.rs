//! AI project-doc generator (README.md / CLAUDE.md / CONTRIBUTING.md).
//!
//! Reads a *project-level* context bundle from `project_root` — manifest
//! (package.json / Cargo.toml / pyproject.toml), recent git activity, top-
//! level file tree, and any existing README/CLAUDE.md — caps the whole bundle
//! at 16 KiB, and asks the gateway for a polished markdown document. Mirrors the
//! streaming-collect + timeout pattern from [`super::changelog`] /
//! [`super::doc_gen`].
//!
//! The user-facing flow is `/readme` → modal with doc_type=readme,
//! `/claude-md` → modal with doc_type=claude-md. The modal also exposes a
//! third "contributing" radio. Saving the result back to disk is the
//! caller's responsibility (`save_file_text`), gated by an explicit user
//! action in the modal.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Total context budget shipped to the model. 16 KiB keeps the prompt well
/// under gateway latency cliffs while still fitting a typical manifest +
/// 20-commit log + tree + existing doc.
const CONTEXT_LIMIT_BYTES: usize = 16 * 1024;

/// Per-file cap for big inputs (existing README/CLAUDE.md or large manifests).
/// 6 KiB each keeps any single source from monopolising the budget.
const PER_FILE_LIMIT_BYTES: usize = 6 * 1024;

/// 45s wall clock — README generation tends to be heavier than doc-gen because
/// the model has to author whole sections from scratch.
const TIMEOUT: Duration = Duration::from_secs(45);

/// How many recent commits to surface to the model. 20 is enough to convey
/// "what's been happening lately" without flooding the context.
const MAX_COMMITS: usize = 20;

#[derive(Debug, Serialize, Clone)]
pub struct ProjectDocResult {
    pub doc_type: String,
    pub markdown: String,
    pub suggested_path: String,
    pub generated_unix_ms: i64,
}

#[tauri::command]
pub async fn generate_project_doc(
    project_root: String,
    doc_type: String,
    state: State<'_, AppState>,
) -> Result<ProjectDocResult, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let canonical_doc_type = canonicalize_doc_type(&doc_type)?;
    let project_name = project_name_from_root(&root);
    let manifest = read_manifest(&root);
    let git_log = read_git_log(&root);
    let tree = read_top_level_tree(&root);
    let existing = read_existing_doc(&root, &canonical_doc_type);

    let context = build_context(
        &project_name,
        manifest.as_deref(),
        git_log.as_deref(),
        &tree,
        existing.as_deref(),
    );
    let context = truncate(context, CONTEXT_LIMIT_BYTES);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let system_prompt = system_prompt_for(&canonical_doc_type);
    let user_prompt = build_user_prompt(&canonical_doc_type, &project_name, &context);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: system_prompt.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.2),
    };

    let raw_out = run_with_timeout(client, req).await?;
    let markdown = sanitize(&raw_out);
    if markdown.trim().is_empty() {
        return Err("The gateway returned an empty document".into());
    }

    let suggested_path = suggested_path_for(&root, &canonical_doc_type);
    Ok(ProjectDocResult {
        doc_type: canonical_doc_type,
        markdown,
        suggested_path,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
    })
}

/// Normalise the user-supplied `doc_type` into one of the three supported
/// canonical keys. Unknown values are rejected rather than silently mapped so
/// the frontend never accidentally hands us something the prompt can't honor.
fn canonicalize_doc_type(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    match trimmed.as_str() {
        "readme" | "readme.md" => Ok("readme".to_string()),
        "claude-md" | "claude" | "claude.md" => Ok("claude-md".to_string()),
        "contributing" | "contributing.md" => Ok("contributing".to_string()),
        _ => Err(format!(
            "unsupported doc_type: '{raw}' (expected readme | claude-md | contributing)"
        )),
    }
}

fn project_name_from_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project")
        .to_string()
}

/// Probe the three manifests we know how to surface in priority order.
/// Returns a labelled blob (capped) or `None` when none exist.
fn read_manifest(root: &Path) -> Option<String> {
    for (name, label) in [
        ("package.json", "package.json"),
        ("Cargo.toml", "Cargo.toml"),
        ("pyproject.toml", "pyproject.toml"),
    ] {
        let p = root.join(name);
        if let Ok(body) = std::fs::read_to_string(&p) {
            let capped = cap_chars(&body, PER_FILE_LIMIT_BYTES);
            return Some(format!("[{label}]\n{capped}"));
        }
    }
    None
}

fn read_git_log(root: &Path) -> Option<String> {
    if !root.join(".git").exists() {
        return None;
    }
    let max_arg = format!("-{}", MAX_COMMITS);
    let out = crate::sys::no_window("git")
        .args(["log", "--oneline", "--no-color", &max_arg])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

/// Top-level (depth 1) listing of the project. Skips noisy dirs we'd never
/// want the model to mention in a README (`.git`, `node_modules`, `target`,
/// `dist`, `.cortex`).
fn read_top_level_tree(root: &Path) -> Vec<String> {
    let skip: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".next",
        ".cortex",
        ".venv",
        "venv",
        "__pycache__",
    ];
    let mut entries: Vec<String> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if skip.contains(&name.as_str()) {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(if is_dir { format!("{name}/") } else { name })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries
}

/// For a `claude-md` request we want the existing `CLAUDE.md` (so the model
/// can produce a delta-aware update); for a `readme` we want the existing
/// `README.md`; for `contributing` we want any existing `CONTRIBUTING.md`.
fn read_existing_doc(root: &Path, doc_type: &str) -> Option<String> {
    let candidates: &[&str] = match doc_type {
        "readme" => &["README.md", "README.MD", "readme.md"],
        "claude-md" => &["CLAUDE.md", "claude.md"],
        "contributing" => &["CONTRIBUTING.md", "contributing.md"],
        _ => &[],
    };
    for name in candidates {
        if let Ok(body) = std::fs::read_to_string(root.join(name)) {
            let capped = cap_chars(&body, PER_FILE_LIMIT_BYTES);
            return Some(format!("[existing {name}]\n{capped}"));
        }
    }
    None
}

fn build_context(
    project_name: &str,
    manifest: Option<&str>,
    git_log: Option<&str>,
    tree: &[String],
    existing: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(5);
    parts.push(format!("Project name: {project_name}"));
    if let Some(m) = manifest {
        parts.push(format!("--- MANIFEST ---\n{m}"));
    }
    if !tree.is_empty() {
        let listed = tree.join("\n");
        parts.push(format!("--- TOP-LEVEL TREE ---\n{listed}"));
    }
    if let Some(log) = git_log {
        parts.push(format!("--- RECENT COMMITS (git log --oneline -20) ---\n{log}"));
    }
    if let Some(e) = existing {
        parts.push(format!("--- EXISTING DOC ---\n{e}"));
    }
    parts.join("\n\n")
}

fn system_prompt_for(doc_type: &str) -> &'static str {
    match doc_type {
        "readme" => {
            "You generate polished README.md files. Sections (in order): \
             Overview, Features, Quick Start, Architecture, Development, License (MIT default). \
             Use the project's actual stack and conventions detected from the context. \
             Return ONLY markdown — no fences, no preamble."
        }
        "claude-md" => {
            "You generate CLAUDE.md files — instructions for an AI coding agent working in the project. \
             Sections (in order): Project context, Architecture, Conventions, Don'ts, Build commands, Test commands. \
             Be specific to the detected stack — cite real script names, real dependencies, real folders. \
             Return ONLY markdown — no fences, no preamble."
        }
        "contributing" => {
            "You generate CONTRIBUTING.md files. Sections (in order): How to contribute, \
             Development setup, Branching model, Pull request process, Coding standards, Reporting bugs. \
             Use the project's detected stack to make setup and standards concrete. \
             Return ONLY markdown — no fences, no preamble."
        }
        _ => "Return ONLY markdown — no fences, no preamble.",
    }
}

fn build_user_prompt(doc_type: &str, project_name: &str, context: &str) -> String {
    format!(
        "Doc type: {doc_type}\nProject: {project_name}\n\n\
         --- PROJECT CONTEXT ---\n{context}\n--- END CONTEXT ---\n\n\
         Return ONLY the full markdown body for the {doc_type} document. \
         No fences, no preamble, no trailing commentary.",
    )
}

fn suggested_path_for(root: &Path, doc_type: &str) -> String {
    let filename = match doc_type {
        "readme" => "README.md",
        "claude-md" => "CLAUDE.md",
        "contributing" => "CONTRIBUTING.md",
        _ => "README.md",
    };
    let root_str = root.to_string_lossy().replace('\\', "/");
    let root_str = root_str.trim_end_matches('/');
    format!("{root_str}/{filename}")
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
            "project doc generator timed out after {}s",
            started.elapsed().as_secs()
        )),
    }
}

/// UTF-8-safe char-wise truncation to a byte budget. Used for both the per-
/// file cap and the overall context budget.
fn cap_chars(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut out = String::with_capacity(limit);
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > limit {
            break;
        }
        out.push(ch);
    }
    out.push_str("\n[truncated]");
    out
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
    s.push_str("\n[truncated — context exceeded 16 KiB]");
    s
}

/// Strip outer fenced ``` blocks and "Here is …" preamble the model may emit
/// despite the prompt. Conservative — only peels one wrapper.
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
    use std::fs;

    #[test]
    fn canonicalize_accepts_known_variants() {
        assert_eq!(canonicalize_doc_type("readme").unwrap(), "readme");
        assert_eq!(canonicalize_doc_type("README.md").unwrap(), "readme");
        assert_eq!(canonicalize_doc_type("claude-md").unwrap(), "claude-md");
        assert_eq!(canonicalize_doc_type("Claude").unwrap(), "claude-md");
        assert_eq!(canonicalize_doc_type("contributing").unwrap(), "contributing");
    }

    #[test]
    fn canonicalize_rejects_unknown() {
        assert!(canonicalize_doc_type("license").is_err());
        assert!(canonicalize_doc_type("").is_err());
    }

    #[test]
    fn suggested_path_uses_forward_slashes() {
        let root = PathBuf::from("/tmp/proj");
        assert_eq!(suggested_path_for(&root, "readme"), "/tmp/proj/README.md");
        assert_eq!(suggested_path_for(&root, "claude-md"), "/tmp/proj/CLAUDE.md");
        assert_eq!(
            suggested_path_for(&root, "contributing"),
            "/tmp/proj/CONTRIBUTING.md"
        );
    }

    #[test]
    fn project_name_falls_back_when_unparseable() {
        // root paths always have a file_name in practice; just sanity-check
        // the common case here.
        let root = PathBuf::from("/tmp/my-cool-app");
        assert_eq!(project_name_from_root(&root), "my-cool-app");
    }

    #[test]
    fn read_top_level_tree_skips_noise() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("README.md"), "x").unwrap();
        let entries = read_top_level_tree(tmp.path());
        assert!(entries.iter().any(|e| e == "src/"));
        assert!(entries.iter().any(|e| e == "README.md"));
        assert!(!entries.iter().any(|e| e.starts_with("node_modules")));
    }

    #[test]
    fn read_manifest_prefers_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();
        let m = read_manifest(tmp.path()).unwrap();
        assert!(m.starts_with("[package.json]"));
    }

    #[test]
    fn read_existing_doc_picks_by_doc_type() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("README.md"), "old readme").unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "old claude").unwrap();
        let r = read_existing_doc(tmp.path(), "readme").unwrap();
        assert!(r.contains("old readme"));
        let c = read_existing_doc(tmp.path(), "claude-md").unwrap();
        assert!(c.contains("old claude"));
        // Missing CONTRIBUTING.md should give None.
        assert!(read_existing_doc(tmp.path(), "contributing").is_none());
    }

    #[test]
    fn truncate_caps_long_contexts() {
        let blob = "x".repeat(CONTEXT_LIMIT_BYTES + 200);
        let out = truncate(blob, CONTEXT_LIMIT_BYTES);
        assert!(out.contains("[truncated"));
        assert!(out.len() < CONTEXT_LIMIT_BYTES + 100);
    }

    #[test]
    fn cap_chars_handles_utf8_boundaries() {
        let s = "ééééé".repeat(2000); // 2 bytes per char
        let out = cap_chars(&s, 50);
        // Must not panic and must end on a char boundary.
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn sanitize_strips_fenced_block() {
        let raw = "```markdown\n# Title\n\nbody\n```";
        assert_eq!(sanitize(raw), "# Title\n\nbody");
    }

    #[test]
    fn sanitize_strips_here_is_preamble() {
        let raw = "Here is the README:\n# Title";
        assert_eq!(sanitize(raw), "# Title");
    }

    #[test]
    fn build_user_prompt_includes_metadata() {
        let p = build_user_prompt("readme", "my-app", "ctx");
        assert!(p.contains("Doc type: readme"));
        assert!(p.contains("Project: my-app"));
        assert!(p.contains("ctx"));
    }

    #[test]
    fn system_prompt_varies_per_doc_type() {
        assert!(system_prompt_for("readme").contains("README"));
        assert!(system_prompt_for("claude-md").contains("CLAUDE.md"));
        assert!(system_prompt_for("contributing").contains("CONTRIBUTING"));
    }

    #[test]
    fn build_context_omits_missing_sections() {
        let ctx = build_context("p", None, None, &[], None);
        assert!(ctx.contains("Project name: p"));
        assert!(!ctx.contains("MANIFEST"));
        assert!(!ctx.contains("TOP-LEVEL TREE"));
        assert!(!ctx.contains("RECENT COMMITS"));
        assert!(!ctx.contains("EXISTING DOC"));
    }
}
