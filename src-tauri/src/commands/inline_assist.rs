//! Inline assist — selection-scoped AI edits from the editor pane.
//!
//! The editor sends the current selection plus surrounding context and a
//! natural-language instruction; we run ONE completion through the adapter
//! registry (`agents::oneshot`, same routing as chat/eval — so a Claude slug
//! reaches the local CLI, `ollama:tag` the Ollama server, anything else the
//! gateway default) and return the replacement text. The frontend previews
//! the diff and applies it as a normal CodeMirror transaction, so the edit is
//! undo-able and flows through the existing dirty/save pipeline — we never
//! touch the file on disk here.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tauri::State;

use crate::agents::oneshot;
use crate::app_state::AppState;

/// Wall-clock cap on the whole completion. Selection rewrites are bigger than
/// ghost-text completions (which cap at 5s) but the popover is modal-ish UI —
/// past a minute it's a miss, not a wait.
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
pub struct InlineAssistArgs {
    /// The exact selected text to rewrite.
    pub selection: String,
    /// Up to ~50 lines immediately preceding the selection.
    #[serde(default)]
    pub before: String,
    /// Up to ~20 lines immediately following the selection.
    #[serde(default)]
    pub after: String,
    /// Human-readable language hint ("TypeScript", "Rust", …).
    #[serde(default)]
    pub language: Option<String>,
    /// What to do to the selection ("add error handling", "make async", …).
    pub instruction: String,
    /// Composer-picker model slug; `None` routes to the default adapter.
    #[serde(default)]
    pub model: Option<String>,
    /// File path, hint-only (steers the model, never read here).
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct InlineAssistResult {
    pub replacement: String,
    /// Which model/agent actually served the request (registry id when the
    /// caller didn't pin a slug).
    pub model: String,
    pub latency_ms: i64,
}

/// E2E-only deterministic stand-in for the LLM call. Under `CORTEX_E2E=1`,
/// instructions beginning with the magic markers short-circuit before any
/// adapter is touched so the probe can verify the command end-to-end
/// (registration, arg shape, result shape) fully offline:
///   `[[e2e:assist]]`     → Ok(selection uppercased)
///   `[[e2e:assist-err]]` → Err — exercises the failure path
fn e2e_fake_result(instruction: &str, selection: &str) -> Option<Result<String, String>> {
    let t = instruction.trim_start();
    if t.starts_with("[[e2e:assist]]") {
        return Some(Ok(selection.to_uppercase()));
    }
    if t.starts_with("[[e2e:assist-err]]") {
        return Some(Err("e2e fake assist failure".into()));
    }
    None
}

#[tauri::command]
pub async fn inline_assist(
    args: InlineAssistArgs,
    state: State<'_, AppState>,
) -> Result<InlineAssistResult, String> {
    let started = Instant::now();
    if args.selection.trim().is_empty() {
        return Err("nothing selected".into());
    }
    if args.instruction.trim().is_empty() {
        return Err("empty instruction".into());
    }

    if let Some(fake) = crate::commands::e2e::e2e_enabled()
        .then(|| e2e_fake_result(&args.instruction, &args.selection))
        .flatten()
    {
        return fake.map(|replacement| InlineAssistResult {
            replacement,
            model: "e2e-fake".into(),
            latency_ms: started.elapsed().as_millis() as i64,
        });
    }

    let prompt = build_prompt(&args);

    // Resilient: a transient provider blip retries with backoff, then falls
    // through to any configured fallback model before surfacing the error.
    let outcome = tokio::time::timeout(
        TIMEOUT,
        oneshot::complete_resilient(&state.registry, args.model.clone(), prompt),
    )
    .await
    .map_err(|_| format!("inline assist timed out after {}s", TIMEOUT.as_secs()))??;

    let replacement = sanitize(&outcome.text, &args.selection);
    if replacement.trim().is_empty() {
        return Err("the model returned an empty rewrite".into());
    }
    Ok(InlineAssistResult {
        // Report the model that actually answered (may differ from the pick if
        // a fallback kicked in), falling back to the resolved adapter id.
        model: args.model.clone().or(outcome.model).unwrap_or(outcome.agent_id),
        replacement,
        latency_ms: started.elapsed().as_millis() as i64,
    })
}

fn build_prompt(args: &InlineAssistArgs) -> String {
    let lang = args
        .language
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("plain text");
    let file = args
        .path
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(unsaved buffer)");
    format!(
        "You are an expert code editor performing a precise selection rewrite.\n\
         Rewrite ONLY the selected code according to the instruction. Return ONLY \
         the replacement for the selection — no explanation, no markdown fences, \
         no surrounding context, and keep the original indentation style so the \
         result drops cleanly into place.\n\n\
         Language: {lang}\n\
         File: {file}\n\
         --- CONTEXT BEFORE SELECTION ---\n{before}\n\
         --- SELECTED CODE ---\n{selection}\n\
         --- CONTEXT AFTER SELECTION ---\n{after}\n\
         --- INSTRUCTION ---\n{instruction}\n\
         --- REPLACEMENT ---\n",
        lang = lang,
        file = file,
        before = args.before,
        selection = args.selection,
        after = args.after,
        instruction = args.instruction.trim(),
    )
}

/// Strip the markdown fence chat models love to add despite instructions, and
/// normalize the trailing newline against the selection (the replacement
/// substitutes the selection verbatim, so a spurious trailing `\n` would push
/// the rest of the line down; a missing one would glue lines together).
fn sanitize(raw: &str, selection: &str) -> String {
    let mut s = raw.trim_start_matches('\n').to_string();

    if let Some(rest) = s.strip_prefix("```") {
        // Skip the optional language tag on the fence line.
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].to_string();
        }
    }

    let selection_ends_nl = selection.ends_with('\n');
    while s.ends_with('\n') && !selection_ends_nl {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    if selection_ends_nl && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_markers_short_circuit_and_real_instructions_pass_through() {
        assert!(matches!(
            e2e_fake_result("[[e2e:assist]] upper", "hello"),
            Some(Ok(s)) if s == "HELLO"
        ));
        assert!(matches!(e2e_fake_result("  [[e2e:assist-err]]", "x"), Some(Err(_))));
        assert!(e2e_fake_result("make this async", "x").is_none());
        assert!(e2e_fake_result("", "x").is_none());
    }

    #[test]
    fn build_prompt_carries_all_sections() {
        let p = build_prompt(&InlineAssistArgs {
            selection: "let x = 1;".into(),
            before: "fn main() {".into(),
            after: "}".into(),
            language: Some("Rust".into()),
            instruction: "rename x to count".into(),
            model: None,
            path: Some("src/main.rs".into()),
        });
        assert!(p.contains("Language: Rust"));
        assert!(p.contains("File: src/main.rs"));
        assert!(p.contains("--- SELECTED CODE ---\nlet x = 1;"));
        assert!(p.contains("--- INSTRUCTION ---\nrename x to count"));
    }

    #[test]
    fn sanitize_strips_fence_and_matches_selection_newline() {
        // Fenced reply, selection without trailing newline → fence + trailing \n dropped.
        assert_eq!(sanitize("```rust\nlet y = 2;\n```", "let x = 1;"), "let y = 2;");
        // Selection ends with \n → replacement keeps exactly one.
        assert_eq!(sanitize("let y = 2;", "let x = 1;\n"), "let y = 2;\n");
        assert_eq!(sanitize("let y = 2;\n\n\n", "let x = 1;"), "let y = 2;");
        // Plain multi-line passes through.
        assert_eq!(sanitize("a\nb", "old\nlines"), "a\nb");
    }
}
