//! `/toc` — Cortex Brain table of contents.
//!
//! Walks every memory source (Claude project memory, runbooks, Obsidian,
//! project / global instructions) and extracts each markdown file's heading
//! hierarchy so the frontend can render a navigable outline. The same source
//! discovery used by `MemoryExplorer` (`default_sources` + `walk_markdown`)
//! powers this, so what shows up in the TOC matches what shows up in the
//! Brain panel exactly.
//!
//! Caps:
//!   - 500 files total (across all sources)
//!   - 50 headings per file
//!
//! The caps exist because user's runbooks vault is ~hundreds of files and
//! the TOC ships as one JSON blob; 500 is plenty for navigation and keeps
//! the payload bounded.

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::memory::sources::{default_sources, walk_markdown, MemorySource, SourceKind};

const MAX_FILES_TOTAL: usize = 500;
const MAX_HEADINGS_PER_FILE: usize = 50;

#[derive(Debug, Clone, Serialize)]
pub struct TocHeading {
    /// `#` count — 1 for `# foo`, 2 for `## foo`, …
    pub level: u8,
    pub text: String,
    /// 1-based line number where the heading sits in the source file.
    /// Lets the frontend ask the editor to scroll to it on click.
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TocFile {
    pub path: PathBuf,
    /// First `# heading` if present, else filename stem.
    pub title: String,
    pub headings: Vec<TocHeading>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TocSource {
    /// Snake-case kind from `SourceKind` (claude_project_memory, runbooks, …).
    pub kind: String,
    pub label: String,
    pub files: Vec<TocFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TocResult {
    pub sources: Vec<TocSource>,
    /// Total files surfaced (post-cap). Useful for the modal subtitle.
    pub file_count: usize,
    pub heading_count: usize,
    /// True when MAX_FILES_TOTAL was hit and trailing sources/files were
    /// dropped so the frontend can show a "truncated" hint.
    pub truncated: bool,
}

/// Map `SourceKind` → the stable snake_case string the frontend uses for
/// grouping. Kept separate from `Serialize` impl so the wire format stays
/// stable even if we tweak the enum later.
fn kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::ClaudeProjectMemory => "claude",
        SourceKind::Runbooks => "runbooks",
        SourceKind::Obsidian => "obsidian",
        SourceKind::ProjectInstructions => "project",
        SourceKind::GlobalInstructions => "global",
    }
}

/// Extract headings from a markdown body. Skips fenced code blocks so a
/// `# header` inside a ```bash block doesn't show up as a TOC entry.
fn extract_headings(body: &str) -> Vec<TocHeading> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (idx, raw) in body.lines().enumerate() {
        let line = raw.trim_end();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !line.starts_with('#') {
            continue;
        }
        let bytes = line.as_bytes();
        let mut level: u8 = 0;
        while (level as usize) < bytes.len() && bytes[level as usize] == b'#' {
            level += 1;
            if level >= 6 {
                break;
            }
        }
        // Require at least one space after the `#`s — otherwise it's `#foo`
        // (anchor / hashtag) not a heading.
        let rest = &line[level as usize..];
        if !rest.starts_with(' ') && !rest.starts_with('\t') {
            continue;
        }
        let text = rest.trim();
        if text.is_empty() {
            continue;
        }
        out.push(TocHeading {
            level,
            text: text.to_string(),
            // `idx` is 0-based; editors expect 1-based line numbers.
            line: (idx as u32) + 1,
        });
        if out.len() >= MAX_HEADINGS_PER_FILE {
            break;
        }
    }
    out
}

/// Title resolution: first `# heading` if present, else the filename stem.
fn title_for(path: &PathBuf, headings: &[TocHeading]) -> String {
    if let Some(h) = headings.iter().find(|h| h.level == 1) {
        return h.text.clone();
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Process a single source into a `TocSource`. Returns `None` when the
/// source has no markdown files left after the global cap.
fn build_source(source: &MemorySource, budget: &mut usize) -> Option<TocSource> {
    if *budget == 0 {
        return None;
    }
    let mut files: Vec<TocFile> = Vec::new();
    for path in walk_markdown(source) {
        if *budget == 0 {
            break;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Strip YAML frontmatter so a `# title:` inside it doesn't show up.
        let body = strip_frontmatter(&raw);
        let headings = extract_headings(body);
        if headings.is_empty() {
            // Skip files with no structure — they'd just clutter the TOC.
            continue;
        }
        let title = title_for(&path, &headings);
        files.push(TocFile { path, title, headings });
        *budget -= 1;
    }
    if files.is_empty() {
        return None;
    }
    Some(TocSource {
        kind: kind_label(source.kind).to_string(),
        label: source.label.clone(),
        files,
    })
}

/// Minimal frontmatter stripper — matches the `memory::markdown` rules but
/// returns just the body slice (no parse). Anything between the opening
/// `---` and matching closing `---` is dropped; if there's no closing
/// fence, we return the whole input unchanged.
fn strip_frontmatter(raw: &str) -> &str {
    if !raw.starts_with("---") {
        return raw;
    }
    // Find the closing `---` on its own line, starting after the first.
    let after_first = &raw[3..];
    let nl = match after_first.find('\n') {
        Some(i) => i + 1,
        None => return raw,
    };
    let rest = &after_first[nl..];
    // Walk by lines looking for `---` exactly.
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            let body_start = 3 + nl + offset + line.len();
            return &raw[body_start..];
        }
        offset += line.len();
    }
    raw
}

#[tauri::command]
pub async fn brain_toc(state: State<'_, AppState>) -> Result<TocResult, String> {
    // Pass the auto-detected/configured Obsidian vault through so the
    // MemoryExplorer actually lists it — previously this was hard-coded to
    // `None`, so a detected vault never appeared in the UI.
    let vault = state.config.read().obsidian_vault.clone();
    let sources = default_sources(None, vault.as_deref());
    let mut out: Vec<TocSource> = Vec::new();
    let mut budget = MAX_FILES_TOTAL;

    for src in &sources {
        if budget == 0 {
            break;
        }
        if let Some(toc_src) = build_source(src, &mut budget) {
            out.push(toc_src);
        }
    }

    let file_count: usize = out.iter().map(|s| s.files.len()).sum();
    let heading_count: usize = out
        .iter()
        .flat_map(|s| s.files.iter())
        .map(|f| f.headings.len())
        .sum();
    // We stopped collecting once `budget` hit zero, i.e. the global file cap
    // was reached; there may be additional files we didn't include.
    let truncated = budget == 0;

    Ok(TocResult {
        sources: out,
        file_count,
        heading_count,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_headings_basic() {
        let md = "# Top\n\nSome text\n\n## Sub\n\n### Deeper\n";
        let h = extract_headings(md);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].level, 1);
        assert_eq!(h[0].text, "Top");
        assert_eq!(h[0].line, 1);
        assert_eq!(h[1].level, 2);
        assert_eq!(h[1].text, "Sub");
        assert_eq!(h[1].line, 5);
        assert_eq!(h[2].level, 3);
        assert_eq!(h[2].text, "Deeper");
    }

    #[test]
    fn skips_fenced_code_blocks() {
        let md = "# Real\n\n```bash\n# inside fence\n```\n\n## Also real\n";
        let h = extract_headings(md);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].text, "Real");
        assert_eq!(h[1].text, "Also real");
    }

    #[test]
    fn caps_at_max_headings() {
        let mut md = String::new();
        for i in 0..(MAX_HEADINGS_PER_FILE + 5) {
            md.push_str(&format!("# h{}\n", i));
        }
        let h = extract_headings(&md);
        assert_eq!(h.len(), MAX_HEADINGS_PER_FILE);
    }

    #[test]
    fn rejects_hash_without_space() {
        let md = "#foo\n# real\n";
        let h = extract_headings(md);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text, "real");
    }

    #[test]
    fn strips_frontmatter_when_present() {
        let raw = "---\ntitle: x\n---\n# Body header\n";
        let body = strip_frontmatter(raw);
        assert!(body.starts_with("# Body header"));
    }

    #[test]
    fn leaves_no_frontmatter_alone() {
        let raw = "# just markdown\n";
        let body = strip_frontmatter(raw);
        assert_eq!(body, raw);
    }
}
