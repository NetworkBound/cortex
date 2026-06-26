//! OpenHands-style **knowledge microagents** — keyword-triggered context.
//!
//! OpenHands lets a repo ship small markdown "microagents" under
//! `.openhands/microagents/`, each with YAML frontmatter declaring `triggers`
//! (keywords). When the user's message mentions a trigger word, that microagent's
//! body is injected into the model's context as specialized, just-in-time
//! knowledge ("when working with the payments module, always…"). This is distinct
//! from the two context blocks Cortex already has:
//!   * **project rules** ([`super::chat::read_project_rules`]) are *always* loaded
//!     — the project's standing conventions;
//!   * the **ranked repo-map** ([`crate::repo_map`]) is auto-ranked *signatures*.
//!
//! Knowledge microagents are **conditional** — only the ones whose trigger word
//! appears in the current message are injected, so a repo can ship many focused
//! knowledge snippets without every one of them spending the context budget on
//! every turn.
//!
//! This module owns:
//!   * discovery — scan `<root>/.cortex/microagents/` and (for cross-tool compat)
//!     `<root>/.openhands/microagents/` for `*.md`;
//!   * [`parse_microagent`] — pull the `triggers` list out of the frontmatter and
//!     keep the markdown body (pure, unit-tested);
//!   * trigger matching — whole-word, case-insensitive, so `cat` doesn't fire on
//!     `category`;
//!   * [`build_microagents_block`] — emit a `<knowledge>` block of the triggered
//!     microagents, bounded so it can never blow the context budget.
//!
//! Everything but the directory scan is pure and unit-tested on real temp dirs.

use serde::Serialize;
use std::path::Path;

/// The microagent dirs we scan, project-root-relative, in priority order
/// (Cortex's own dir first, then the OpenHands convention for portability).
const MICROAGENT_DIRS: [&str; 2] = [".cortex/microagents", ".openhands/microagents"];

/// Hard cap on the total bytes emitted in the `<knowledge>` block across all
/// triggered microagents — keeps a chatty repo from dominating the prompt.
pub const MAX_BLOCK_BYTES: usize = 16 * 1024;

/// Per-microagent body cap, so one large file can't crowd out the others.
/// Clipped on a UTF-8 char boundary with a trailing marker.
pub const MAX_AGENT_BYTES: usize = 6 * 1024;

/// A parsed knowledge microagent.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MicroAgent {
    /// Display name (frontmatter `name`, else the file stem).
    pub name: String,
    /// Keywords that, when present in the message, inject this microagent.
    pub triggers: Vec<String>,
    /// The markdown body (frontmatter stripped, trimmed).
    pub body: String,
}

/// Split a `---\n…\n---\n<body>` document into (frontmatter, body). When there's
/// no leading frontmatter fence, returns `("", whole)`.
fn split_frontmatter(content: &str) -> (&str, &str) {
    let trimmed = content.trim_start_matches('\u{feff}');
    let rest = match trimmed.strip_prefix("---\n").or_else(|| trimmed.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return ("", trimmed),
    };
    // Find the closing fence at the start of a line.
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n"] {
        if let Some(idx) = rest.find(marker) {
            let fm = &rest[..idx];
            let body = &rest[idx + marker.len()..];
            return (fm, body);
        }
    }
    // A doc that ends exactly at the closing fence (no trailing body).
    for suffix in ["\n---", "\r\n---"] {
        if let Some(fm) = rest.strip_suffix(suffix) {
            return (fm, "");
        }
    }
    ("", trimmed)
}

/// Parse the `triggers:` (or singular `trigger:`) list out of YAML-ish
/// frontmatter. Supports both an inline flow list (`triggers: [a, b]`) and a
/// block list (`triggers:` then `  - a` lines). Tolerant by design — this is a
/// best-effort reader, not a full YAML parser.
fn parse_triggers(frontmatter: &str) -> Vec<String> {
    let mut triggers = Vec::new();
    let mut lines = frontmatter.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim();
        let key = line
            .strip_prefix("triggers:")
            .or_else(|| line.strip_prefix("trigger:"));
        let Some(after) = key else { continue };
        let after = after.trim();
        if let Some(inline) = after.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            // Inline flow list: triggers: [foo, "bar baz"]
            for item in inline.split(',') {
                let v = clean_scalar(item);
                if !v.is_empty() {
                    triggers.push(v);
                }
            }
        } else if !after.is_empty() {
            // Single scalar on the same line: triggers: foo
            let v = clean_scalar(after);
            if !v.is_empty() {
                triggers.push(v);
            }
        }
        // Block list: subsequent `- item` lines (indented or not).
        while let Some(next) = lines.peek() {
            let t = next.trim();
            if let Some(item) = t.strip_prefix('-') {
                let v = clean_scalar(item);
                if !v.is_empty() {
                    triggers.push(v);
                }
                lines.next();
            } else if t.is_empty() {
                lines.next();
            } else {
                break;
            }
        }
        break;
    }
    triggers
}

/// Strip surrounding quotes/whitespace from a YAML scalar and lowercase it
/// (triggers match case-insensitively).
fn clean_scalar(s: &str) -> String {
    let t = s.trim();
    let t = t
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| t.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
        .unwrap_or(t);
    t.trim().to_lowercase()
}

/// Read the `name:` scalar from frontmatter, if present.
fn parse_name(frontmatter: &str) -> Option<String> {
    for raw in frontmatter.lines() {
        let line = raw.trim();
        if let Some(after) = line.strip_prefix("name:") {
            let v = after
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Parse a microagent file's contents. `stem` is the filename without extension,
/// used as the fallback name. Returns `None` when the body is empty or there are
/// no triggers (a triggerless file is an always-on "repo" microagent, which the
/// always-loaded project-rules block already covers — we only handle the
/// *conditional* knowledge kind here).
pub fn parse_microagent(stem: &str, content: &str) -> Option<MicroAgent> {
    let (frontmatter, body) = split_frontmatter(content);
    let triggers = parse_triggers(frontmatter);
    if triggers.is_empty() {
        return None;
    }
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let name = parse_name(frontmatter).unwrap_or_else(|| stem.to_string());
    Some(MicroAgent {
        name,
        triggers,
        body: body.to_string(),
    })
}

/// Whole-word, case-insensitive containment: does `needle` appear in
/// `haystack_lower` bounded by non-alphanumeric chars (so `cat` matches
/// `the cat sat` and `cat.` but not `category`)? Multi-word triggers are matched
/// literally (boundaries on the ends). `needle` must already be lowercase.
fn contains_word(haystack_lower: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hb = haystack_lower.as_bytes();
    let nb = needle.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack_lower[start..].find(needle) {
        let i = start + pos;
        let before_ok = i == 0
            || !haystack_lower[..i]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false);
        let after_idx = i + nb.len();
        let after_ok = after_idx >= hb.len()
            || !haystack_lower[after_idx..]
                .chars()
                .next()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = i + 1;
        if start >= haystack_lower.len() {
            break;
        }
    }
    false
}

/// Load every knowledge microagent under the project's microagent dirs.
/// Files are read in a deterministic order (by dir priority, then filename) so
/// the injected block is byte-stable for the same inputs.
pub fn load_microagents(root: &Path) -> Vec<MicroAgent> {
    let mut out = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    for dir in MICROAGENT_DIRS {
        let path = root.join(dir);
        let Ok(rd) = std::fs::read_dir(&path) else {
            continue;
        };
        let mut entries: Vec<_> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            })
            .collect();
        entries.sort();
        for file in entries {
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("microagent");
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            if let Some(agent) = parse_microagent(stem, &content) {
                // A name collision (e.g. same file in both dirs) keeps the
                // higher-priority one already inserted.
                if seen_names.insert(agent.name.to_lowercase()) {
                    out.push(agent);
                }
            }
        }
    }
    out
}

/// Clip `body` to `MAX_AGENT_BYTES` on a char boundary, appending a marker when
/// truncated.
fn clip_body(body: &str) -> String {
    if body.len() <= MAX_AGENT_BYTES {
        return body.to_string();
    }
    let mut end = MAX_AGENT_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated)", &body[..end])
}

/// Build the `<knowledge>` context block from the microagents whose triggers
/// appear in `task`. Returns `None` when nothing is triggered (so the caller
/// leaves the message untouched). Bounded at [`MAX_BLOCK_BYTES`]; once full,
/// remaining triggered agents are noted but not embedded.
pub fn build_microagents_block(root: &Path, task: &str) -> Option<String> {
    let agents = load_microagents(root);
    if agents.is_empty() {
        return None;
    }
    let task_lower = task.to_lowercase();
    let mut body = String::new();
    let mut used = 0usize;
    let mut omitted = 0usize;
    for agent in &agents {
        let Some(hit) = agent
            .triggers
            .iter()
            .find(|t| contains_word(&task_lower, t))
        else {
            continue;
        };
        let clipped = clip_body(&agent.body);
        let section = format!("## {} (triggered by \"{hit}\")\n{clipped}\n\n", agent.name);
        if used + section.len() > MAX_BLOCK_BYTES && used > 0 {
            omitted += 1;
            continue;
        }
        body.push_str(&section);
        used += section.len();
    }
    if body.is_empty() {
        return None;
    }
    if omitted > 0 {
        body.push_str(&format!(
            "(+{omitted} more triggered microagent(s) omitted — context budget reached)\n\n"
        ));
    }
    Some(format!(
        "<knowledge>\nRepo-specific knowledge triggered by your message. Treat as authoritative guidance for this task.\n{body}</knowledge>\n\n"
    ))
}

/// Frontend-facing summary of a microagent (no body — just what's defined).
#[derive(Debug, Clone, Serialize)]
pub struct MicroAgentInfo {
    pub name: String,
    pub triggers: Vec<String>,
    pub bytes: usize,
}

/// List the knowledge microagents defined in the active project (for a `/microagents`
/// listing — transparency into what conditional knowledge the repo ships).
#[tauri::command]
pub fn list_microagents(project_root: String) -> Result<Vec<MicroAgentInfo>, String> {
    let root = Path::new(&project_root);
    if !root.is_dir() {
        return Err("project root is not a directory".into());
    }
    Ok(load_microagents(root)
        .into_iter()
        .map(|a| MicroAgentInfo {
            name: a.name,
            triggers: a.triggers,
            bytes: a.body.len(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_extracts_block_list_triggers_and_body() {
        let doc = "---\nname: Payments\ntriggers:\n  - payment\n  - stripe\n---\nAlways use the idempotency key.";
        let a = parse_microagent("file", doc).unwrap();
        assert_eq!(a.name, "Payments");
        assert_eq!(a.triggers, vec!["payment", "stripe"]);
        assert_eq!(a.body, "Always use the idempotency key.");
    }

    #[test]
    fn parse_extracts_inline_flow_list_and_falls_back_to_stem_name() {
        let doc = "---\ntriggers: [Deploy, \"k8s rollout\"]\n---\nRun the canary first.";
        let a = parse_microagent("deploy", doc).unwrap();
        assert_eq!(a.name, "deploy"); // no name: in frontmatter → file stem
        assert_eq!(a.triggers, vec!["deploy", "k8s rollout"]); // lowercased, quotes stripped
        assert_eq!(a.body, "Run the canary first.");
    }

    #[test]
    fn parse_rejects_triggerless_and_empty_body() {
        // No triggers → not a knowledge microagent (always-on is project-rules' job).
        assert!(parse_microagent("x", "---\nname: x\n---\nbody").is_none());
        // Triggers but empty body → nothing to inject.
        assert!(parse_microagent("x", "---\ntriggers:\n  - foo\n---\n   ").is_none());
        // No frontmatter at all → no triggers → none.
        assert!(parse_microagent("x", "just some text").is_none());
    }

    #[test]
    fn contains_word_is_whole_word_and_case_insensitive() {
        assert!(contains_word("the cat sat", "cat"));
        assert!(contains_word("a cat.", "cat")); // punctuation boundary
        assert!(!contains_word("the category", "cat")); // not a substring match
        assert!(contains_word("deploy to k8s rollout now", "k8s rollout")); // multi-word
        // caller lowercases the haystack; needle is pre-lowercased on parse
        assert!(contains_word("use stripe here", "stripe"));
        assert!(!contains_word("", "cat"));
    }

    #[test]
    fn load_and_build_block_injects_only_triggered_agents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ma = root.join(".cortex").join("microagents");
        fs::create_dir_all(&ma).unwrap();
        fs::write(
            ma.join("payments.md"),
            "---\nname: Payments\ntriggers:\n  - payment\n---\nUse the idempotency key for payments.",
        )
        .unwrap();
        fs::write(
            ma.join("deploy.md"),
            "---\ntriggers:\n  - deploy\n---\nRun the canary first.",
        )
        .unwrap();

        // Loads both.
        let agents = load_microagents(root);
        assert_eq!(agents.len(), 2);

        // A message mentioning only "payment" injects only that agent.
        let block = build_microagents_block(root, "how do I handle a payment refund?").unwrap();
        assert!(block.contains("<knowledge>"));
        assert!(block.contains("## Payments (triggered by \"payment\")"));
        assert!(block.contains("idempotency key"));
        assert!(!block.contains("canary"), "deploy agent should not be triggered: {block}");

        // A message with no trigger word → no block.
        assert!(build_microagents_block(root, "what is the weather?").is_none());

        // A message hitting both triggers injects both.
        let both = build_microagents_block(root, "deploy the payment service").unwrap();
        assert!(both.contains("idempotency key") && both.contains("canary"));
    }

    #[test]
    fn build_block_returns_none_with_no_microagents_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_microagents_block(dir.path(), "deploy payment").is_none());
        assert!(load_microagents(dir.path()).is_empty());
    }

    #[test]
    fn clip_body_truncates_on_boundary_with_marker() {
        let big = "x".repeat(MAX_AGENT_BYTES + 500);
        let clipped = clip_body(&big);
        assert!(clipped.ends_with("… (truncated)"));
        assert!(clipped.len() <= MAX_AGENT_BYTES + "\n… (truncated)".len());
        // Small body is returned verbatim.
        assert_eq!(clip_body("short"), "short");
    }

    #[test]
    fn openhands_compat_dir_is_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ma = root.join(".openhands").join("microagents");
        fs::create_dir_all(&ma).unwrap();
        fs::write(
            ma.join("kube.md"),
            "---\ntriggers:\n  - kubernetes\n---\nAlways set resource limits.",
        )
        .unwrap();
        let block = build_microagents_block(root, "deploying to kubernetes").unwrap();
        assert!(block.contains("resource limits"));
    }

    #[test]
    fn list_microagents_reports_defs_without_body() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ma = root.join(".cortex").join("microagents");
        fs::create_dir_all(&ma).unwrap();
        fs::write(
            ma.join("p.md"),
            "---\nname: Payments\ntriggers:\n  - payment\n  - stripe\n---\nBody here.",
        )
        .unwrap();
        let infos = list_microagents(root.to_string_lossy().to_string()).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "Payments");
        assert_eq!(infos[0].triggers, vec!["payment", "stripe"]);
        assert_eq!(infos[0].bytes, "Body here.".len());
        // Non-dir root errors.
        assert!(list_microagents("/no/such/dir/xyz".into()).is_err());
    }
}
