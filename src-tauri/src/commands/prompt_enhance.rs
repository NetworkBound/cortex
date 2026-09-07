use std::time::Duration;

use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

const TIMEOUT: Duration = Duration::from_secs(30);

const ENHANCER_SYSTEM: &str = "\
You are Cortex Prompt Enhancer — a meta-prompting engine that rewrites casual user \
messages into production-grade instructions for AI coding agents (Claude, Codex, Gemini).

Rules:
1. PRESERVE the user's original intent exactly. Never add goals they didn't ask for.
2. Add structural clarity: break vague requests into numbered steps.
3. Add specificity: if the user says \"fix the bug\", keep the scope but add \"identify the \
root cause, implement the fix, verify no regressions\".
4. Add output expectations: specify that the agent should show file paths, explain changes, \
and verify its work.
5. Add agentic best practices:
   - \"Think step-by-step before making changes\"
   - \"Read relevant files before editing\"
   - \"Run tests or type-check after changes\"
   - \"Commit to a plan, then execute fully\"
6. Keep it concise — enhanced prompts should be 2-4x the original length, NOT 10x.
7. Use imperative voice: \"Refactor X\" not \"Could you please refactor X\".
8. For coding tasks, include: scope constraints, quality bar, verification steps.
9. NEVER wrap the output in markdown code fences or quotes — return ONLY the enhanced prompt text.
10. If the user's message is already detailed and specific, make minimal changes.

Output ONLY the enhanced prompt — no preamble, no explanation, no wrapping.";

const AUTO_AGENT_SYSTEM: &str = "\
You are Cortex Agent Architect. Given a project description and file listing, generate \
a custom agent system-prompt (instructions) tailored to that specific project.

The instructions you generate will be injected as the system prompt for an AI coding agent \
working on this project. They should:

1. Define the agent's role based on the project's tech stack and structure.
2. List key conventions observed in the codebase (naming, patterns, frameworks).
3. Specify how the agent should approach tasks (read-first, test-after, etc.).
4. Include project-specific constraints (e.g. \"this is a Tauri app — backend is Rust, \
frontend is React/TypeScript\").
5. List files/directories the agent should be aware of.
6. Specify coding standards: error handling approach, test patterns, commit style.
7. Keep under 800 words — dense and actionable, not verbose.

Output ONLY the system prompt text. No preamble, no markdown fences, no explanation.";

#[tauri::command]
pub async fn enhance_prompt(
    message: String,
    context: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("Empty message".into());
    }

    let mut user_content = format!("Enhance this prompt for a coding agent:\n\n{}", message);
    if let Some(ctx) = context {
        if !ctx.is_empty() {
            user_content.push_str(&format!(
                "\n\nProject context: {}",
                ctx
            ));
        }
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: ENHANCER_SYSTEM.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user_content,
            },
        ],
        stream: true,
        temperature: Some(0.4),
    };

    let (tx, mut rx) = mpsc::channel::<StreamItem>(128);
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
        Ok(body) => {
            if body.trim().is_empty() {
                Err("Enhancer returned empty response".into())
            } else {
                Ok(body.trim().to_string())
            }
        }
        Err(_) => Err("Prompt enhancement timed out".into()),
    }
}

#[tauri::command]
pub async fn generate_agent_instructions(
    project_root: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if project_root.is_empty() {
        return Err("No project selected".into());
    }

    let root = std::path::Path::new(&project_root);
    if !root.is_dir() {
        return Err("Project root is not a directory".into());
    }

    let mut file_list = Vec::new();
    let mut tech_hints = Vec::new();

    for entry in walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.')
                && name != "node_modules"
                && name != "target"
                && name != "dist"
                && name != "__pycache__"
                && name != ".git"
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            file_list.push(rel.to_string());

            let fname = entry.file_name().to_string_lossy().to_lowercase();
            if fname == "cargo.toml" {
                tech_hints.push("Rust/Cargo project");
            } else if fname == "package.json" {
                tech_hints.push("Node.js/npm project");
            } else if fname == "tsconfig.json" {
                tech_hints.push("TypeScript project");
            } else if fname == "tauri.conf.json" || fname == "tauri.conf.json5" {
                tech_hints.push("Tauri desktop app");
            } else if fname == "pyproject.toml" || fname == "setup.py" {
                tech_hints.push("Python project");
            } else if fname == "go.mod" {
                tech_hints.push("Go project");
            } else if fname == "dockerfile" {
                tech_hints.push("Docker containerized");
            } else if fname == ".github" {
                tech_hints.push("GitHub CI/CD");
            }
        }
    }

    tech_hints.sort();
    tech_hints.dedup();

    let file_listing = if file_list.len() > 200 {
        let mut truncated = file_list[..200].to_vec();
        truncated.push(format!("... and {} more files", file_list.len() - 200));
        truncated.join("\n")
    } else {
        file_list.join("\n")
    };

    let user_content = format!(
        "Generate agent instructions for this project:\n\n\
         Project root: {}\n\
         Detected tech: {}\n\n\
         File listing (depth 3):\n{}\n\n\
         Generate a tailored system prompt for an AI coding agent working on this project.",
        project_root,
        if tech_hints.is_empty() {
            "unknown".to_string()
        } else {
            tech_hints.join(", ")
        },
        file_listing,
    );

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: AUTO_AGENT_SYSTEM.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user_content,
            },
        ],
        stream: true,
        temperature: Some(0.3),
    };

    let (tx, mut rx) = mpsc::channel::<StreamItem>(128);
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

    match tokio::time::timeout(Duration::from_secs(60), async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => {
            if body.trim().is_empty() {
                Err("Agent architect returned empty response".into())
            } else {
                Ok(body.trim().to_string())
            }
        }
        Err(_) => Err("Agent instruction generation timed out".into()),
    }
}
