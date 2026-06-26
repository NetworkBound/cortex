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
