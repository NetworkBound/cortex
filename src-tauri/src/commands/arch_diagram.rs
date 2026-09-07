//! Repo-architecture diagram generator (gitdiagram-style two-pass).
//!
//! `generate_arch_diagram` walks the active project's file tree (reusing the
//! established [`crate::repo_map`] walker + ignore set), then makes two gateway
//! gateway calls:
//!   - Pass 1: summarise the structure into a short English system description.
//!   - Pass 2: turn that description into a VALID Mermaid `flowchart`/`graph`
//!     definition.
//! The result is cached keyed by the current git SHA so re-opening the panel is
//! instant; `force=true` bypasses the cache and regenerates.
//!
//! Mirrors the streaming-collect + timeout pattern from [`super::explain`] for
//! the gateway calls, and the process-wide TTL/LRU cache shape from
//! [`crate::repo_map`] for the SHA-keyed result cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Serialize;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};
use crate::repo_map::{compute_repo_map, format_as_text};

/// Cap on files included in the map we hand to the model. Keeps the structural
/// summary under the gateway context window for large monorepos.
const DEFAULT_MAX_FILES: usize = 200;

/// Hard cap on the file-tree text we feed pass 1. The repo-map formatter
/// already caps at ~50 KiB; this is a defensive trim for very dense trees.
const MAX_TREE_BYTES: usize = 32 * 1024;

/// Wall-clock per gateway call. Pass 1 (summary) and pass 2 (mermaid) each get
/// their own budget — diagram synthesis is heavier than commit messages but
/// lighter than full doc rewriting.
const PASS_TIMEOUT: Duration = Duration::from_secs(60);

/// Result cache TTL. The cache is keyed by `(root, sha)` so a stable repo stays
/// cached indefinitely within the TTL; a new commit changes the key. The TTL
/// is a backstop against unbounded growth across many projects in one session.
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// LRU cap on the result cache (one entry per project+SHA).
const CACHE_MAX_ENTRIES: usize = 16;

const PASS1_SYSTEM: &str = "You are a software architect. Given a project's file/symbol \
map, describe the system's high-level architecture in 6-12 short sentences: the major \
modules/layers, how they depend on each other, the entry points, and the external \
boundaries (UI, gateway/API, storage). Be concrete and use the real module names. Return \
plain prose only — no markdown headings, no bullet lists, no code fences.";

const PASS2_SYSTEM: &str = "You convert an architecture description into a single valid \
Mermaid diagram. Output ONLY the Mermaid definition — no prose, no markdown code fences. \
Start with `flowchart TD`. Use short alphanumeric node ids (A, B, C…) with bracketed \
labels, e.g. `A[UI Layer]`. Connect nodes with `-->`. Group related nodes with \
`subgraph name ... end` where it clarifies layers. Keep it under 30 nodes. Do not invent \
technologies not present in the description.";

/// The shape returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct ArchDiagram {
    /// A valid Mermaid `flowchart`/`graph` definition.
    pub mermaid: String,
    /// The pass-1 English system description.
    pub description: String,
    /// Git SHA the diagram was generated against (or "uncommitted"/"nogit").
    pub sha: String,
    /// True when served from the SHA-keyed cache (i.e. not regenerated).
    pub cached: bool,
}

/// Process-wide SHA-keyed cache. Key: `"{root}|{sha}"`.
static ARCH_CACHE: Lazy<Mutex<HashMap<String, (Instant, ArchDiagram)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn cache_key(root: &Path, sha: &str) -> String {
    format!("{}|{}", root.display(), sha)
}

fn validate_root(project_root: &str) -> Result<PathBuf, String> {
    if project_root.trim().is_empty() {
        return Err("project_root is empty".into());
    }
    let p = PathBuf::from(project_root);
    if !p.is_dir() {
        return Err(format!("project_root is not a directory: {project_root}"));
    }
    Ok(p)
}

/// Resolve the current git HEAD SHA for `root`. Falls back to a stable sentinel
/// when the project isn't a git repo so the cache still keys deterministically
/// (a `force=true` regenerate is the escape hatch there).
fn current_sha(root: &Path) -> String {
    match crate::sys::no_window("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
    {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                "nogit".into()
            } else {
                s
            }
        }
        _ => "nogit".into(),
    }
}

/// Generate (or fetch from cache) the architecture diagram for `project_root`.
///
/// Two-pass: structure → English description → validated Mermaid. Cached by the
/// current git SHA; `force=true` bypasses and refreshes the cache entry.
#[tauri::command]
pub async fn generate_arch_diagram(
    project_root: String,
    force: Option<bool>,
    state: State<'_, AppState>,
) -> Result<ArchDiagram, String> {
    let root = validate_root(&project_root)?;
    let force = force.unwrap_or(false);

    let sha = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || current_sha(&root))
            .await
            .map_err(|e| format!("sha task failed: {e}"))?
    };
    let key = cache_key(&root, &sha);

    if !force {
        if let Ok(cache) = ARCH_CACHE.lock() {
            if let Some((stored_at, diag)) = cache.get(&key) {
                if stored_at.elapsed() < CACHE_TTL {
                    let mut hit = diag.clone();
                    hit.cached = true;
                    return Ok(hit);
                }
            }
        }
    }

    // Pass 0 — walk the tree (blocking) and render the Aider-style map text.
    let tree_text = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || {
            let map = compute_repo_map(&root, DEFAULT_MAX_FILES);
            format_as_text(&map)
        })
        .await
        .map_err(|e| format!("repo walk task failed: {e}"))?
    };
    let tree_text = truncate(tree_text, MAX_TREE_BYTES);
    if tree_text.trim().is_empty() {
        return Err("no scannable source files found in project".into());
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();

    // Pass 1 — structure → English description.
    let project_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let pass1_user = format!(
        "Project: {project_name}\n\nFile/symbol map (Aider-style, ★ = PageRank centrality):\n\n{tree_text}\n\nDescribe the architecture."
    );
    let description = run_pass(
        GatewayClient::new(cfg.gateway_base_url.clone(), api_key.clone()),
        ChatCompletionRequest {
            model: cfg.gateway_model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: PASS1_SYSTEM.into() },
                ChatMessage { role: "user".into(), content: pass1_user },
            ],
            stream: true,
            temperature: Some(0.3),
        },
    )
    .await?;
    let description = strip_prose_fence(&description);
    if description.trim().is_empty() {
        return Err("The gateway returned an empty architecture description".into());
    }

    // Pass 2 — description → Mermaid.
    let pass2_user = format!(
        "Architecture description for `{project_name}`:\n\n{description}\n\nReturn ONLY the Mermaid definition."
    );
    let raw_mermaid = run_pass(
        GatewayClient::new(cfg.gateway_base_url.clone(), api_key.clone()),
        ChatCompletionRequest {
            model: cfg.gateway_model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: PASS2_SYSTEM.into() },
                ChatMessage { role: "user".into(), content: pass2_user },
            ],
            stream: true,
            temperature: Some(0.2),
        },
    )
    .await?;
    let mermaid = sanitize_mermaid(&raw_mermaid);
    if !is_valid_mermaid_header(&mermaid) {
        return Err(format!(
            "The gateway did not return a valid Mermaid flowchart (got: {:?})",
            mermaid.lines().next().unwrap_or("")
        ));
    }

    let diagram = ArchDiagram {
        mermaid,
        description,
        sha: sha.clone(),
        cached: false,
    };

    if let Ok(mut cache) = ARCH_CACHE.lock() {
        if cache.len() >= CACHE_MAX_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), diagram.clone()));
    }

    Ok(diagram)
}

/// Run a single streaming gateway pass and collect the full text body.
async fn run_pass(client: GatewayClient, req: ChatCompletionRequest) -> Result<String, String> {
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

    match tokio::time::timeout(PASS_TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err(format!(
            "architecture pass timed out after {}s",
            PASS_TIMEOUT.as_secs()
        )),
    }
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
    s.push_str("\n… (tree truncated)");
    s
}

/// Strip a stray outer code fence or "Here is …" preamble from pass-1 prose.
fn strip_prose_fence(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim().to_string();
        }
    }
    if let Some(rest) = s.strip_prefix("Here is") {
        if let Some(idx) = rest.find('\n') {
            s = rest[idx + 1..].trim_start().to_string();
        }
    }
    s
}

/// Pull a clean Mermaid definition out of the raw model output: drop a fenced
/// ```mermaid block if present, trim, and discard a leading "Here is…" line.
fn sanitize_mermaid(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        // rest may begin with "mermaid\n" or directly with the body.
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].to_string();
        } else {
            s = after_lang.to_string();
        }
        s = s.trim().to_string();
    }
    // Drop any leading non-diagram preamble line the model slipped in.
    if !is_valid_mermaid_header(&s) {
        if let Some(idx) = s.find("flowchart").or_else(|| s.find("graph ")) {
            s = s[idx..].trim().to_string();
        }
    }
    s.trim().to_string()
}

/// A Mermaid graph definition must start with `flowchart` or `graph`.
fn is_valid_mermaid_header(s: &str) -> bool {
    let head = s.trim_start();
    head.starts_with("flowchart") || head.starts_with("graph ") || head.starts_with("graph\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_validation() {
        assert!(is_valid_mermaid_header("flowchart TD\nA-->B"));
        assert!(is_valid_mermaid_header("graph LR\nA-->B"));
        assert!(!is_valid_mermaid_header("Here is your diagram"));
        assert!(!is_valid_mermaid_header("```mermaid"));
    }

    #[test]
    fn sanitize_strips_fenced_mermaid() {
        let raw = "```mermaid\nflowchart TD\nA[UI] --> B[API]\n```";
        let out = sanitize_mermaid(raw);
        assert_eq!(out, "flowchart TD\nA[UI] --> B[API]");
        assert!(is_valid_mermaid_header(&out));
    }

    #[test]
    fn sanitize_recovers_after_preamble() {
        let raw = "Here is the diagram:\nflowchart TD\nA --> B";
        let out = sanitize_mermaid(raw);
        assert!(out.starts_with("flowchart TD"));
        assert!(is_valid_mermaid_header(&out));
    }

    #[test]
    fn sanitize_plain_passthrough() {
        let raw = "flowchart TD\nA --> B";
        assert_eq!(sanitize_mermaid(raw), raw);
    }

    #[test]
    fn strip_prose_removes_fence_and_preamble() {
        assert_eq!(strip_prose_fence("```\nhello world\n```"), "hello world");
        assert_eq!(strip_prose_fence("Here is the summary:\nbody text"), "body text");
        assert_eq!(strip_prose_fence("plain prose"), "plain prose");
    }

    #[test]
    fn truncate_caps_long_trees() {
        let blob = "x".repeat(MAX_TREE_BYTES + 100);
        let out = truncate(blob, MAX_TREE_BYTES);
        assert!(out.contains("(tree truncated)"));
        assert!(out.len() < MAX_TREE_BYTES + 60);
    }

    #[test]
    fn cache_key_combines_root_and_sha() {
        let k = cache_key(Path::new("/tmp/proj"), "abc123");
        assert_eq!(k, "/tmp/proj|abc123");
    }
}
