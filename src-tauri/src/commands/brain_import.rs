//! `/brain-save` — write a chat (or any markdown body) into the local
//! Cortex Brain vault under `~/Documents/Cortex Brain/imports/<date>-<slug>.md`.
//!
//! Path-traversal safe: the slug is sanitised and the output is anchored to
//! the brain vault root. YAML frontmatter is prepended so downstream Obsidian
//! / memory walkers can identify the kind + ingest time without parsing the
//! filename.

use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Local, Utc};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub written_path: PathBuf,
    pub bytes: usize,
}

/// Write `content` to the Cortex Brain imports directory.
///
/// * `content` — the markdown body (frontmatter is prepended automatically).
/// * `label`   — used to build the filename slug. Empty strings fall back to
///               `"import"`.
/// * `kind`    — short identifier stored in the YAML frontmatter (e.g. `chat`,
///               `note`).
#[tauri::command]
pub async fn import_to_brain(
    content: String,
    label: String,
    kind: String,
) -> Result<ImportResult, String> {
    let brain_root = brain_dir().ok_or_else(|| "could not resolve ~/Documents".to_string())?;
    let imports_dir = brain_root.join("imports");
    fs::create_dir_all(&imports_dir)
        .map_err(|e| format!("create imports dir failed: {e}"))?;

    let now_local = Local::now();
    // Include a high-resolution time component so repeated imports on the same
    // day (with the same label) don't silently overwrite earlier files.
    let date = now_local.format("%Y-%m-%d").to_string();
    let time = now_local.format("%H%M%S%3f").to_string();
    let slug = slugify(&label);
    let filename = format!("{date}-{time}-{slug}.md");
    let written_path = imports_dir.join(&filename);

    let now_iso: DateTime<Utc> = Utc::now();
    let frontmatter = format!(
        "---\nimported_at: {}\nkind: {}\n---\n\n",
        now_iso.to_rfc3339(),
        yaml_escape(&kind),
    );

    let body = format!("{frontmatter}{}", content);
    let bytes = body.as_bytes().len();
    fs::write(&written_path, &body)
        .map_err(|e| format!("write {} failed: {e}", written_path.display()))?;

    Ok(ImportResult {
        written_path,
        bytes,
    })
}

fn brain_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Documents").join("Cortex Brain"))
}

/// Lowercase, ASCII-alnum-and-dash slug capped at 60 chars. Falls back to
/// `"import"` for empty / all-symbol inputs.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 60 {
        out.truncate(60);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "import".into()
    } else {
        out
    }
}

/// Minimal YAML scalar escaping. Strips newlines and trims surrounding
/// whitespace; if the value contains structural YAML characters we quote it.
fn yaml_escape(input: &str) -> String {
    let cleaned = input.replace(['\n', '\r'], " ").trim().to_string();
    if cleaned.is_empty() {
        return "unknown".into();
    }
    if cleaned.chars().any(|c| matches!(c, ':' | '#' | '"' | '\'' | '{' | '}' | '[' | ']' | ',' | '&' | '*' | '!' | '|' | '>' | '%' | '@' | '`')) {
        format!("\"{}\"", cleaned.replace('"', "\\\""))
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("  multiple   spaces  "), "multiple-spaces");
        assert_eq!(slugify("symbols!!! @@@ ok"), "symbols-ok");
    }

    #[test]
    fn slug_falls_back_when_empty() {
        assert_eq!(slugify(""), "import");
        assert_eq!(slugify("!!!"), "import");
    }

    #[test]
    fn slug_truncates() {
        let long = "a".repeat(200);
        let s = slugify(&long);
        assert!(s.len() <= 60);
    }

    #[test]
    fn yaml_escape_plain() {
        assert_eq!(yaml_escape("chat"), "chat");
        assert_eq!(yaml_escape("  with-spaces  "), "with-spaces");
    }

    #[test]
    fn yaml_escape_quotes_special() {
        let s = yaml_escape("foo: bar");
        assert!(s.starts_with('"') && s.ends_with('"'));
    }

    #[test]
    fn yaml_escape_empty() {
        assert_eq!(yaml_escape(""), "unknown");
        assert_eq!(yaml_escape("   "), "unknown");
    }
}
