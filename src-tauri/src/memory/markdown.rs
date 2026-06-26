use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct MarkdownEntry {
    pub path: PathBuf,
    pub title: Option<String>,
    pub frontmatter: serde_json::Value,
    pub body: String,
    pub wikilinks: Vec<String>,
    pub size_bytes: u64,
    pub modified_unix_ms: i64,
}

pub fn read_entry(path: &Path) -> anyhow::Result<MarkdownEntry> {
    let raw = std::fs::read_to_string(path)?;
    let meta = std::fs::metadata(path)?;
    let modified_unix_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let (frontmatter, body) = split_frontmatter(&raw);
    let wikilinks = extract_wikilinks(&body);

    let title = frontmatter
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            body.lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches('#').trim().to_string())
        });

    Ok(MarkdownEntry {
        path: path.to_path_buf(),
        title,
        frontmatter,
        body,
        wikilinks,
        size_bytes: meta.len(),
        modified_unix_ms,
    })
}

fn split_frontmatter(raw: &str) -> (serde_json::Value, String) {
    if !raw.starts_with("---") {
        return (serde_json::Value::Object(Default::default()), raw.to_string());
    }
    let mut lines = raw.lines();
    lines.next(); // skip opening ---
    let mut fm = String::new();
    let mut found_close = false;
    for line in &mut lines {
        if line == "---" {
            found_close = true;
            break;
        }
        fm.push_str(line);
        fm.push('\n');
    }
    if !found_close {
        return (serde_json::Value::Object(Default::default()), raw.to_string());
    }
    let body: String = lines.collect::<Vec<_>>().join("\n");
    let body = body.trim_start_matches('\n').to_string();
    let parsed: serde_json::Value = serde_yaml::from_str(&fm).unwrap_or_else(|_| serde_json::json!({}));
    (parsed, body)
}

fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = body;
    while let Some(start) = s.find("[[") {
        if let Some(end) = s[start + 2..].find("]]") {
            let inner = &s[start + 2..start + 2 + end];
            if !inner.is_empty() {
                out.push(inner.to_string());
            }
            s = &s[start + 2 + end + 2..];
        } else {
            break;
        }
    }
    out
}
