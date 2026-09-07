//! `build_dep_graph` — walks the active project, parses import statements
//! out of every supported source file, and emits a node-per-file +
//! edge-per-import graph the frontend can render as a force-directed
//! visualization.
//!
//! Resolution is best-effort and intentionally tiny: we only emit an
//! edge when the importer's specifier maps onto a known project-local
//! file. External packages, stdlib references, and unresolved relative
//! paths are dropped silently so the graph stays focused on the
//! intra-project dependency shape (which is what the user actually
//! wants to look at).
//!
//! We cap at 500 nodes / 2000 edges so the frontend's O(n²) force
//! simulation stays smooth even on a fully-truncated payload.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use walkdir::WalkDir;

use crate::projects::ignore::CortexIgnore;

const MAX_NODES: usize = 500;
const MAX_EDGES: usize = 2000;
const MAX_FILE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Serialize)]
pub struct DepNode {
    /// Project-relative forward-slash path; used as the node id.
    pub id: String,
    /// Final path component for compact display in the SVG.
    pub label: String,
    /// One of: `ts`, `tsx`, `js`, `jsx`, `rs`, `py`, `css`, `html`,
    /// `json`, `md`, `other`.
    pub language: String,
    /// Line count of the source file. Drives rendered radius.
    pub lines: usize,
}

#[derive(Debug, Serialize)]
pub struct DepEdge {
    pub from: String,
    pub to: String,
    /// `import`, `require`, `use`, `mod`, or `py-import`.
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct DepGraph {
    pub nodes: Vec<DepNode>,
    pub edges: Vec<DepEdge>,
    /// Whether we hit the 500-node / 2000-edge caps.
    pub truncated: bool,
}

// ---- Regex sets ------------------------------------------------------------

// ESM: `import … from "foo"` and `import "foo"` and `export … from "foo"`.
static JS_FROM_RX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"from\s+["']([^"']+)["']"#).unwrap());
// CJS: `require("foo")`.
static JS_REQUIRE_RX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"require\(\s*["']([^"']+)["']\s*\)"#).unwrap());

// Rust: `use a::b::c;` (capture the whole tree, we'll split on `::`).
static RS_USE_RX: Lazy<Regex> = Lazy::new(|| Regex::new(r"use\s+([^;]+);").unwrap());
// Rust: `mod foo;` (declares a sibling module file).
static RS_MOD_RX: Lazy<Regex> = Lazy::new(|| Regex::new(r"mod\s+(\w+)\s*;").unwrap());

// Python: `from a.b import …` OR `import a.b`.
static PY_IMPORT_RX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:from\s+([^\s]+)\s+import|import\s+([^\s,]+))").unwrap()
});

// ---- Language detection ----------------------------------------------------

fn detect_lang(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "rs" => "rs",
        "py" => "py",
        "css" | "scss" | "sass" => "css",
        "html" | "htm" => "html",
        "json" => "json",
        "md" | "markdown" => "md",
        _ => return None,
    })
}

fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".git"
            | ".cortex-worktrees"
            | ".next"
            | ".svelte-kit"
            | ".turbo"
            | ".cache"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".pytest_cache"
            | ".idea"
            | ".vscode"
            | "vendor"
            | "Pods"
            | "DerivedData"
    )
}

fn rel_id(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn count_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count().max(1)
    }
}

// ---- Resolution ------------------------------------------------------------

/// JS/TS extension candidates to probe when a specifier omits its
/// extension. Order matters — `.ts` wins over `.js` when both exist.
const JS_EXT_CANDIDATES: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

/// Resolve a JS/TS specifier (`./foo`, `../bar/baz`, `@/lib/x`) against
/// the importer's directory and the project root. Returns the matching
/// node id (project-relative path) when something on disk wins.
fn resolve_js(
    spec: &str,
    importer_dir: &Path,
    root: &Path,
    known: &HashSet<String>,
) -> Option<String> {
    // Skip obvious externals.
    if !spec.starts_with('.') && !spec.starts_with('@') && !spec.starts_with('/') {
        return None;
    }

    // `@/foo/bar` → project-root-relative. Common in Vite/Tauri configs.
    let candidate_base: PathBuf = if let Some(rest) = spec.strip_prefix("@/") {
        root.join(rest)
    } else if spec.starts_with('/') {
        root.join(spec.trim_start_matches('/'))
    } else {
        importer_dir.join(spec)
    };

    // Direct hit (specifier already had an extension).
    if let Ok(canon) = candidate_base.canonicalize() {
        let id = rel_id(&canon, root);
        if known.contains(&id) {
            return Some(id);
        }
    }

    // Try `<base>.<ext>` for each known JS/TS extension.
    for ext in JS_EXT_CANDIDATES {
        let probe = candidate_base.with_extension(ext);
        let id = rel_id(&probe, root);
        if known.contains(&id) {
            return Some(id);
        }
    }

    // Try `<base>/index.<ext>` for directory-style imports.
    for ext in JS_EXT_CANDIDATES {
        let probe = candidate_base.join(format!("index.{}", ext));
        let id = rel_id(&probe, root);
        if known.contains(&id) {
            return Some(id);
        }
    }

    None
}

/// Resolve a Rust `use crate::foo::bar` or `mod bar;` against the
/// importer's location. Cortex projects almost always live under
/// `src/`, so we walk that subtree looking for a matching basename.
/// Best-effort — we don't pretend to implement rustc.
fn resolve_rust(
    segment: &str,
    importer: &Path,
    root: &Path,
    known: &HashSet<String>,
) -> Option<String> {
    let importer_dir = importer.parent()?;
    let leaf = segment.split("::").last()?.trim();
    if leaf.is_empty() || leaf == "*" || leaf == "self" || leaf == "super" {
        return None;
    }

    // Sibling file `<leaf>.rs`.
    let sibling = importer_dir.join(format!("{}.rs", leaf));
    let id = rel_id(&sibling, root);
    if known.contains(&id) {
        return Some(id);
    }

    // Sibling directory `<leaf>/mod.rs`.
    let mod_path = importer_dir.join(leaf).join("mod.rs");
    let id = rel_id(&mod_path, root);
    if known.contains(&id) {
        return Some(id);
    }

    None
}

/// Resolve a Python `from a.b.c import x` (or `import a.b`) against the
/// project root. Dots become slashes; we try both `a/b/c.py` and
/// `a/b/c/__init__.py`.
fn resolve_python(
    spec: &str,
    root: &Path,
    known: &HashSet<String>,
) -> Option<String> {
    let trimmed = spec.trim_start_matches('.').trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let base = parts.iter().fold(PathBuf::from(root), |acc, p| acc.join(p));

    let file = base.with_extension("py");
    let id = rel_id(&file, root);
    if known.contains(&id) {
        return Some(id);
    }
    let pkg = base.join("__init__.py");
    let id = rel_id(&pkg, root);
    if known.contains(&id) {
        return Some(id);
    }
    None
}

// ---- Main command ----------------------------------------------------------

#[tauri::command]
pub async fn build_dep_graph(project_root: String) -> Result<DepGraph, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {}", project_root));
    }

    let ignore = CortexIgnore::load(&root);

    // ---- First pass: collect nodes. -------------------------------------
    let walker = WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.file_name().to_string_lossy().as_ref()));

    let mut nodes: Vec<DepNode> = Vec::new();
    let mut content_by_id: HashMap<String, (PathBuf, String, &'static str)> = HashMap::new();
    let mut known_ids: HashSet<String> = HashSet::new();
    let mut truncated = false;

    'walk: for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(lang) = detect_lang(path) else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        if ignore.is_denied(path, &root) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(path) else { continue };
        let id = rel_id(path, &root);
        let label = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| id.clone());
        let lines = count_lines(&content);

        nodes.push(DepNode {
            id: id.clone(),
            label,
            language: lang.to_string(),
            lines,
        });
        known_ids.insert(id.clone());
        content_by_id.insert(id, (path.to_path_buf(), content, lang));

        if nodes.len() >= MAX_NODES {
            truncated = true;
            break 'walk;
        }
    }

    // ---- Second pass: parse imports + resolve. --------------------------
    let mut edges: Vec<DepEdge> = Vec::new();
    let mut edge_set: HashSet<(String, String, String)> = HashSet::new();

    'edges: for node in &nodes {
        let Some((path, content, lang)) = content_by_id.get(&node.id) else { continue };
        let importer_dir = path.parent().unwrap_or(&root).to_path_buf();

        // Collected as (specifier, kind) so we apply the right resolver.
        let mut specs: Vec<(String, &'static str)> = Vec::new();

        match *lang {
            "ts" | "tsx" | "js" | "jsx" => {
                for c in JS_FROM_RX.captures_iter(content) {
                    if let Some(m) = c.get(1) {
                        specs.push((m.as_str().to_string(), "import"));
                    }
                }
                for c in JS_REQUIRE_RX.captures_iter(content) {
                    if let Some(m) = c.get(1) {
                        specs.push((m.as_str().to_string(), "require"));
                    }
                }
            }
            "rs" => {
                for c in RS_USE_RX.captures_iter(content) {
                    if let Some(m) = c.get(1) {
                        specs.push((m.as_str().trim().to_string(), "use"));
                    }
                }
                for c in RS_MOD_RX.captures_iter(content) {
                    if let Some(m) = c.get(1) {
                        specs.push((m.as_str().to_string(), "mod"));
                    }
                }
            }
            "py" => {
                for c in PY_IMPORT_RX.captures_iter(content) {
                    if let Some(m) = c.get(1).or_else(|| c.get(2)) {
                        specs.push((m.as_str().to_string(), "py-import"));
                    }
                }
            }
            _ => continue, // css/html/json/md don't get edges.
        }

        for (spec, kind) in specs {
            let resolved = match *lang {
                "ts" | "tsx" | "js" | "jsx" => {
                    resolve_js(&spec, &importer_dir, &root, &known_ids)
                }
                "rs" => resolve_rust(&spec, path, &root, &known_ids),
                "py" => resolve_python(&spec, &root, &known_ids),
                _ => None,
            };
            let Some(to_id) = resolved else { continue };
            if to_id == node.id {
                continue; // skip self-edges
            }
            let key = (node.id.clone(), to_id.clone(), kind.to_string());
            if !edge_set.insert(key) {
                continue;
            }
            edges.push(DepEdge {
                from: node.id.clone(),
                to: to_id,
                kind: kind.to_string(),
            });
            if edges.len() >= MAX_EDGES {
                truncated = true;
                break 'edges;
            }
        }
    }

    Ok(DepGraph {
        nodes,
        edges,
        truncated,
    })
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_languages_by_extension() {
        assert_eq!(detect_lang(Path::new("a.ts")), Some("ts"));
        assert_eq!(detect_lang(Path::new("a.tsx")), Some("tsx"));
        assert_eq!(detect_lang(Path::new("a.rs")), Some("rs"));
        assert_eq!(detect_lang(Path::new("a.py")), Some("py"));
        assert_eq!(detect_lang(Path::new("a.unknown")), None);
    }

    #[test]
    fn skipped_dirs_match() {
        assert!(is_skipped_dir("node_modules"));
        assert!(is_skipped_dir("target"));
        assert!(!is_skipped_dir("src"));
    }

    #[test]
    fn rel_id_uses_forward_slashes() {
        let root = Path::new("/tmp/proj");
        let p = root.join("src").join("lib.rs");
        assert_eq!(rel_id(&p, root), "src/lib.rs");
    }

    #[test]
    fn count_lines_handles_empty() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a\nb\nc"), 3);
    }

    #[test]
    fn resolve_python_finds_dotted_modules() {
        let mut known = HashSet::new();
        known.insert("pkg/sub/mod.py".to_string());
        let root = Path::new("/tmp/proj");
        let r = resolve_python("pkg.sub.mod", root, &known);
        assert_eq!(r.as_deref(), Some("pkg/sub/mod.py"));
    }
}
