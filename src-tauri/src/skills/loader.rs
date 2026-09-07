//! Skill loader — walks `~/.cortex/skills/<name>/SKILL.md` and parses YAML
//! frontmatter + Markdown body into [`Skill`] records.
//!
//! Design notes
//! ------------
//! * We use `yaml-front-matter` (already a dep) to split frontmatter from body
//!   so we don't reinvent the `---\n…\n---` split.
//! * `inputs` is intentionally permissive: each entry is either a bare string
//!   (`- testFramework`) or `- testFramework: jest|vitest|cargo` — we parse
//!   the `pipe-separated` option list on the right-hand side so the UI can
//!   render a `<select>` instead of a freeform field when options exist.
//! * Bad files (missing frontmatter, malformed YAML, missing `name`) are
//!   silently skipped. Returning a `Result` per-file would make the panel
//!   noisy for users who hand-edit skills.
//! * Skill names are slug-validated (`[A-Za-z0-9_.-]+`) so a malicious
//!   frontmatter can't smuggle path traversal into a later `load_skill_by_name`
//!   lookup.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use yaml_front_matter::YamlFrontMatter;

/// Outbound shape for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<SkillInput>,
    pub body: String,
}

/// A single declared input variable. `options` is empty for freeform text
/// inputs; non-empty for `<select>` style enumerated choices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInput {
    pub name: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// Raw shape of the YAML frontmatter. Inputs come in as either bare strings or
/// `{ name: options }` maps; we normalize both into [`SkillInput`].
#[derive(Debug, Deserialize)]
struct RawFrontmatter {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    inputs: Vec<serde_yaml::Value>,
}

/// Root directory holding `<skill>/SKILL.md` subfolders. Public so tests can
/// override via env once we wire that up; for now it's just convenience.
pub fn skills_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cortex").join("skills"))
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        // Reject the pure-dot segments `.` and `..` which would otherwise pass
        // the char allowlist (`.` is permitted) and let a caller resolve into a
        // parent/self directory in `load_skill_by_name`.
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Parse a single `inputs:` entry into a [`SkillInput`].
///
/// Accepts both forms:
///   - `- testFramework` (bare string ⇒ freeform text input)
///   - `- testFramework: jest|vitest|cargo` (map ⇒ enumerated select)
fn parse_input(raw: &serde_yaml::Value) -> Option<SkillInput> {
    match raw {
        serde_yaml::Value::String(s) => {
            let name = s.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(SkillInput {
                name,
                options: Vec::new(),
            })
        }
        serde_yaml::Value::Mapping(map) => {
            // We expect exactly one key/value pair here. Anything else (an
            // empty map, multiple keys) is malformed — skip it.
            let mut iter = map.iter();
            let (k, v) = iter.next()?;
            if iter.next().is_some() {
                return None;
            }
            let name = k.as_str()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let opts = v.as_str().unwrap_or_default();
            let options: Vec<String> = opts
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            Some(SkillInput { name, options })
        }
        _ => None,
    }
}

fn parse_file(path: &std::path::Path) -> Option<Skill> {
    let text = fs::read_to_string(path).ok()?;
    let doc = YamlFrontMatter::parse::<RawFrontmatter>(&text).ok()?;
    let fm = doc.metadata;
    if !is_valid_name(&fm.name) {
        return None;
    }
    let inputs = fm
        .inputs
        .iter()
        .filter_map(parse_input)
        .collect::<Vec<_>>();
    Some(Skill {
        name: fm.name,
        description: fm.description,
        inputs,
        body: doc.content,
    })
}

/// Load every parseable skill under `~/.cortex/skills/<name>/SKILL.md`. Bad
/// files are skipped, not surfaced as errors — the UI stays usable.
pub fn load_skills() -> Vec<Skill> {
    let Some(root) = skills_root() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<Skill> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Use the entry's own file type (does NOT follow symlinks) so a
        // symlinked directory under skills/ can't pull a SKILL.md from an
        // arbitrary location on disk into the panel.
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {}
            _ => continue,
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        if let Some(skill) = parse_file(&skill_md) {
            out.push(skill);
        }
    }
    // Stable alpha sort so the panel doesn't shuffle on every reload.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Load a single skill by name. `None` if the directory or file is missing /
/// unparseable. Validates the name first so we never resolve `..` into a
/// sibling directory.
pub fn load_skill_by_name(name: &str) -> Option<Skill> {
    if !is_valid_name(name) {
        return None;
    }
    let root = skills_root()?;
    let skill_md = root.join(name).join("SKILL.md");
    if !skill_md.is_file() {
        return None;
    }
    parse_file(&skill_md)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(is_valid_name("write-test"));
        assert!(is_valid_name("a.b_c-1"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name(".."));
        assert!(!is_valid_name("."));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }

    #[test]
    fn parses_bare_and_typed_inputs() {
        let bare = serde_yaml::Value::String("foo".to_string());
        let parsed = parse_input(&bare).unwrap();
        assert_eq!(parsed.name, "foo");
        assert!(parsed.options.is_empty());

        let typed: serde_yaml::Value =
            serde_yaml::from_str("framework: jest|vitest|cargo").unwrap();
        let parsed = parse_input(&typed).unwrap();
        assert_eq!(parsed.name, "framework");
        assert_eq!(parsed.options, vec!["jest", "vitest", "cargo"]);
    }

    #[test]
    fn parses_full_skill_file() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("write-test");
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        fs::write(
            &path,
            "---\nname: write-test\ndescription: Generate a unit test\ninputs:\n  - testFramework: jest|vitest|cargo\n  - functionName\n---\nWrite a {{testFramework}} test for {{functionName}}.\n",
        )
        .unwrap();
        let skill = parse_file(&path).expect("parses");
        assert_eq!(skill.name, "write-test");
        assert_eq!(skill.description, "Generate a unit test");
        assert_eq!(skill.inputs.len(), 2);
        assert_eq!(skill.inputs[0].name, "testFramework");
        assert_eq!(skill.inputs[0].options, vec!["jest", "vitest", "cargo"]);
        assert_eq!(skill.inputs[1].name, "functionName");
        assert!(skill.inputs[1].options.is_empty());
        assert!(skill.body.contains("{{functionName}}"));
    }

    #[test]
    fn bad_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.md");
        fs::write(&bad, "no frontmatter here").unwrap();
        assert!(parse_file(&bad).is_none());
    }
}
