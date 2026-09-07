//! Cursor-style `.cortex/rules/*.md` activation taxonomy. Each rule file may
//! carry a YAML frontmatter fence (`--- ... ---`) with an `activation` field:
//! `alwaysApply` (default), `globs`, `description`, or `manual`. Rules with
//! no frontmatter keep the legacy "always loaded" behaviour.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// How a rule decides whether to apply to the current turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Activation {
    AlwaysApply,
    Globs,
    Description,
    Manual,
}

/// One parsed rule from `.cortex/rules/`. Body excludes the frontmatter.
#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub name: String,
    pub path: PathBuf,
    pub activation: Activation,
    #[serde(default)]
    pub globs: Vec<String>,
    pub description: Option<String>,
    pub body: String,
}

/// Lightweight summary surfaced to the UI (no body payload).
#[derive(Debug, Clone, Serialize)]
pub struct RuleSummary {
    pub name: String,
    pub activation: Activation,
    pub globs: Vec<String>,
    pub description: Option<String>,
}

impl Rule {
    pub fn summary(&self) -> RuleSummary {
        RuleSummary {
            name: self.name.clone(),
            activation: self.activation.clone(),
            globs: self.globs.clone(),
            description: self.description.clone(),
        }
    }
}

/// Splits a file's contents into `(frontmatter_yaml, body)`. Returns
/// `(None, full_content)` if no leading `---` fence is present.
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let rest = match content.strip_prefix("---\n").or_else(|| content.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return (None, content),
    };
    // Find the closing fence — the `---` must be the entire line (modulo
    // trailing whitespace). A body line that merely *begins* with `---`
    // (e.g. `---foo`, `----`, or `--- still text`) must not be mistaken for
    // the fence, otherwise the frontmatter/body split truncates.
    for (idx, marker) in rest.match_indices("\n---") {
        let after = &rest[idx + marker.len()..];
        // Everything after the three dashes up to the next newline (or EOF)
        // must be blank for this to be a real closing fence.
        let line_tail = after.split('\n').next().unwrap_or(after);
        if !line_tail.trim().is_empty() {
            continue;
        }
        let fm = &rest[..idx];
        // Skip past the fence line, including its terminating newline.
        let body = match after.split_once('\n') {
            Some((_, b)) => b,
            None => "",
        };
        return (Some(fm), body);
    }
    (None, content)
}

/// Manual YAML mini-parser. Handles only the shapes Cortex rule frontmatter
/// uses: scalar `key: value` and list-of-strings `key:\n  - item`. Quoted
/// strings have the surrounding quotes stripped. Unknown keys are ignored.
fn parse_frontmatter(fm: &str) -> (Option<String>, Vec<String>, Option<String>) {
    let mut activation: Option<String> = None;
    let mut globs: Vec<String> = Vec::new();
    let mut description: Option<String> = None;
    let mut current_list: Option<&'static str> = None;

    for raw in fm.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // List item under the active key.
        if let Some(rest) = line.trim_start().strip_prefix("- ") {
            if current_list == Some("globs") {
                globs.push(unquote(rest.trim()).to_string());
            }
            continue;
        }
        // `key: value` or `key:` (list header).
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim();
        let value = value.trim();
        match key {
            "activation" if !value.is_empty() => { activation = Some(unquote(value).to_string()); current_list = None; }
            "description" if !value.is_empty() => { description = Some(unquote(value).to_string()); current_list = None; }
            "globs" if value.is_empty() => { current_list = Some("globs"); }
            "globs" => { globs.push(unquote(value).to_string()); current_list = None; }
            _ => { current_list = None; }
        }
    }
    (activation, globs, description)
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse one rule file. Missing frontmatter or missing `activation` field
/// defaults to `alwaysApply` so legacy rules keep working unchanged.
pub fn parse_rule(path: &Path, content: &str) -> Rule {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rule")
        .to_string();
    let (fm_opt, body) = split_frontmatter(content);
    let (act_str, globs, description) = fm_opt.map(parse_frontmatter).unwrap_or((None, Vec::new(), None));
    // Unknown values fall back to alwaysApply so a typo doesn't silently
    // drop a rule from the context.
    let activation = match act_str.as_deref() {
        Some("globs") => Activation::Globs,
        Some("description") => Activation::Description,
        Some("manual") => Activation::Manual,
        _ => Activation::AlwaysApply,
    };
    Rule {
        name,
        path: path.to_path_buf(),
        activation,
        globs,
        description,
        body: body.trim_start_matches('\n').to_string(),
    }
}

/// Walk `<project>/.cortex/rules/*.md` (depth 1) and return parsed rules
/// sorted by file name. Each body is capped at 4000 chars for parity with
/// the prior loader behaviour.
pub fn load_rules(project: &Path) -> Vec<Rule> {
    let dir = project.join(".cortex").join("rules");
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<Rule> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else { continue };
            if !fname.ends_with(".md") && !fname.ends_with(".mdc") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                let mut rule = parse_rule(&path, &content);
                if rule.body.chars().count() > 4000 {
                    rule.body = rule.body.chars().take(4000).collect();
                }
                out.push(rule);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Decide which rules apply to the given user message.
///
/// Heuristics:
/// - `alwaysApply`: always included.
/// - `globs`: included if the message contains `@file:<path>` where `<path>`
///   matches any rule glob.
/// - `description`: included if any whitespace-separated keyword from the
///   description appears in the message (case-insensitive substring, words
///   ≥3 chars only to avoid noise).
/// - `manual`: included only if the message contains `@rule:<name>`.
pub fn select_active<'a>(rules: &'a [Rule], user_message: &str) -> Vec<&'a Rule> {
    let lower = user_message.to_lowercase();
    let referenced_files = extract_file_refs(user_message);
    let manual_refs = extract_manual_refs(user_message);

    rules
        .iter()
        .filter(|r| match r.activation {
            Activation::AlwaysApply => true,
            Activation::Globs => referenced_files.iter().any(|f| any_glob_matches(&r.globs, f)),
            Activation::Description => r
                .description
                .as_deref()
                .map(|d| description_hits(d, &lower))
                .unwrap_or(false),
            Activation::Manual => manual_refs.iter().any(|n| n.eq_ignore_ascii_case(&r.name)),
        })
        .collect()
}

fn extract_file_refs(msg: &str) -> Vec<String> {
    let mut out = Vec::new();
    // `@file:<path>` — path runs until whitespace or end. We deliberately
    // accept anything non-whitespace so Windows backslash paths work too.
    for (idx, _) in msg.match_indices("@file:") {
        let rest = &msg[idx + "@file:".len()..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        if end > 0 {
            out.push(rest[..end].to_string());
        }
    }
    out
}

fn extract_manual_refs(msg: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (idx, _) in msg.match_indices("@rule:") {
        let rest = &msg[idx + "@rule:".len()..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',')
            .unwrap_or(rest.len());
        if end > 0 {
            out.push(rest[..end].to_string());
        }
    }
    out
}

fn any_glob_matches(patterns: &[String], path: &str) -> bool {
    let normalized = normalize_ref(path);
    patterns
        .iter()
        .filter_map(|p| {
            // `case_insensitive` so `Src/App.TS` still matches `src/**/*.ts`.
            globset::GlobBuilder::new(&normalize_ref(p))
                .case_insensitive(true)
                .build()
                .ok()
        })
        .any(|g| g.compile_matcher().is_match(&normalized))
}

/// Normalize a file reference or glob pattern for matching: convert Windows
/// backslashes to forward slashes and drop a leading `./` or `/` so that
/// `@file:/src/app.ts`, `@file:./src/app.ts`, and `@file:src/app.ts` all
/// compare equal to a `src/**/*.ts` glob.
fn normalize_ref(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s);
    s.strip_prefix('/').unwrap_or(s).to_string()
}

/// Common English stop words that carry no activation signal. A
/// description-gated rule should not fire merely because a message contains
/// `the` or `when`, so these are excluded from the keyword match.
const DESCRIPTION_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "when", "you", "your", "this", "that", "are",
    "was", "from", "have", "has", "into", "onto", "but", "not", "all", "any",
    "use", "using", "via", "per", "out", "off", "its", "our", "their", "them",
    "then", "than", "some", "such", "only", "also", "etc",
];

fn description_hits(description: &str, lower_msg: &str) -> bool {
    description
        .split(|c: char| !c.is_alphanumeric())
        // Require ≥4 chars and exclude common stop words so a description like
        // "when working on the auth flow" gates on "working"/"auth", not on
        // ubiquitous filler words that appear in almost every message.
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.to_lowercase())
        .filter(|w| !DESCRIPTION_STOPWORDS.contains(&w.as_str()))
        .any(|w| lower_msg.contains(&w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rf(name: &str, content: &str) -> Rule {
        parse_rule(&PathBuf::from(format!("/tmp/{name}.md")), content)
    }

    #[test]
    fn no_frontmatter_defaults_to_always_apply() {
        let r = rf("legacy", "# Legacy rule\n\nbody");
        assert_eq!(r.activation, Activation::AlwaysApply);
        assert!(r.body.contains("Legacy rule"));
    }

    #[test]
    fn parses_all_activation_variants() {
        let globs = rf("g", "---\nactivation: globs\nglobs:\n  - \"**/*.ts\"\n  - 'src/**/*.tsx'\n---\nbody");
        assert_eq!(globs.activation, Activation::Globs);
        assert_eq!(globs.globs, vec!["**/*.ts".to_string(), "src/**/*.tsx".to_string()]);

        let desc = rf("d", "---\nactivation: description\ndescription: when working on auth\n---\nbody");
        assert_eq!(desc.activation, Activation::Description);
        assert_eq!(desc.description.as_deref(), Some("when working on auth"));

        let manual = rf("m", "---\nactivation: manual\n---\nbody");
        assert_eq!(manual.activation, Activation::Manual);

        let unknown = rf("u", "---\nactivation: yolo\n---\nbody");
        assert_eq!(unknown.activation, Activation::AlwaysApply);
    }

    #[test]
    fn select_active_respects_all_modes() {
        let rules = vec![
            rf("always", "body"),
            rf("ts", "---\nactivation: globs\nglobs:\n  - \"src/**/*.ts\"\n---\nbody"),
            rf("auth", "---\nactivation: description\ndescription: when working on auth flows\n---\nbody"),
            rf("danger", "---\nactivation: manual\n---\nbody"),
        ];
        let names = |active: Vec<&Rule>| -> Vec<String> { active.iter().map(|r| r.name.clone()).collect() };

        assert_eq!(names(select_active(&rules, "")), vec!["always"]);
        assert_eq!(names(select_active(&rules, "see @file:src/app/main.ts")), vec!["always", "ts"]);
        assert_eq!(names(select_active(&rules, "fix the AUTH bug")), vec!["always", "auth"]);
        assert_eq!(names(select_active(&rules, "use @rule:danger here")), vec!["always", "danger"]);
    }
}
