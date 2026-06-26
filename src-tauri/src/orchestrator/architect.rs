//! Aider-style `/architect` two-phase orchestration.
//!
//! A *planner* model drafts a concise, ordered implementation plan, then an
//! *editor* model executes it with the plan injected into its prompt. This
//! module holds the **pure** decision/formatting logic — which model serves
//! each phase, the planner instruction, and the `<plan>` envelope — so the
//! `chat_send` runtime and the tests share one source of truth and can't drift.
//!
//! The live two-model dispatch (running the planner adapter, streaming its
//! tokens, then running the editor) lives in `commands::chat`; everything here
//! is side-effect-free and unit-tested.

use super::aliases::resolve_model;

/// Default upstream models when the user gives no override and picked no model
/// (Auto mode). The planner gets the stronger reasoning model; the editor a
/// faster execution model — Aider's "smart planner + cheap editor" split. When
/// the user *did* pick a model, both phases default to that pick (see
/// [`planner_model`]/[`editor_model`]) so architect mode never silently swaps
/// the model out from under an explicit choice.
pub const DEFAULT_PLANNER_MODEL: &str = "claude-opus-4-8";
pub const DEFAULT_EDITOR_MODEL: &str = "claude-sonnet-4-6";

/// True when the request opted into architect mode.
pub fn is_active(flag: Option<bool>) -> bool {
    flag.unwrap_or(false)
}

/// Resolve the planner-phase model. Precedence: an explicit override (the
/// `/architect planner_model=…` value) → the model the user explicitly picked
/// for the turn → the project's configured **planner** role default
/// (Continue.dev-style, from `.cortex/model-roles.toml`) → the built-in default.
/// The result is canonicalized through the alias catalog so a short alias
/// (`opus`) resolves to the id the adapter expects.
pub fn planner_model(
    override_model: Option<&str>,
    picked: Option<&str>,
    configured: Option<&str>,
) -> String {
    pick_model(override_model, picked, configured, DEFAULT_PLANNER_MODEL)
}

/// Resolve the editor-phase model. Same precedence as [`planner_model`] (with
/// the configured **editor** role default in the third slot) but falling back to
/// [`DEFAULT_EDITOR_MODEL`] in Auto mode.
pub fn editor_model(
    override_model: Option<&str>,
    picked: Option<&str>,
    configured: Option<&str>,
) -> String {
    pick_model(override_model, picked, configured, DEFAULT_EDITOR_MODEL)
}

fn pick_model(
    override_model: Option<&str>,
    picked: Option<&str>,
    configured: Option<&str>,
    default: &str,
) -> String {
    let nonempty = |s: &&str| !s.trim().is_empty();
    let raw = override_model
        .map(str::trim)
        .filter(nonempty)
        .or_else(|| picked.map(str::trim).filter(nonempty))
        .or_else(|| configured.map(str::trim).filter(nonempty))
        .unwrap_or(default);
    resolve_model(raw)
}

/// The planner-phase prompt. Wraps the user's request in an instruction to
/// produce a concise, ordered plan *without* editing files or running tools —
/// the editor phase does the actual work. Kept short so it doesn't crowd the
/// model's context window.
pub fn plan_instruction(user_message: &str) -> String {
    format!(
        "You are the *planner* in a two-phase architect workflow. Read the \
request and produce a concise, numbered implementation plan: the concrete \
steps, the files to touch, and the changes each needs. Do NOT write code edits \
or run tools — a separate editor agent will execute your plan. Keep it focused \
and actionable.\n\n# Request\n{}",
        user_message.trim()
    )
}

/// Combine the planner's output with the original request for the editor phase.
/// The plan is wrapped in a `<plan>…</plan>` envelope the editor is told to
/// follow; the original request is preserved verbatim so the editor keeps full
/// context (and any `@`-mention attachments already spliced into it).
pub fn inject_plan(plan: &str, user_message: &str) -> String {
    format!(
        "<plan>\n{}\n</plan>\n\nFollow the plan above to implement the request. \
Make the edits and run any tools needed.\n\n{}",
        plan.trim(),
        user_message.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_active_defaults_off() {
        assert!(!is_active(None));
        assert!(!is_active(Some(false)));
        assert!(is_active(Some(true)));
    }

    #[test]
    fn planner_model_override_wins_and_canonicalizes() {
        // A short alias override resolves to its canonical id.
        assert_eq!(planner_model(Some("opus"), Some("gpt-5.5"), None), "claude-opus-4-8");
        // Whitespace/case tolerated.
        assert_eq!(planner_model(Some("  Sonnet "), None, None), "claude-sonnet-4-6");
        // Override beats a configured role default too.
        assert_eq!(
            planner_model(Some("opus"), None, Some("claude-haiku-4-5")),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn planner_model_falls_back_to_pick_then_default() {
        // Empty/whitespace override → use the user's pick.
        assert_eq!(planner_model(Some("  "), Some("gpt-5.5"), None), "gpt-5.5");
        assert_eq!(planner_model(None, Some("gpt-5.5"), None), "gpt-5.5");
        // No override, no pick → the Auto-mode planner default.
        assert_eq!(planner_model(None, None, None), DEFAULT_PLANNER_MODEL);
    }

    #[test]
    fn configured_role_default_sits_below_pick_above_builtin() {
        // No override, no pick → the configured planner role wins over the
        // built-in default (Continue.dev model-roles), canonicalized.
        assert_eq!(
            planner_model(None, None, Some("opus")),
            "claude-opus-4-8"
        );
        // An explicit pick still beats the configured role default.
        assert_eq!(
            planner_model(None, Some("gpt-5.5"), Some("claude-opus-4-8")),
            "gpt-5.5"
        );
        // A blank configured value is ignored → built-in default.
        assert_eq!(planner_model(None, None, Some("  ")), DEFAULT_PLANNER_MODEL);
        // Same precedence for the editor role.
        assert_eq!(editor_model(None, None, Some("haiku")), "claude-haiku-4-5");
        assert_eq!(
            editor_model(None, Some("gpt-5.5"), Some("claude-opus-4-8")),
            "gpt-5.5"
        );
    }

    #[test]
    fn editor_model_falls_back_to_pick_then_default() {
        assert_eq!(editor_model(Some("haiku"), None, None), "claude-haiku-4-5");
        assert_eq!(editor_model(None, Some("gpt-5.5"), None), "gpt-5.5");
        assert_eq!(editor_model(None, None, None), DEFAULT_EDITOR_MODEL);
    }

    #[test]
    fn unknown_slugs_pass_through_unchanged() {
        // Ollama / unknown slugs are not in the catalog → verbatim.
        assert_eq!(planner_model(Some("ollama:llama3.2:1b"), None, None), "ollama:llama3.2:1b");
        assert_eq!(editor_model(None, Some("some-gateway-model"), None), "some-gateway-model");
        // A configured (unknown) slug also passes through verbatim.
        assert_eq!(
            editor_model(None, None, Some("ollama:codellama")),
            "ollama:codellama"
        );
    }

    #[test]
    fn plan_instruction_embeds_request_and_blocks_edits() {
        let p = plan_instruction("  add a /undo command  ");
        assert!(p.contains("add a /undo command"));
        assert!(p.to_lowercase().contains("planner"));
        // It explicitly tells the planner not to edit / run tools.
        assert!(p.contains("Do NOT"));
        // Trimmed: the request body has no leading/trailing pad.
        assert!(!p.contains("  add a /undo command  "));
    }

    #[test]
    fn inject_plan_wraps_and_preserves_both() {
        let out = inject_plan("1. edit foo.rs\n2. test", "refactor foo");
        assert!(out.contains("<plan>"));
        assert!(out.contains("</plan>"));
        assert!(out.contains("1. edit foo.rs"));
        // The original request survives so the editor keeps context.
        assert!(out.contains("refactor foo"));
        // The plan comes before the request.
        assert!(out.find("<plan>").unwrap() < out.find("refactor foo").unwrap());
    }
}
