use crate::app_state::AppState;
use crate::commands::project_doc;
use crate::memory::{markdown::read_entry, sources};
use crate::observability::tracing_store::{StoredMessage, TracingStore};
use crate::orchestrator::trust;
use crate::projects::rules;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;

#[tauri::command]
pub async fn load_session_messages(
    session_id: String,
    store: State<'_, TracingStore>,
) -> Result<Vec<StoredMessage>, String> {
    store.load_session_messages(&session_id).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct RecordMessageArgs {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub agent_id: Option<String>,
    pub content: String,
    pub run_id: Option<String>,
    pub reasoning: Option<String>,
    #[serde(default)]
    pub project_root: Option<String>,
}

#[tauri::command]
pub async fn record_message(
    args: RecordMessageArgs,
    store: State<'_, TracingStore>,
) -> Result<(), String> {
    let msg = StoredMessage {
        id: args.id,
        session_id: args.session_id,
        ts: chrono::Utc::now().timestamp_millis(),
        role: args.role,
        agent_id: args.agent_id,
        content: args.content,
        run_id: args.run_id,
        reasoning: args.reasoning,
        project_root: args.project_root,
    };
    store.record_message(&msg).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
pub struct ProjectBootstrap {
    pub session_id: String,
    pub messages: Vec<StoredMessage>,
    pub is_resume: bool,
    pub context_files_loaded: usize,
}

/// Click-a-project flow: either resume the most-recent chat that touched
/// this project root, or generate a fresh session seeded with the project's
/// CLAUDE.md + runbooks + claude-memory + Obsidian vault as a system message.
#[tauri::command]
pub async fn bootstrap_project_session(
    project_root: String,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<ProjectBootstrap, String> {
    if let Ok(Some(existing)) = store.latest_session_for_project(&project_root) {
        let msgs = store.load_session_messages(&existing).map_err(|e| e.to_string())?;
        if !msgs.is_empty() {
            return Ok(ProjectBootstrap {
                session_id: existing,
                messages: msgs,
                is_resume: true,
                context_files_loaded: 0,
            });
        }
    }

    // No prior session — generate a fresh one seeded with project context
    let session_id = format!("session-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().timestamp_millis();
    let project_path = PathBuf::from(&project_root);
    let vault = state.config.read().obsidian_vault.clone();
    let (context_msg, n_files) = gather_project_context(&project_path, vault.as_deref());

    let msg = StoredMessage {
        id: format!("ctx-{}", uuid::Uuid::new_v4()),
        session_id: session_id.clone(),
        ts: now,
        role: "system".to_string(),
        agent_id: None,
        content: context_msg,
        run_id: None,
        reasoning: None,
        project_root: Some(project_root.clone()),
    };
    let _ = store.record_message(&msg);

    Ok(ProjectBootstrap {
        session_id,
        messages: vec![msg],
        is_resume: false,
        context_files_loaded: n_files,
    })
}

/// Builds the auto-loaded system context for a fresh project session.
///
/// Sources, in order:
/// 1. Project name + working directory header
/// 2. Root-level prompt files: `CLAUDE.md`, `CLAUDE.local.md`, `AGENTS.md`, `README.md`
/// 3. **`.cortex/rules/*.md`** (Cursor-style per-project rules, depth 1, each capped at 4000 chars).
///    Drop any of these into `<project_root>/.cortex/rules/` to have them prepended
///    to every new Cortex session for this project. Suggested seed files:
///      - `architecture.md` — high-level system constraints
///      - `style.md` — coding conventions
///      - `dangerous.md` — things the AI must never do
///    If the `.cortex/rules/` directory does not exist, this step is a no-op.
/// 4. `runbooks/` listing (if present)
/// 5. Memory & Obsidian note previews from `sources::default_sources`
fn gather_project_context(project: &Path, obsidian: Option<&Path>) -> (String, usize) {
    let mut sections: Vec<String> = Vec::new();
    let mut n_files = 0;
    let project_name = project.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    sections.push(format!(
        "# Cortex project session — {}\n\nWorking directory: `{}`\n",
        project_name,
        project.display(),
    ));

    // Codex-style AGENTS.md hierarchy — global → codex → project → cortex →
    // cwd. We inject this once, in merged form, so the model sees a single
    // canonical block instead of the same content appearing twice (once via
    // this stack and once via the legacy flat AGENTS.md loader below).
    // Idempotency: the root-level loop below explicitly skips `AGENTS.md`
    // when at least one segment was found here.
    let agents_stack = project_doc::build_stack(project, None);
    let agents_loaded = !agents_stack.is_empty();
    if agents_loaded {
        let merged = project_doc::merged_text(&agents_stack);
        n_files += agents_stack.len();
        sections.push(format!("## AGENTS.md (hierarchical)\n\n{}", merged));
    }

    // Root-level prompt files. `AGENTS.md` is skipped whenever the
    // hierarchical loader picked anything up — otherwise we'd double-prepend
    // the repo's AGENTS.md (Codex #2 idempotency rule).
    for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md", "README.md"] {
        if name == "AGENTS.md" && agents_loaded {
            continue;
        }
        let path = project.join(name);
        if let Ok(body) = std::fs::read_to_string(&path) {
            let trimmed: String = body.chars().take(8000).collect();
            sections.push(format!("## {}\n\n{}", name, trimmed));
            n_files += 1;
        }
    }

    // Cursor-style per-project rules with activation taxonomy. At bootstrap
    // there is no user message yet, so only `alwaysApply` rules fire —
    // matching the original loader's behaviour for legacy rule files. Glob /
    // description / manual rules are evaluated per-turn elsewhere.
    //
    // **Trust gate**: untrusted projects skip `.cortex/rules/*.md` entirely.
    // Same applies to `.cortex/danger.toml` / `.cortex/approvals.toml` (loaded
    // in `chat.rs` — see the trust check there). An untrusted project gets
    // CLAUDE.md and root-level prompts only.
    let trusted = trust::is_trusted(project);
    if trusted {
        let all_rules = rules::load_rules(project);
        let active = rules::select_active(&all_rules, "");
        if !active.is_empty() {
            let body = active
                .iter()
                .map(|r| format!("### {}\n\n{}", r.name, r.body))
                .collect::<Vec<_>>()
                .join("\n\n");
            n_files += active.len();
            sections.push(format!("## Cortex rules\n\n{}", body));
        }
    } else {
        sections.push(
            "## Cortex rules\n\n_This project is untrusted. `.cortex/rules/*.md`, `.cortex/danger.toml`, and `.cortex/approvals.toml` are not loaded. Sandbox is forced to read-only. Click \"Trust this project\" to enable full project context._"
                .to_string(),
        );
    }

    let runbooks = project.join("runbooks");
    if runbooks.exists() {
        let mut listing: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&runbooks) {
            for e in entries.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    if name.ends_with(".md") {
                        listing.push(format!("- `{}`", name));
                        n_files += 1;
                    }
                }
            }
        }
        if !listing.is_empty() {
            listing.sort();
            sections.push(format!("## runbooks/ ({} files)\n\n{}", listing.len(), listing.join("\n")));
        }
    }

    let srcs = sources::default_sources(Some(project), obsidian);
    let mut memory_lines: Vec<String> = Vec::new();
    for src in &srcs {
        for p in sources::walk_markdown(src).into_iter().take(8) {
            if let Ok(entry) = read_entry(&p) {
                let preview: String = entry.body.chars().take(220).collect();
                memory_lines.push(format!(
                    "- **{}** ({}): {}",
                    entry.title.as_deref().unwrap_or("untitled"),
                    src.label,
                    preview.replace('\n', " "),
                ));
                n_files += 1;
                if memory_lines.len() >= 30 { break; }
            }
        }
        if memory_lines.len() >= 30 { break; }
    }
    if !memory_lines.is_empty() {
        sections.push(format!("## Memory & Obsidian notes\n\n{}", memory_lines.join("\n")));
    }

    sections.push(
        "---\nThis context was auto-loaded by Cortex. Ask anything about this project — your CLAUDE.md instructions, runbooks, and memory are all available to me. Type @ to insert any project file."
            .to_string(),
    );

    (sections.join("\n\n"), n_files)
}

// ───────────────────────────────────────────────────────────────────────────
// Save chat to Brain — export a session (incl. imported Claude.ai / ChatGPT
// history) to the Obsidian vault as a self-contained Markdown note. Closes the
// chat-history → second-brain loop: anything you've imported or chatted becomes
// a first-class, linkable note in your vault.
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ExportToVaultResult {
    pub written_path: String,
    pub bytes: usize,
    pub message_count: usize,
}

/// Export one chat session to the user's Obsidian vault as Markdown. Works for
/// imported (Claude.ai/ChatGPT) sessions and native Cortex chats alike. Writes
/// to `<vault>/Cortex Chats/<slug>-<id>.md`. No model/provider needed — pure
/// local read + file write.
#[tauri::command]
pub async fn export_session_to_vault(
    session_id: String,
    state: State<'_, AppState>,
    store: State<'_, TracingStore>,
) -> Result<ExportToVaultResult, String> {
    let vault = state.config.read().obsidian_vault.clone();
    export_session_markdown(store.inner(), vault, &session_id)
}

/// Shared export core, reused by the Tauri command AND the mobile HTTP endpoint
/// (`POST /api/sessions/{id}/export`). Loads the session, resolves the vault,
/// renders Markdown, and writes `<vault>/Cortex Chats/<slug>-<id>.md`.
pub(crate) fn export_session_markdown(
    store: &TracingStore,
    configured_vault: Option<PathBuf>,
    session_id: &str,
) -> Result<ExportToVaultResult, String> {
    let msgs = store.load_session_messages(session_id).map_err(|e| e.to_string())?;
    if msgs.is_empty() {
        return Err("that session has no messages to export".to_string());
    }
    let vault = resolve_vault_dir(configured_vault)
        .ok_or_else(|| "no Obsidian vault configured (and ~/Documents is unavailable)".to_string())?;
    let dir = vault.join("Cortex Chats");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {} failed: {e}", dir.display()))?;
    let now_iso = chrono::Utc::now().to_rfc3339();
    let (path, bytes) = write_session_export(&dir, session_id, &msgs, &now_iso)?;
    Ok(ExportToVaultResult {
        written_path: path.display().to_string(),
        bytes,
        message_count: msgs.len(),
    })
}

/// Configured Obsidian vault if set, else the default `~/Documents/Cortex Brain`
/// (matches `daily_journal::brain_dir` and the rest of the app).
fn resolve_vault_dir(configured: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = configured {
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join("Documents").join("Cortex Brain"))
}

/// Render + write the export. Takes an explicit `dir` so it is unit-testable
/// against a temp directory without the Tauri command/State wrapper.
fn write_session_export(
    dir: &Path,
    session_id: &str,
    msgs: &[StoredMessage],
    now_iso: &str,
) -> Result<(PathBuf, usize), String> {
    let (title, source) = derive_title_and_source(msgs);
    let content = render_session_markdown(&title, &source, session_id, msgs, now_iso);
    let filename = safe_filename(&title, session_id);
    let path = dir.join(&filename);
    // Path-traversal guard: the resolved path must stay inside `dir`.
    if !path.starts_with(dir) {
        return Err("refusing to write outside the vault".to_string());
    }
    std::fs::write(&path, &content).map_err(|e| format!("write {} failed: {e}", path.display()))?;
    Ok((path, content.as_bytes().len()))
}

/// Title + provider label from a session's messages. Imported sessions carry a
/// `[Imported from <source> — <title>]` banner on the first message and an
/// `import:<source>` agent_id; native chats fall back to the first user line.
fn derive_title_and_source(msgs: &[StoredMessage]) -> (String, String) {
    let source = msgs
        .iter()
        .find_map(|m| m.agent_id.as_deref())
        .and_then(|a| a.strip_prefix("import:"))
        .map(pretty_source)
        .unwrap_or_else(|| "Cortex".to_string());

    let first_non_system = msgs.iter().find(|m| m.role != "system");
    let raw = first_non_system.map(|m| m.content.as_str()).unwrap_or("");
    let title = parse_import_banner_title(raw).unwrap_or_else(|| first_line(raw));
    let title = title.trim();
    let title = if title.is_empty() { "Untitled chat" } else { title };
    (truncate_chars(title, 80), source)
}

fn pretty_source(s: &str) -> String {
    match s {
        "claude.ai" => "Claude.ai".to_string(),
        "chatgpt" => "ChatGPT".to_string(),
        other => other.to_string(),
    }
}

/// Extract `<title>` from a leading `[Imported from <src> — <title>]` banner.
fn parse_import_banner_title(s: &str) -> Option<String> {
    let s = s.trim_start();
    let inner = s.strip_prefix('[')?;
    let end = inner.find(']')?;
    let banner = &inner[..end];
    let rest = banner.strip_prefix("Imported from ")?;
    let (_src, title) = rest.split_once(" — ").or_else(|| rest.split_once(" - "))?;
    let t = title.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Strip a leading import banner so the first message body isn't duplicated
/// (the title already comes from it).
fn strip_import_banner(content: &str) -> String {
    let t = content.trim_start();
    if t.starts_with("[Imported from ") {
        if let Some(end) = t.find(']') {
            return t[end + 1..].trim_start().to_string();
        }
    }
    content.to_string()
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}

/// Filesystem-safe `<title-slug>-<id-tail>.md`. Non-alphanumerics become `-`,
/// runs collapse, so `../` and path separators can never survive.
fn safe_filename(title: &str, session_id: &str) -> String {
    let slug = {
        let s = slugify(title, 60);
        if s.is_empty() { "chat".to_string() } else { s }
    };
    let id = slugify(session_id, 64);
    let chars: Vec<char> = id.chars().collect();
    let start = chars.len().saturating_sub(16);
    let tail: String = chars[start..].iter().collect();
    let tail = if tail.is_empty() { "session".to_string() } else { tail };
    format!("{slug}-{tail}.md")
}

fn slugify(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    cleaned
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(max)
        .collect()
}

fn yaml_escape(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
    )
}

/// Render the whole session as a self-contained Markdown note with YAML
/// frontmatter (so Obsidian/Cortex can index + link it).
fn render_session_markdown(
    title: &str,
    source: &str,
    session_id: &str,
    msgs: &[StoredMessage],
    now_iso: &str,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("kind: chat-export\n");
    out.push_str(&format!("source: {}\n", yaml_escape(source)));
    out.push_str(&format!("title: {}\n", yaml_escape(title)));
    out.push_str(&format!("session_id: {}\n", yaml_escape(session_id)));
    out.push_str(&format!("exported_at: {now_iso}\n"));
    out.push_str(&format!("messages: {}\n", msgs.len()));
    out.push_str("tags: [cortex, chat-export]\n");
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!(
        "> Imported from {source} · {} messages · saved by Cortex\n\n",
        msgs.len()
    ));
    for (i, m) in msgs.iter().enumerate() {
        let label = match m.role.as_str() {
            "user" => "🧑 You",
            "assistant" => "🤖 Assistant",
            "system" => "⚙️ System",
            other => other,
        };
        let body = if i == 0 {
            strip_import_banner(&m.content)
        } else {
            m.content.clone()
        };
        out.push_str(&format!("### {label}\n\n{}\n\n", body.trim()));
    }
    out
}

#[cfg(test)]
mod export_tests {
    use super::*;

    fn msg(role: &str, agent: Option<&str>, content: &str) -> StoredMessage {
        StoredMessage {
            id: format!("m-{role}-{}", content.len()),
            session_id: "session-import-claudeai-f3d2af7b75688824".to_string(),
            ts: 1,
            role: role.to_string(),
            agent_id: agent.map(|s| s.to_string()),
            content: content.to_string(),
            run_id: None,
            reasoning: None,
            project_root: None,
        }
    }

    #[test]
    fn parse_import_banner_extracts_title() {
        assert_eq!(
            parse_import_banner_title("[Imported from claude.ai — Speedify on Proxmox]\n\nhi"),
            Some("Speedify on Proxmox".to_string())
        );
        assert_eq!(parse_import_banner_title("just a normal message"), None);
        assert_eq!(parse_import_banner_title("[malformed banner"), None);
    }

    #[test]
    fn derive_title_and_source_imported_vs_native() {
        let imported = vec![
            msg("user", Some("import:claude.ai"), "[Imported from claude.ai — Speedify on Proxmox]\n\nhow to use speedify"),
            msg("assistant", Some("import:claude.ai"), "Here's how..."),
        ];
        let (t, s) = derive_title_and_source(&imported);
        assert_eq!(t, "Speedify on Proxmox");
        assert_eq!(s, "Claude.ai");

        let native = vec![msg("user", None, "Refactor the auth module please")];
        let (t2, s2) = derive_title_and_source(&native);
        assert_eq!(t2, "Refactor the auth module please");
        assert_eq!(s2, "Cortex");
    }

    #[test]
    fn safe_filename_is_path_safe() {
        let f = safe_filename("../../etc/passwd", "session-../../x");
        assert!(f.ends_with(".md"));
        assert!(!f.contains('/'));
        assert!(!f.contains('\\'));
        assert!(!f.contains(".."));
    }

    #[test]
    fn render_has_frontmatter_and_turns() {
        let msgs = vec![
            msg("user", Some("import:chatgpt"), "[Imported from chatgpt — Trip plan]\n\nplan a trip"),
            msg("assistant", Some("import:chatgpt"), "Sure, here is a plan."),
        ];
        let md = render_session_markdown("Trip plan", "ChatGPT", "sid-123", &msgs, "2026-06-27T00:00:00Z");
        assert!(md.starts_with("---\nkind: chat-export\n"));
        assert!(md.contains("source: \"ChatGPT\""));
        assert!(md.contains("title: \"Trip plan\""));
        assert!(md.contains("# Trip plan"));
        assert!(md.contains("### 🧑 You"));
        assert!(md.contains("plan a trip"));
        // banner stripped from first message body, not duplicated in the turn
        assert!(!md.contains("[Imported from chatgpt"));
        assert!(md.contains("### 🤖 Assistant"));
        assert!(md.contains("Sure, here is a plan."));
    }

    #[test]
    fn write_session_export_writes_inside_dir() {
        let dir = std::env::temp_dir().join(format!("cortex-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let msgs = vec![
            msg("user", Some("import:claude.ai"), "[Imported from claude.ai — Hello world]\n\nhi there"),
            msg("assistant", Some("import:claude.ai"), "hello!"),
        ];
        let (path, bytes) =
            write_session_export(&dir, "session-import-claudeai-abc123def456ffff", &msgs, "2026-06-27T00:00:00Z")
                .unwrap();
        assert!(path.starts_with(&dir));
        assert!(path.exists());
        assert!(bytes > 0);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# Hello world"));
        assert!(body.contains("hi there"));
        assert!(body.contains("hello!"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
