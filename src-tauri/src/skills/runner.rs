//! Skill template runner — Handlebars-flavored `{{var}}` substitution against
//! a skill's body. Deliberately tiny: we don't want to drag in a full template
//! engine just to do straight variable replacement.
//!
//! Rules:
//!   * A `{{name}}` marker is replaced by `vars[name]`.
//!   * Whitespace inside the braces is tolerated (`{{ name }}` works).
//!   * Marker names must match `[A-Za-z_][A-Za-z0-9_]*` — that lets us tell
//!     real markers apart from incidental `{{ ... }}` text in code samples.
//!   * Missing vars are an *error*, not a silent empty string — the caller is
//!     the UI, which already validated required inputs, so an unknown name
//!     here is almost always a typo in the SKILL.md template.

use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use std::collections::HashMap;

use super::loader::load_skill_by_name;

/// `{{ identifier }}` — at least one alphanumeric/underscore, no leading digit.
/// Whitespace inside the braces is allowed and trimmed.
static TEMPLATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}\}").expect("valid regex")
});

/// Substitute every `{{var}}` marker in `body` with the matching `vars` entry.
/// Returns the first missing-var error encountered so the user sees *which*
/// name they're missing rather than getting a half-rendered prompt.
pub fn render(body: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut missing: Option<String> = None;
    let out = TEMPLATE_RE.replace_all(body, |caps: &Captures| {
        let key = &caps[1];
        match vars.get(key) {
            Some(v) => v.clone(),
            None => {
                // First missing wins. Later iterations are no-ops; we just
                // need *some* string to keep the closure happy.
                if missing.is_none() {
                    missing = Some(key.to_string());
                }
                String::new()
            }
        }
    });
    if let Some(name) = missing {
        return Err(format!("missing variable '{name}' for skill template"));
    }
    Ok(out.into_owned())
}

/// Load a skill by name and render its body against `vars`. Returns the
/// expanded prompt that the UI will then drop into chat as a system message.
pub fn expand_skill(
    name: &str,
    vars: HashMap<String, String>,
) -> Result<String, String> {
    let skill = load_skill_by_name(name)
        .ok_or_else(|| format!("skill '{name}' not found"))?;
    render(&skill.body, &vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn renders_basic_template() {
        let body = "Hello {{name}}! You are using {{framework}}.";
        let out = render(body, &vars(&[("name", "user"), ("framework", "vitest")])).unwrap();
        assert_eq!(out, "Hello user! You are using vitest.");
    }

    #[test]
    fn tolerates_inner_whitespace() {
        let body = "x={{ x }} y={{  y  }}";
        let out = render(body, &vars(&[("x", "1"), ("y", "2")])).unwrap();
        assert_eq!(out, "x=1 y=2");
    }

    #[test]
    fn missing_var_is_error() {
        let body = "hi {{who}}";
        let err = render(body, &vars(&[])).unwrap_err();
        assert!(err.contains("who"), "error mentions missing var: {err}");
    }

    #[test]
    fn ignores_non_identifier_braces() {
        // Things like `{{ 1+2 }}` aren't templates — they pass through verbatim.
        let body = "code: {{ 1+2 }}";
        let out = render(body, &vars(&[])).unwrap();
        assert_eq!(out, "code: {{ 1+2 }}");
    }

    #[test]
    fn unused_vars_are_fine() {
        let body = "hi {{a}}";
        let out = render(body, &vars(&[("a", "x"), ("b", "y")])).unwrap();
        assert_eq!(out, "hi x");
    }
}
