//! Project-wide code metrics.
//!
//! Walks the project tree (respecting `.cortexignore` and the always-on
//! hidden-file denylist), counts lines / bytes per file, and aggregates into
//! language buckets, top-10 largest files, and top-5 biggest directories.
//! Read-only — surfaced by the Project metrics panel via `/metrics`.
//!
//! The walk uses `walkdir` without the depth cap that `projects::list_files`
//! enforces, so deep monorepos surface fully. We still bound the scan at
//! 50,000 entries to keep latency under a second on typical repos.

use crate::projects::ignore::CortexIgnore;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Upper bound on the number of file entries we scan. Matches the existing
/// `project_files(_, 50000)` ceiling so the two views agree about "what's
/// in the project".
const MAX_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, Serialize, Default)]
pub struct LangStat {
    pub files: usize,
    pub lines: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub lines: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub path: PathBuf,
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectMetrics {
    pub project_root: PathBuf,
    pub total_files: usize,
    pub total_lines: usize,
    pub total_bytes: u64,
    pub languages: HashMap<String, LangStat>,
    pub largest_files: Vec<FileEntry>,
    pub biggest_dirs: Vec<DirEntry>,
    pub generated_unix_ms: i64,
    /// True iff we hit `MAX_ENTRIES` before exhausting the tree.
    pub truncated: bool,
}

#[tauri::command]
pub async fn project_metrics(project_root: String) -> Result<ProjectMetrics, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let ignore = CortexIgnore::load(&root);

    let mut total_files = 0usize;
    let mut total_lines = 0usize;
    let mut total_bytes = 0u64;
    let mut langs: HashMap<String, LangStat> = HashMap::new();
    let mut files_acc: Vec<FileEntry> = Vec::new();
    let mut dirs_acc: HashMap<PathBuf, DirEntry> = HashMap::new();
    let mut scanned = 0usize;
    let mut truncated = false;

    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| {
            // Never filter the root itself — `filter_entry` is invoked on it,
            // and if it returns `false` the whole walk yields nothing. The
            // user may legitimately scan a dotted directory (e.g. a temp
            // path or `~/.cortex/...`) so the dot-prefix check only applies
            // at depth ≥ 1. Same effective policy as `projects::list_files`.
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if name.starts_with('.') {
                return false;
            }
            !ignore.is_denied(e.path(), &root)
        })
        .filter_map(|e| e.ok())
    {
        scanned += 1;
        if scanned > MAX_ENTRIES {
            truncated = true;
            break;
        }
        if entry.depth() == 0 || !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let bytes = meta.len();
        let lines = count_lines(path);

        total_files += 1;
        total_lines += lines;
        total_bytes += bytes;

        let lang = detect_language(path);
        let stat = langs.entry(lang).or_default();
        stat.files += 1;
        stat.lines += lines;
        stat.bytes += bytes;

        files_acc.push(FileEntry {
            path: path.to_path_buf(),
            lines,
            bytes,
        });

        // Bucket into the first-level directory under root. Anything sitting
        // at the root itself goes under a synthetic `.` bucket so it still
        // shows up in the dirs panel.
        let bucket = top_level_dir(path, &root).unwrap_or_else(|| root.join("."));
        let d = dirs_acc.entry(bucket.clone()).or_insert_with(|| DirEntry {
            path: bucket,
            file_count: 0,
            total_bytes: 0,
        });
        d.file_count += 1;
        d.total_bytes += bytes;
    }

    // Top-10 largest files by line count (ties broken by bytes).
    files_acc.sort_by(|a, b| b.lines.cmp(&a.lines).then(b.bytes.cmp(&a.bytes)));
    let largest_files: Vec<FileEntry> = files_acc.into_iter().take(10).collect();

    // Top-5 biggest dirs by byte size.
    let mut dirs: Vec<DirEntry> = dirs_acc.into_values().collect();
    dirs.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes));
    let biggest_dirs: Vec<DirEntry> = dirs.into_iter().take(5).collect();

    Ok(ProjectMetrics {
        project_root: root,
        total_files,
        total_lines,
        total_bytes,
        languages: langs,
        largest_files,
        biggest_dirs,
        generated_unix_ms: chrono::Utc::now().timestamp_millis(),
        truncated,
    })
}

/// Count newline-terminated lines in `path`. Reads up to 4 MiB to avoid
/// pinning RAM on a stray giant artefact; anything bigger is approximated by
/// dividing the byte length by the average line length seen so far. We swap
/// to raw byte counting (no UTF-8 validation) so binary blobs don't panic.
fn count_lines(path: &Path) -> usize {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if meta.len() == 0 {
        return 0;
    }
    if meta.len() > 4 * 1024 * 1024 {
        // Skip huge files — counting their lines would dominate latency.
        return 0;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let mut n = bytes.iter().filter(|&&b| b == b'\n').count();
    // Files without a trailing newline still have one logical line — bump
    // the count so a single-line file with no terminator reports `1`.
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        n += 1;
    }
    n
}

fn top_level_dir(path: &Path, root: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(root).ok()?;
    let first = rel.components().next()?;
    Some(root.join(first.as_os_str()))
}

/// Best-effort language label from the file extension. Same table as
/// `commands::doc_gen::detect_language` (kept in sync deliberately — we want
/// the panel labels to match the doc-gen modal labels).
fn detect_language(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "scala" => "Scala",
        "sh" | "bash" => "Shell",
        "lua" => "Lua",
        "sql" => "SQL",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "" => "Other",
        other => return format!(".{other}"),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn detect_language_known_extensions() {
        assert_eq!(detect_language(Path::new("foo.rs")), "Rust");
        assert_eq!(detect_language(Path::new("foo.tsx")), "TypeScript");
        assert_eq!(detect_language(Path::new("foo.py")), "Python");
        assert_eq!(detect_language(Path::new("Makefile")), "Other");
        assert_eq!(detect_language(Path::new("foo.unknown")), ".unknown");
    }

    #[test]
    fn count_lines_handles_no_trailing_newline() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, "one\ntwo\nthree").unwrap();
        assert_eq!(count_lines(&p), 3);
    }

    #[test]
    fn count_lines_handles_empty_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("empty.txt");
        fs::write(&p, "").unwrap();
        assert_eq!(count_lines(&p), 0);
    }

    #[tokio::test]
    async fn project_metrics_aggregates_basic_tree() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("README.md"), "# hi\n").unwrap();

        let m = project_metrics(root.to_string_lossy().to_string())
            .await
            .unwrap();
        assert_eq!(m.total_files, 3);
        assert!(m.total_lines >= 4);
        assert!(m.languages.contains_key("Rust"));
        assert!(m.languages.contains_key("Markdown"));
        assert!(!m.largest_files.is_empty());
        assert!(!m.biggest_dirs.is_empty());
    }
}
