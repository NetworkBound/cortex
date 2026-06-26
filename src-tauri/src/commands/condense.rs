//! LLM-backed conversation condensing — the real engine behind `/compact`.
//!
//! The frontend `/compact` command used to fold older turns into a *heuristic*
//! placeholder (first-60-chars topics + regex-matched "decision" lines + edited
//! file paths) with no model call — it discarded the actual reasoning. This is
//! the flagship Cline "Condense Context" / Claude-Code `/compact` behaviour: ask
//! the model to write a faithful, structured summary of the older turns so the
//! salient context survives compaction instead of being thrown away.
//!
//! This module owns the **pure** transcript/prompt building (unit-tested) plus
//! the live one-shot completion that runs the older turns through the routed
//! adapter (`run_condense`). The latter mirrors `chat::run_planner_phase`: build
//! a `ChatRequest`, run the adapter to completion, collect `Token` deltas. The
//! routed adapter means condensing works with whatever model the chat uses —
//! claude-cli, a local Ollama, or the Cortex Gateway.

use crate::agents::{AgentEvent, ChatRequest, ChatTurn};
use crate::app_state::AppState;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;
use tokio::sync::mpsc;

/// The condense instruction. Asks for a faithful, structured summary that
/// preserves the context a fresh continuation needs — the Cline/Claude-Code
/// "detailed summary" shape, trimmed to what actually carries forward.
const CONDENSE_INSTRUCTION: &str = "You are condensing the EARLIER part of an ongoing \
conversation so it can be replaced by a compact summary while the chat continues. \
Write a faithful, information-dense summary of the conversation below. Preserve what a \
continuation would need and DROP nothing important. Use these sections (omit a section \
only if it is genuinely empty):\n\
- **Context**: what the user is trying to accomplish overall.\n\
- **Decisions**: choices made and why.\n\
- **Changes & artifacts**: files created/edited and the gist of each change.\n\
- **Current state**: where things stand right now.\n\
- **Open threads / next steps**: anything unresolved or planned.\n\
Be specific (keep file paths, names, and key values). Do NOT invent details that are not \
in the conversation. Return ONLY the summary, no preamble.";

/// 45s wall clock — summarising a long history is heavier than the inline /
/// planner single-shots, but it's still a one-off ask, not interactive.
const CONDENSE_TIMEOUT: Duration = Duration::from_secs(45);

/// Cap the transcript fed to the model so a giant history doesn't blow the
/// context window. The newest of the folded turns win (they're closest to the
/// live conversation), mirroring `session_summary::build_transcript`.
const MAX_TRANSCRIPT_CHARS: usize = 24_000;

#[derive(Debug, Serialize, Clone)]
pub struct CondenseResult {
    /// The model-written summary of the folded turns.
    pub summary: String,
    /// How many turns were folded into the summary.
    pub folded: usize,
    /// The model that produced the summary (the routed/resolved id).
    pub model: String,
}

/// Condense the given (older) turns into a single model-written summary.
///
/// The frontend slices off the turns it wants to fold (everything except the
/// most-recent window) and passes them here; this returns the summary text the
/// caller splices in as one synthetic system message. Errors (empty input, no
/// available model, timeout, empty completion) surface to the frontend, which
/// falls back to the cheap heuristic summary so `/compact` never hard-fails.
#[tauri::command]
pub async fn condense_history(
    turns: Vec<ChatTurn>,
    model: Option<String>,
    state: State<'_, AppState>,
) -> Result<CondenseResult, String> {
    let turns: Vec<ChatTurn> = turns
        .into_iter()
        .filter(|t| !t.content.trim().is_empty())
        .collect();
    if turns.is_empty() {
        return Err("no turns to condense".into());
    }
    let folded = turns.len();

    // Resolve a canonical model id (alias → catalog id; unknown/Ollama slugs
    // pass through verbatim) so routing picks the right adapter.
    let resolved = model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::orchestrator::aliases::resolve_model);

    let message = build_condense_message(&turns);

    // Resolve the adapter inside a lexical block so the (non-Send) registry
    // read guard is dropped before the `.await` below (same constraint the
    // architect planner phase solved).
    let (adapter, model_id) = {
        let registry = state.registry.read();
        let req = ChatRequest {
            session_id: String::new(),
            message: String::new(),
            project_root: None,
            history: Vec::new(),
            model: resolved.clone(),
            reasoning_effort: None,
        };
        let decision = crate::orchestrator::route(&req, &registry, None);
        let id = decision
            .agents
            .into_iter()
            .next()
            .ok_or_else(|| "no model available to condense".to_string())?;
        let adapter = registry
            .get(&id)
            .ok_or_else(|| format!("no adapter registered for '{id}'"))?;
        // The model the adapter will actually run: the resolved pick, else the
        // routed agent id so the caller can show what produced the summary.
        let model_id = resolved.clone().unwrap_or(id);
        (adapter, model_id)
    };

    let summary = run_condense(adapter, model_id.clone(), message, |_| {})
        .await
        .ok_or_else(|| "condense produced no output (timed out or empty)".to_string())?;

    Ok(CondenseResult {
        summary: summary.trim().to_string(),
        folded,
        model: model_id,
    })
}

/// Build the full condense prompt: the instruction followed by a chronological,
/// role-labelled transcript of the turns (oldest→newest), truncated to
/// [`MAX_TRANSCRIPT_CHARS`] keeping the most-recent turns when over budget.
pub fn build_condense_message(turns: &[ChatTurn]) -> String {
    format!(
        "{CONDENSE_INSTRUCTION}\n\n# Conversation to summarize\n\n{}",
        build_transcript(turns)
    )
}

fn role_label(role: &str) -> &str {
    match role {
        "user" => "User",
        "assistant" => "Assistant",
        "system" => "System",
        other => other,
    }
}

/// Render turns as a plain transcript. Measured newest-first so truncation
/// keeps the most recent turns, then reversed so the prompt reads
/// chronologically.
fn build_transcript(turns: &[ChatTurn]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(turns.len());
    let mut total = 0usize;
    for t in turns.iter().rev() {
        let entry = format!("{}: {}", role_label(&t.role), t.content.trim());
        if total + entry.len() > MAX_TRANSCRIPT_CHARS && !lines.is_empty() {
            lines.push("[…older messages truncated…]".to_string());
            break;
        }
        total += entry.len();
        lines.push(entry);
    }
    lines.reverse();
    lines.join("\n\n")
}

/// Run a single condense completion through `adapter`, collecting streamed
/// `Token` deltas into the summary string. Returns `None` on timeout or when
/// the model produced nothing. Mirrors `chat::run_planner_phase`.
pub async fn run_condense<F>(
    adapter: Arc<dyn crate::agents::AgentAdapter>,
    model: String,
    message: String,
    mut on_token: F,
) -> Option<String>
where
    F: FnMut(&str) + Send,
{
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let req = ChatRequest {
        session_id: String::new(),
        message,
        project_root: None,
        history: Vec::new(),
        model: Some(model),
        reasoning_effort: None,
    };
    let run_fut = {
        let adapter = adapter.clone();
        async move {
            let _ = adapter.run(req, tx).await;
        }
    };
    let collect_fut = async {
        let mut out = String::new();
        while let Some(evt) = rx.recv().await {
            if let AgentEvent::Token { delta } = evt {
                on_token(&delta);
                out.push_str(&delta);
            }
        }
        out
    };
    let out = tokio::time::timeout(CONDENSE_TIMEOUT, async {
        let (_run, out) = tokio::join!(run_fut, collect_fut);
        out
    })
    .await
    .ok()?;
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: &str, content: &str) -> ChatTurn {
        ChatTurn { role: role.into(), content: content.into(), agent: None }
    }

    #[test]
    fn message_carries_instruction_and_transcript() {
        let turns = vec![
            turn("user", "Add a /compact command"),
            turn("assistant", "Done — wired it to the condenser."),
        ];
        let msg = build_condense_message(&turns);
        assert!(msg.contains("condensing the EARLIER part"));
        assert!(msg.contains("# Conversation to summarize"));
        // Role labels are humanised and content preserved.
        assert!(msg.contains("User: Add a /compact command"));
        assert!(msg.contains("Assistant: Done — wired it to the condenser."));
    }

    #[test]
    fn transcript_reads_chronologically() {
        let turns = vec![turn("user", "first"), turn("assistant", "second"), turn("user", "third")];
        let t = build_transcript(&turns);
        let first = t.find("first").unwrap();
        let second = t.find("second").unwrap();
        let third = t.find("third").unwrap();
        assert!(first < second && second < third, "transcript out of order: {t}");
    }

    #[test]
    fn transcript_humanises_unknown_roles() {
        let t = build_transcript(&[turn("tool", "ran grep")]);
        assert!(t.starts_with("tool: ran grep"), "{t}");
    }

    #[test]
    fn transcript_truncation_keeps_newest_turns() {
        // One huge old turn + a small recent one. Over budget → the old turn is
        // dropped (truncation marker present), the newest survives.
        let big = "x".repeat(MAX_TRANSCRIPT_CHARS + 5_000);
        let turns = vec![turn("user", &big), turn("assistant", "recent answer")];
        let t = build_transcript(&turns);
        assert!(t.contains("recent answer"), "newest turn must survive");
        assert!(t.contains("[…older messages truncated…]"), "marker expected");
        assert!(!t.contains(&big), "the oversized old turn should be dropped");
    }

    #[test]
    fn transcript_keeps_at_least_one_oversized_turn() {
        // A single turn that alone exceeds the cap is still kept (we never emit
        // an empty transcript) — the marker only appears once something fits.
        let big = "y".repeat(MAX_TRANSCRIPT_CHARS + 5_000);
        let t = build_transcript(&[turn("user", &big)]);
        assert!(t.contains(&big));
        assert!(!t.contains("truncated"));
    }

    /// Live one-shot condense against a running local Ollama. Ignored by
    /// default (needs `ollama serve` + a pulled model). Run with:
    ///   `cargo test --lib commands::condense::tests::condense_live_ollama -- --ignored`
    /// Proves the real `run_condense` path draws a non-empty summary from a live
    /// model using the exact prompt `build_condense_message` produces.
    #[tokio::test]
    #[ignore]
    async fn condense_live_ollama() {
        use crate::agents::ollama::OllamaAgent;
        let adapter: Arc<dyn crate::agents::AgentAdapter> = Arc::new(OllamaAgent::new(
            "http://127.0.0.1:11434".into(),
            "llama3.2:1b".into(),
        ));
        let turns = vec![
            turn("user", "Let's refactor the auth module to use JWTs."),
            turn("assistant", "I updated src/auth/jwt.rs to sign tokens and added a verify() helper."),
            turn("user", "Also rotate the signing key on startup."),
            turn("assistant", "Done — key rotation wired into src/auth/keys.rs at boot."),
        ];
        let msg = build_condense_message(&turns);
        let summary = run_condense(adapter, "ollama:llama3.2:1b".into(), msg, |_| {})
            .await
            .expect("live condense should return a non-empty summary");
        assert!(summary.len() > 40, "summary too short: {summary:?}");
        eprintln!("LIVE CONDENSE ({} chars):\n{summary}", summary.len());
    }
}
