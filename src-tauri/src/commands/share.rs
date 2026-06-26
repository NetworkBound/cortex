//! `/share` — render the current chat as Markdown and optionally write it
//! to disk under a known-safe directory (the Cortex Brain vault or the
//! currently-active project root). Files outside those roots are rejected so
//! a malicious slash-command can't escape the sandbox.

use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// One serialised chat message. Mirrors a subset of the frontend
/// `Message` shape — we only need the fields that render meaningfully in
/// markdown.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShareMessage {
    pub role: String,
    #[serde(default)]
    pub agent: Option<String>,
    pub content: String,
    /// Unix epoch milliseconds; missing/0 means "no timestamp available".
    #[serde(default, alias = "tsUnixMs")]
    pub ts_unix_ms: Option<i64>,
}

/// Render a chat to markdown. When `target` is supplied it must resolve to a
/// path inside `~/Documents/Cortex Brain` or inside `active_project_root` (if
/// provided). Returns the markdown body either way so the caller can also
/// copy it to the clipboard.
#[tauri::command]
pub async fn share_chat_as_markdown(
    messages: Vec<ShareMessage>,
    target: Option<String>,
    active_project_root: Option<String>,
) -> Result<String, String> {
    // Scrub secrets before the chat leaves the app — exports commonly land in
    // the Cortex Brain vault, which auto-publishes via Quartz. Redacts both the
    // written file and the returned (clipboard) copy.
    let markdown = crate::redact::redact_text(&render(&messages));

    if let Some(raw_target) = target.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let resolved = PathBuf::from(raw_target);
        validate_target(&resolved, active_project_root.as_deref())?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create parent dir failed: {e}"))?;
        }
        fs::write(&resolved, &markdown)
            .map_err(|e| format!("write {} failed: {e}", resolved.display()))?;
    }

    Ok(markdown)
}

fn render(messages: &[ShareMessage]) -> String {
    let mut out = String::new();
    out.push_str("# Cortex chat\n\n");
    let now: DateTime<Local> = Local::now();
    out.push_str(&format!("_Exported {}_\n\n", now.format("%Y-%m-%d %H:%M:%S %Z")));

    if messages.is_empty() {
        out.push_str("_(no messages)_\n");
        return out;
    }

    for m in messages {
        let heading = match m.agent.as_deref() {
            Some(a) if !a.is_empty() => format!("{} ({})", m.role, a),
            _ => m.role.clone(),
        };
        let stamp = m
            .ts_unix_ms
            .filter(|t| *t > 0)
            .and_then(|t| Utc.timestamp_millis_opt(t).single())
            .map(|d| format!(" — _{}_", d.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S")))
            .unwrap_or_default();
        out.push_str(&format!("## {heading}{stamp}\n\n"));
        out.push_str(m.content.trim_end());
        out.push_str("\n\n");
    }
    out
}

/// Ensure `target` lives inside one of the allowed roots and contains no
/// path-traversal components. We first reject `..` and non-absolute paths and
/// check lexical containment, then canonicalise the closest existing ancestor
/// (the file itself may not exist yet) so a symlink inside an allowed root
/// cannot be used to escape the sandbox.
fn validate_target(target: &Path, active_project_root: Option<&str>) -> Result<(), String> {
    if target
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("path traversal (..) is not allowed".into());
    }
    if !target.is_absolute() {
        return Err("target must be an absolute path".into());
    }

    let brain_root = brain_dir().ok_or_else(|| "could not resolve ~/Documents".to_string())?;
    // The active project root must itself be an absolute path; a relative
    // root could never sensibly contain an absolute target and would silently
    // never match, so reject it explicitly rather than treating it as "no
    // project root".
    let project_root = match active_project_root {
        Some(root) => {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err("active project root must be an absolute path".into());
            }
            Some(root)
        }
        None => None,
    };

    let allowed = [Some(brain_root), project_root]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if !allowed.iter().any(|root| path_starts_with(target, root)) {
        return Err(format!(
            "target must live under ~/Documents/Cortex Brain or the active project root (got {})",
            target.display()
        ));
    }

    // Lexical containment is not enough: a symlink *inside* an allowed root
    // could point outside it, letting a write escape the sandbox. Resolve the
    // closest existing ancestor of `target` (the file itself, and possibly
    // some intermediate directories, may not exist yet) and confirm the real,
    // symlink-free location is consistent with an allowed root.
    let mut existing = target;
    let resolved = loop {
        match existing.canonicalize() {
            Ok(p) => break p,
            Err(_) => match existing.parent() {
                Some(parent) => existing = parent,
                // Nothing along the path exists; the lexical check above is the
                // best we can do for a fully-virtual path.
                None => return Ok(()),
            },
        }
    };

    // The resolved ancestor is acceptable when it is *inside* an allowed root
    // (the in-bounds parts exist and contain no escaping symlink) or is itself
    // an *ancestor* of an allowed root (the root's intermediate dirs aren't all
    // created yet, so no symlink can have been planted below it). Roots are
    // canonicalised too so the comparison is symlink-free on both sides.
    let canonical_ok = allowed.iter().any(|root| match root.canonicalize() {
        Ok(canon_root) => {
            path_starts_with(&resolved, &canon_root) || path_starts_with(&canon_root, &resolved)
        }
        // The root itself doesn't fully exist yet, so it can't have been the
        // source of a symlink. The resolved ancestor must therefore sit at or
        // above the (lexical) root for the still-to-be-created path to land
        // inside it; the `..`-free, absolute lexical check above already
        // guarantees the relationship, so accept when resolved is an ancestor.
        Err(_) => path_starts_with(root, &resolved),
    });

    if canonical_ok {
        Ok(())
    } else {
        Err(format!(
            "target resolves outside the allowed roots (got {})",
            resolved.display()
        ))
    }
}

fn path_starts_with(target: &Path, root: &Path) -> bool {
    // Lexical comparison — both paths are absolute (validated above for
    // target; brain/project roots come from trusted sources). We strip
    // trailing separators by re-collecting components.
    let target_components: Vec<_> = target.components().collect();
    let root_components: Vec<_> = root.components().collect();
    if root_components.len() > target_components.len() {
        return false;
    }
    target_components
        .iter()
        .zip(root_components.iter())
        .all(|(a, b)| a == b)
}

fn brain_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Documents").join("Cortex Brain"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_role_and_content() {
        let messages = vec![
            ShareMessage {
                role: "user".into(),
                agent: None,
                content: "Hello there".into(),
                ts_unix_ms: None,
            },
            ShareMessage {
                role: "assistant".into(),
                agent: Some("gateway".into()),
                content: "General Kenobi".into(),
                ts_unix_ms: Some(1_700_000_000_000),
            },
        ];
        let md = render(&messages);
        assert!(md.contains("## user\n"));
        assert!(md.contains("Hello there"));
        assert!(md.contains("## assistant (gateway)"));
        assert!(md.contains("General Kenobi"));
    }

    #[test]
    fn render_handles_empty_list() {
        let md = render(&[]);
        assert!(md.contains("(no messages)"));
    }

    #[test]
    fn validate_rejects_parent_dir() {
        let p = PathBuf::from("/tmp/../etc/passwd");
        assert!(validate_target(&p, None).is_err());
    }

    #[test]
    fn validate_rejects_relative_path() {
        let p = PathBuf::from("notes.md");
        assert!(validate_target(&p, None).is_err());
    }

    #[test]
    fn validate_accepts_project_path() {
        let p = PathBuf::from("/tmp/proj/notes/shared.md");
        let res = validate_target(&p, Some("/tmp/proj"));
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    #[test]
    fn validate_rejects_outside_roots() {
        let p = PathBuf::from("/etc/passwd");
        assert!(validate_target(&p, Some("/tmp/proj")).is_err());
    }
}
