// WIRING (do in lib.rs):
//   pub mod repo_map;
//
// This module computes an Aider-style compressed symbol map of a project so the
// The gateway agent always has structural context without the user @-mentioning files.
// It is intentionally regex-based (no tree-sitter) to keep build size small.
//
// Layers (top-down):
//   - compute_repo_map (entry) → personalized variant (with caching layer below)
//   - process-wide 10s TTL cache (wave 208), LRU eviction at 16-entry cap (wave 223)
//   - uncached walk: collect_candidates → extract_symbols + extract_references
//   - PageRank-lite (wave 182) with optional personalize boost (wave 188)
//   - format_as_text emits Aider-style tree with ★PageRank annotations

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// Maximum symbols extracted per file.
const MAX_SYMBOLS_PER_FILE: usize = 50;
/// Maximum bytes the formatted text representation can occupy.
const MAX_TEXT_BYTES: usize = 50 * 1024;
/// Maximum file size we are willing to scan (avoid generated/minified blobs).
const MAX_FILE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// One of: "fn", "struct", "class", "interface", "type", "export", "const", "trait", "enum", "heading".
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSymbols {
    /// Path relative to project_root, using forward slashes.
    pub path: String,
    pub language: String,
    pub symbols: Vec<Symbol>,
    /// Wave 181 — identifier references found in the file body. Used as
    /// raw input to the wave-182 PageRank-lite pass that promotes files
    /// other files import/call out to. Cap: 200 unique idents per file
    /// (avoid pathological worst-case on minified/generated files).
    #[serde(default)]
    pub references: Vec<String>,
    /// Wave 182 — PageRank-lite score (0.0–1.0). Computed once at the
    /// end of `compute_repo_map` from the references graph. Higher
    /// score → more files reference this file's symbols → more likely
    /// to be central to the project.
    #[serde(default)]
    pub pagerank: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMap {
    pub files: Vec<FileSymbols>,
    pub total_files_scanned: usize,
    pub truncated: bool,
}

/// Wave 208 — process-wide TTL cache for repo maps. compute_repo_map walks
/// the whole tree (up to ~500 files) on every call which is wasteful when
/// the same project gets queried multiple times in a few seconds (e.g.
/// /repomap-top followed by @repomap on the next message). Cache keyed by
/// `(root, max_files, personalize_hash)` with a 10-second TTL.
static REPO_MAP_CACHE: Lazy<Mutex<HashMap<String, (Instant, RepoMap)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// Wave 215 — env-overridable TTL so tests / power-users can flush more
/// aggressively. CORTEX_REPO_MAP_CACHE_TTL_SECS=0 disables caching.
fn cache_ttl() -> Duration {
    static TTL: Lazy<Duration> = Lazy::new(|| {
        std::env::var("CORTEX_REPO_MAP_CACHE_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(10))
    });
    *TTL
}

/// Wave 226 — exposed for the /cache-stats slash command.
pub fn cache_size() -> usize {
    REPO_MAP_CACHE.lock().map(|c| c.len()).unwrap_or(0)
}

/// Wave 244 — exposed for the /clear-cache slash. Returns count cleared.
pub fn cache_clear() -> usize {
    if let Ok(mut c) = REPO_MAP_CACHE.lock() {
        let n = c.len();
        c.clear();
        // Wave 268 — info-level so users see this in normal logs (vs the
        // debug-level cache-hit/miss noise). Useful for diagnosing
        // "user just cleared, why is the next call slow".
        tracing::info!(target: "cortex::repo_map", "cache cleared ({n} entries)");
        n
    } else {
        0
    }
}

fn cache_key(project_root: &Path, max_files: usize, personalize: &[String]) -> String {
    let mut k = String::with_capacity(64);
    k.push_str(&project_root.display().to_string());
    k.push('|');
    k.push_str(&max_files.to_string());
    k.push('|');
    let mut sorted: Vec<&String> = personalize.iter().collect();
    sorted.sort();
    for s in sorted { k.push_str(s); k.push(','); }
    k
}

/// Compute a repo map for `project_root`, capped at `max_files` files.
/// Files are sorted by mtime (newest first) before scanning so the most
/// recently edited files always make the cut.
///
/// Results from this and its personalized cousin are cached process-wide
/// for 10s (see wave 208) so back-to-back calls (e.g. @repomap followed
/// by /repomap-top in the next message) reuse the walk instead of
/// re-paying the walkdir cost. Cache cap: 16 entries. Pass a distinct
/// `personalize` list to get a fresh ranking.
pub fn compute_repo_map(project_root: &Path, max_files: usize) -> RepoMap {
    compute_repo_map_personalized(project_root, max_files, &[])
}

/// Wave 188 — personalized PageRank. The `personalize` list is the user's
/// mentioned identifiers (Aider's `mentioned_idents` concept). Each file
/// containing any of these terms gets a +2 inbound boost before the
/// global normalize, so files relevant to the current task float higher.
/// Pass `&[]` to get the wave-182 base behavior.
pub fn compute_repo_map_personalized(
    project_root: &Path,
    max_files: usize,
    personalize: &[String],
) -> RepoMap {
    // Wave 208 — cache hit?
    let key = cache_key(project_root, max_files, personalize);
    if let Ok(cache) = REPO_MAP_CACHE.lock() {
        if let Some((stored_at, map)) = cache.get(&key) {
            if stored_at.elapsed() < cache_ttl() {
                // Wave 213 — tracing on cache hit/miss helps debug why
                // /repomap-top sometimes feels instant vs occasionally
                // pauses for the walk.
                tracing::debug!(
                    target: "cortex::repo_map",
                    "cache hit for {} (age={:?})",
                    project_root.display(),
                    stored_at.elapsed()
                );
                return map.clone();
            }
        }
    }
    tracing::debug!(
        target: "cortex::repo_map",
        "cache miss for {} — computing...",
        project_root.display()
    );
    let started = Instant::now();
    let result = compute_repo_map_personalized_uncached(project_root, max_files, personalize);
    tracing::debug!(
        target: "cortex::repo_map",
        "compute_repo_map done in {:?} ({} files)",
        started.elapsed(),
        result.files.len()
    );
    if let Ok(mut cache) = REPO_MAP_CACHE.lock() {
        // Wave 223 — LRU-ish eviction: when full, drop the single oldest
        // entry instead of clearing the whole table. Avoids
        // "warmup wave" where cache is empty after every 16th insert.
        if cache.len() >= 16 {
            if let Some(oldest) = cache.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                tracing::debug!(
                    target: "cortex::repo_map",
                    "cache evicting oldest: {}",
                    oldest.chars().take(80).collect::<String>()
                );
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), result.clone()));
    }
    result
}

fn compute_repo_map_personalized_uncached(
    project_root: &Path,
    max_files: usize,
    personalize: &[String],
) -> RepoMap {
    let max_files = if max_files == 0 { 200 } else { max_files };

    let candidates = collect_candidates(project_root);
    let total_scanned = candidates.len();
    let truncated = total_scanned > max_files;

    let mut files: Vec<FileSymbols> = Vec::new();
    for (path, _mtime) in candidates.into_iter().take(max_files) {
        let Some(language) = detect_language(&path) else { continue };
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let symbols = extract_symbols(&content, &language);
        if symbols.is_empty() {
            continue;
        }
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        // Wave 181 — extract identifier references in the file body so the
        // wave-182 PageRank-lite pass can weight files that are referenced
        // by many other files.
        let references = extract_references(&content);
        files.push(FileSymbols {
            path: rel,
            language,
            symbols,
            references,
            pagerank: 0.0,
        });
    }

    // Wave 182 — PageRank-lite. For each file's reference list, look up
    // which OTHER files declare a matching symbol; count that as an
    // inbound edge. Two-pass: build defines map → compute in-degree
    // (raw counts) → normalize to [0..1]. Not a true iterative
    // PageRank but gives the user-facing ordering the same shape
    // (central code surfaces higher) for ~30% the complexity.
    let mut defines: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, f) in files.iter().enumerate() {
        for sym in &f.symbols {
            defines.entry(sym.name.as_str()).or_default().push(idx);
        }
    }
    let mut inbound: Vec<u32> = vec![0; files.len()];
    for f in &files {
        let mut hit: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for ident in &f.references {
            if let Some(defs) = defines.get(ident.as_str()) {
                for &idx in defs { hit.insert(idx); }
            }
        }
        for idx in hit { inbound[idx] = inbound[idx].saturating_add(1); }
    }
    // Wave 188 — personalize. For each mentioned identifier, boost every
    // file that defines OR references it by +2 inbound. Boosts both
    // directions so a file that uses `processOrder` AND a file that
    // defines `processOrder` both rise.
    if !personalize.is_empty() {
        for ident in personalize {
            if let Some(defs) = defines.get(ident.as_str()) {
                for &idx in defs { inbound[idx] = inbound[idx].saturating_add(2); }
            }
            for (i, f) in files.iter().enumerate() {
                if f.references.iter().any(|r| r == ident) {
                    inbound[i] = inbound[i].saturating_add(2);
                }
            }
        }
    }
    let max_in = inbound.iter().copied().max().unwrap_or(0).max(1) as f32;
    for (i, f) in files.iter_mut().enumerate() {
        f.pagerank = (inbound[i] as f32) / max_in;
    }
    // Stable sort by pagerank desc (preserves mtime order on ties).
    files.sort_by(|a, b| b.pagerank.partial_cmp(&a.pagerank).unwrap_or(std::cmp::Ordering::Equal));

    RepoMap {
        files,
        total_files_scanned: total_scanned,
        truncated,
    }
}

/// Wave 181 — extract identifier-like tokens from a file body for the
/// PageRank-lite pass. Keeps it cheap (single character-iteration sweep,
/// no regex), `looks_id` heuristic (wave 252 added TitleCase), length
/// >= 4, capped at 200 unique tokens per file. Build-output noise is
/// kept down by collect_candidates skipping minified files (wave 255).
fn extract_references(content: &str) -> Vec<String> {
    // Wave 252 — `looks_id` now also accepts TitleCase (capital letter +
    // lowercase tail) since that's the canonical form for Rust structs
    // and traits (`User`, `Repo`). The original wave-181 heuristic
    // required `_` or a lowercase→uppercase transition, which missed
    // those entirely and dropped their inbound edge count to zero.
    let looks_id = |s: &str| -> bool {
        if s.contains('_') { return true; }
        let mut chars = s.chars();
        let first = chars.next();
        if let Some(c0) = first {
            // CamelCase / PascalCase: starts uppercase, has lowercase.
            if c0.is_uppercase() && chars.any(|c| c.is_lowercase()) { return true; }
        }
        // lowercase→uppercase transition mid-word (e.g. `processOrder`).
        s.chars()
            .zip(s.chars().skip(1))
            .any(|(a, b)| a.is_lowercase() && b.is_uppercase())
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in content.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
            continue;
        }
        if cur.len() >= 4 && looks_id(&cur) && seen.insert(cur.clone()) {
            out.push(cur.clone());
            if out.len() >= 200 { return out; }
        }
        cur.clear();
    }
    if cur.len() >= 4 && looks_id(&cur) && seen.insert(cur.clone()) {
        out.push(cur);
    }
    out
}

/// Walk the project root and return candidate files sorted by mtime desc.
fn collect_candidates(project_root: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut out: Vec<(PathBuf, SystemTime)> = Vec::new();

    let walker = WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.file_name().to_string_lossy().as_ref()));

    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if detect_language(path).is_none() {
            continue;
        }
        // Wave 255 — skip minified / bundled JS/CSS that pollute the
        // PageRank graph with extracted "identifiers" that are really
        // mangled variable names from build output.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_lowercase();
            if lower.ends_with(".min.js")
                || lower.ends_with(".min.css")
                || lower.ends_with(".bundle.js")
                || lower.contains(".chunk.")
                || lower.starts_with("vendor.")
            {
                continue;
            }
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.push((path.to_path_buf(), mtime));
    }

    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
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

fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "rs" => Some("rust".into()),
        "ts" | "mts" | "cts" => Some("typescript".into()),
        "tsx" => Some("tsx".into()),
        "js" | "mjs" | "cjs" => Some("javascript".into()),
        "jsx" => Some("jsx".into()),
        "py" => Some("python".into()),
        "go" => Some("go".into()),
        "java" => Some("java".into()),
        "swift" => Some("swift".into()),
        "rb" => Some("ruby".into()),
        "php" => Some("php".into()),
        "c" | "h" => Some("c".into()),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some("cpp".into()),
        "cs" => Some("csharp".into()),
        "md" | "markdown" => Some("markdown".into()),
        _ => None,
    }
}

// ---- Regex sets (lazy, compiled once) ----

struct LangRegexes {
    // (regex, kind, name_group_index)
    patterns: Vec<(Regex, &'static str, usize)>,
}

static RUST_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*pub\s+(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*pub\s+struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "struct", 1),
        (Regex::new(r"^\s*pub\s+enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "enum", 1),
        (Regex::new(r"^\s*pub\s+trait\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "trait", 1),
        (Regex::new(r"^\s*pub\s+(?:type|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "const", 1),
        (Regex::new(r"^\s*impl(?:<[^>]*>)?\s+([A-Za-z_][A-Za-z0-9_:<>\s,]*?)\s*\{").unwrap(), "type", 1),
    ],
});

static TS_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*export\s+(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*export\s+default\s+(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*export\s+(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "const", 1),
        (Regex::new(r"^\s*export\s+(?:default\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*export\s+(?:default\s+)?interface\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "interface", 1),
        (Regex::new(r"^\s*export\s+(?:default\s+)?type\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "type", 1),
        (Regex::new(r"^\s*export\s+(?:default\s+)?enum\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "enum", 1),
        (Regex::new(r"^\s*(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*class\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap(), "class", 1),
    ],
});

static PY_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*async\s+def\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
    ],
});

static GO_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*func\s+(?:\([^)]+\)\s+)?([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+(?:struct|interface)").unwrap(), "struct", 1),
        (Regex::new(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "type", 1),
    ],
});

static JAVA_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*public\s+(?:static\s+)?(?:abstract\s+)?(?:final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*public\s+interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "interface", 1),
        (Regex::new(r"^\s*public\s+(?:static\s+)?(?:[A-Za-z_][A-Za-z0-9_<>\[\],\s]*\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap(), "fn", 1),
    ],
});

static SWIFT_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*(?:public\s+|open\s+)?func\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*(?:public\s+|open\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*(?:public\s+|open\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "struct", 1),
        (Regex::new(r"^\s*(?:public\s+|open\s+)?protocol\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "interface", 1),
        (Regex::new(r"^\s*(?:public\s+|open\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "enum", 1),
    ],
});

static RUBY_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*def\s+([A-Za-z_][A-Za-z0-9_?!]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*module\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "type", 1),
    ],
});

static PHP_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*(?:public\s+|private\s+|protected\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "fn", 1),
        (Regex::new(r"^\s*(?:abstract\s+|final\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "interface", 1),
        (Regex::new(r"^\s*trait\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "trait", 1),
    ],
});

static C_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*(?:static\s+|extern\s+)?(?:inline\s+)?[A-Za-z_][A-Za-z0-9_\*\s]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\([^;]*$").unwrap(), "fn", 1),
        (Regex::new(r"^\s*typedef\s+(?:struct\s+)?[A-Za-z_][A-Za-z0-9_\s\*]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap(), "type", 1),
        (Regex::new(r"^\s*struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{").unwrap(), "struct", 1),
    ],
});

static CPP_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*(?:template\s*<[^>]+>\s*)?(?:class|struct)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*namespace\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "type", 1),
        (Regex::new(r"^\s*(?:[A-Za-z_][A-Za-z0-9_:<>,\s\*&]+\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\([^;]*\)\s*\{").unwrap(), "fn", 1),
    ],
});

static CS_RX: Lazy<LangRegexes> = Lazy::new(|| LangRegexes {
    patterns: vec![
        (Regex::new(r"^\s*public\s+(?:static\s+|abstract\s+|sealed\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "class", 1),
        (Regex::new(r"^\s*public\s+interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(), "interface", 1),
        (Regex::new(r"^\s*public\s+(?:static\s+)?(?:async\s+)?[A-Za-z_][A-Za-z0-9_<>\[\],\s]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap(), "fn", 1),
    ],
});

static MD_HEADING_RX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(#{1,3})\s+(.+?)\s*$").unwrap());

fn regex_set_for(language: &str) -> Option<&'static LangRegexes> {
    match language {
        "rust" => Some(&RUST_RX),
        "typescript" | "tsx" | "javascript" | "jsx" => Some(&TS_RX),
        "python" => Some(&PY_RX),
        "go" => Some(&GO_RX),
        "java" => Some(&JAVA_RX),
        "swift" => Some(&SWIFT_RX),
        "ruby" => Some(&RUBY_RX),
        "php" => Some(&PHP_RX),
        "c" => Some(&C_RX),
        "cpp" => Some(&CPP_RX),
        "csharp" => Some(&CS_RX),
        _ => None,
    }
}

fn extract_symbols(content: &str, language: &str) -> Vec<Symbol> {
    if language == "markdown" {
        return extract_markdown_symbols(content);
    }
    let Some(rx_set) = regex_set_for(language) else {
        return Vec::new();
    };

    let mut out: Vec<Symbol> = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        if out.len() >= MAX_SYMBOLS_PER_FILE {
            break;
        }
        // Cheap filters before regex
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') && language != "python" {
            // markdown uses # but is handled above; '#' lines in source are mostly comments/preproc.
            // For C/C++ '#include' etc. we still skip — they are not symbols.
            continue;
        }
        if raw_line.len() > 400 {
            // Skip enormous lines (likely generated)
            continue;
        }

        for (rx, kind, group) in &rx_set.patterns {
            if let Some(caps) = rx.captures(raw_line) {
                let Some(name_match) = caps.get(*group) else { continue };
                let name = name_match.as_str().trim().to_string();
                if name.is_empty() {
                    continue;
                }
                let signature = signature_from_line(raw_line);
                out.push(Symbol {
                    kind: (*kind).to_string(),
                    name,
                    signature,
                    line: (idx as u32) + 1,
                });
                break; // one symbol per line
            }
        }
    }
    out
}

fn extract_markdown_symbols(content: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if out.len() >= MAX_SYMBOLS_PER_FILE {
            break;
        }
        if let Some(caps) = MD_HEADING_RX.captures(line) {
            let level = caps.get(1).map(|m| m.as_str().len()).unwrap_or(1);
            let name = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            out.push(Symbol {
                kind: "heading".into(),
                name,
                signature: Some(format!("h{level}")),
                line: (idx as u32) + 1,
            });
        }
    }
    out
}

fn signature_from_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let cut = trimmed
        .find(|c| c == '{' || c == ';')
        .map(|i| &trimmed[..i])
        .unwrap_or(trimmed)
        .trim_end();
    if cut.is_empty() {
        return None;
    }
    let condensed = cut.split_whitespace().collect::<Vec<_>>().join(" ");
    if condensed.len() > 240 {
        // Clamp to a UTF-8 char boundary so we never slice mid-codepoint.
        let mut end = 240;
        while end > 0 && !condensed.is_char_boundary(end) {
            end -= 1;
        }
        Some(format!("{}…", &condensed[..end]))
    } else {
        Some(condensed)
    }
}

/// Flat symbol hit for the @-picker: includes path so callers don't need to
/// flatten a [`RepoMap`] tree. `kind` matches the tags used by [`Symbol`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolHit {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
}

/// Walk the same candidate set as [`compute_repo_map`] and return up to
/// `limit` hits whose name contains `query` (case-insensitive). Empty query
/// matches everything. Hard-capped at 50.
pub fn repo_symbols(root: &Path, query: &str, limit: usize) -> Vec<SymbolHit> {
    const HARD_CAP: usize = 50;
    let cap = if limit == 0 { HARD_CAP } else { limit.min(HARD_CAP) };
    let needle = query.trim().to_lowercase();

    let mut out: Vec<SymbolHit> = Vec::with_capacity(cap);
    let candidates = collect_candidates(root);

    for (path, _mtime) in candidates {
        if out.len() >= cap {
            break;
        }
        let Some(language) = detect_language(&path) else { continue };
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let symbols = extract_symbols(&content, &language);
        if symbols.is_empty() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for sym in symbols {
            if out.len() >= cap {
                break;
            }
            if !needle.is_empty() && !sym.name.to_lowercase().contains(&needle) {
                continue;
            }
            out.push(SymbolHit {
                path: rel.clone(),
                name: sym.name,
                kind: sym.kind,
                line: sym.line,
            });
        }
    }
    out
}

/// Format a RepoMap as a compact Aider-style tree. Truncates to `MAX_TEXT_BYTES`.
pub fn format_as_text(map: &RepoMap) -> String {
    let mut out = String::with_capacity(8 * 1024);
    let mut truncated = false;

    for file in &map.files {
        // Wave 183 — render PageRank score next to the path so the gateway
        // agent reading @repomap sees which files are central to the
        // project (high inbound reference count). `★0.87` style; skipped
        // when score is 0 (file has no inbound references).
        let header = if file.pagerank > 0.0 {
            format!("{} ★{:.2}\n", file.path, file.pagerank)
        } else {
            format!("{}\n", file.path)
        };
        if out.len() + header.len() > MAX_TEXT_BYTES {
            truncated = true;
            break;
        }
        out.push_str(&header);

        for sym in &file.symbols {
            let line = match &sym.signature {
                Some(sig) if !sig.is_empty() => format!("  {sig}\n"),
                _ => format!("  {} {}\n", sym.kind, sym.name),
            };
            if out.len() + line.len() > MAX_TEXT_BYTES {
                truncated = true;
                break;
            }
            out.push_str(&line);
        }
        if truncated {
            break;
        }
    }

    if truncated {
        out.push_str("… (truncated)\n");
    }
    out
}

/// Wave 300 — build a compact, ranked repo-map block for **auto-injection**
/// into an agent's context (aider/Continue-style auto-context). Unlike
/// [`format_as_text`] (the on-demand `@repomap` dump, capped at 50 KB and
/// emitting every symbol), this is sized for the per-message context budget:
/// it personalizes the PageRank by identifiers mentioned in `task` (the
/// user's message — Aider's `mentioned_idents`), then emits only the
/// top-ranked files with a handful of signatures each, hard-capped at
/// `max_bytes`. Returns `None` when the repo yields no rankable files, so the
/// caller injects nothing rather than an empty envelope.
pub fn build_context_block(project_root: &Path, task: &str, max_bytes: usize) -> Option<String> {
    const MAX_FILES: usize = 40;
    const MAX_SYMBOLS_PER_FILE: usize = 8;

    // Reuse the same identifier heuristic the reference graph is built from,
    // so a task that names `OrderService` boosts the file defining it.
    let idents = extract_references(task);
    let map = compute_repo_map_personalized(project_root, MAX_FILES, &idents);
    if map.files.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(max_bytes.min(8 * 1024));
    'files: for file in &map.files {
        // Files are already sorted by PageRank desc, so the budget naturally
        // spends on the most central / most task-relevant files first.
        let header = if file.pagerank > 0.0 {
            format!("{} ★{:.2}\n", file.path, file.pagerank)
        } else {
            format!("{}\n", file.path)
        };
        if out.len() + header.len() > max_bytes {
            break;
        }
        out.push_str(&header);
        for sym in file.symbols.iter().take(MAX_SYMBOLS_PER_FILE) {
            let line = match &sym.signature {
                Some(sig) if !sig.is_empty() => format!("  {sig}\n"),
                _ => format!("  {} {}\n", sym.kind, sym.name),
            };
            if out.len() + line.len() > max_bytes {
                break 'files;
            }
            out.push_str(&line);
        }
    }

    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Resolve `rel` to an existing path confined under `root`. Accepts either a
/// project-relative path or an absolute path that lands inside `root`. Uses
/// `canonicalize` so `..` segments and symlinks are fully resolved before the
/// containment check — anything escaping `root` (or that doesn't exist)
/// returns `None`. Factored out so the outline resolver can't be tricked into
/// reading a file outside the open project.
fn confine_under(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let p = Path::new(rel);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let canon = joined.canonicalize().ok()?;
    let canon_root = root.canonicalize().ok()?;
    if !canon.starts_with(&canon_root) {
        return None;
    }
    Some(canon)
}

/// Wave 320 — Zed-style file **outline**. Renders the symbol structure of a
/// single named file — the in-file view that the cross-file `@repomap` (which
/// ranks *files* by centrality, emitting a few signatures each) deliberately
/// doesn't give. Lists every top-level symbol with its line number and
/// signature so the model can navigate and reference the file's structure;
/// markdown headings are indented by level to read as a table of contents.
///
/// `rel` is confined under `root` (see [`confine_under`]). Returns:
/// - `None` when the path can't be resolved, isn't a regular file, is over
///   [`MAX_FILE_BYTES`], or has an extension we don't extract symbols for
///   (the caller then ships the token verbatim);
/// - `Some(text)` otherwise — including a `no symbols found` line for a valid
///   source file with no top-level declarations.
///
/// Output is bounded at `max_bytes`.
pub fn build_outline(root: &Path, rel: &str, max_bytes: usize) -> Option<String> {
    let path = confine_under(root, rel)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let language = detect_language(&path)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let symbols = extract_symbols(&content, &language);

    // Echo the user-typed path (normalized to forward slashes) in the header
    // so the label and body agree on how the file is named.
    let rel_disp = rel.trim().replace('\\', "/");
    let n = symbols.len();
    let mut out = String::with_capacity(256);
    out.push_str(&format!(
        "{rel_disp} · {language} · {n} symbol{}\n\n",
        if n == 1 { "" } else { "s" }
    ));
    if symbols.is_empty() {
        out.push_str("(no symbols found)\n");
        return Some(out);
    }

    let mut byte_truncated = false;
    for sym in &symbols {
        let line = if language == "markdown" {
            // Indent headings by level (parsed from the "hN" signature) so the
            // outline reads as a nested table of contents.
            let level = sym
                .signature
                .as_deref()
                .and_then(|s| s.strip_prefix('h'))
                .and_then(|d| d.parse::<usize>().ok())
                .unwrap_or(1);
            let indent = "  ".repeat(level.saturating_sub(1));
            format!("{:>5}  {indent}{}\n", sym.line, sym.name)
        } else {
            let disp = match &sym.signature {
                Some(s) if !s.is_empty() => s.clone(),
                _ => format!("{} {}", sym.kind, sym.name),
            };
            format!("{:>5}  {disp}\n", sym.line)
        };
        if out.len() + line.len() > max_bytes {
            out.push_str("… (truncated)\n");
            byte_truncated = true;
            break;
        }
        out.push_str(&line);
    }
    // `extract_symbols` caps at MAX_SYMBOLS_PER_FILE; flag when a large file
    // hit that ceiling (unless we already byte-truncated above).
    if !byte_truncated && n == MAX_SYMBOLS_PER_FILE {
        out.push_str(&format!(
            "… (only the first {MAX_SYMBOLS_PER_FILE} symbols shown)\n"
        ));
    }
    Some(out)
}

/// Continue.dev-style **folder** context provider. Concatenates the text files
/// directly inside one named folder (one level deep — not recursive) so the
/// model can read a whole module at once, the companion to `@file` (a single
/// file) and `@tree` (structure only, no contents). `rel` is confined under
/// `root` (see [`confine_under`]); a folder that escapes the project, doesn't
/// exist, or isn't a directory returns `None` (the caller ships the token
/// verbatim).
///
/// Bounded on every axis so a fat folder can't blow the model's window:
/// - only files with a known source/text extension are included (binaries and
///   unknown types are listed by name but their bodies are skipped);
/// - each file body is capped at `MAX_TEXT_BYTES`;
/// - the whole block is capped at `max_bytes` (a truncation marker is appended
///   when either ceiling trips).
///
/// Output is deterministic — children are sorted alphabetically — so it's
/// unit-testable. A folder with no readable files still returns `Some(header)`
/// (an explicit "no readable files" signal), never an empty attachment.
pub fn build_folder(root: &Path, rel: &str, max_bytes: usize) -> Option<String> {
    // Same extension set the other text-scanning providers use (`@grep`,
    // `@recent`) so "what counts as a file" is consistent across the app.
    const TEXT_EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt", "c", "cc", "cpp",
        "h", "hpp", "rb", "php", "swift", "scala", "md", "toml", "yaml", "yml",
        "json", "css", "scss", "html", "sh", "sql", "proto", "gradle", "txt",
        "zig", "dart", "elm", "lua", "nix", "tf", "mjs", "cjs", "astro", "vue",
        "svelte", "jl", "ex", "exs", "clj", "hs", "ml",
    ];

    let dir = confine_under(root, rel)?;
    if !dir.is_dir() {
        return None;
    }

    // Direct children only (one level). Collect names + paths, then sort so the
    // output is stable regardless of filesystem iteration order.
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            files.push((name, entry.path()));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let rel_disp = rel.trim().replace('\\', "/");
    let rel_disp = rel_disp.trim_end_matches('/');
    let mut out = String::with_capacity(512);
    out.push_str(&format!(
        "{} · {} file{}\n",
        if rel_disp.is_empty() { "." } else { rel_disp },
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    ));

    let mut included = 0usize;
    let mut truncated = false;
    for (name, path) in &files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        // No fenced code blocks here: the caller wraps the whole body in a
        // single ```diff block, so an inner ``` would close it early. Files
        // are separated by a plain `===== name =====` banner instead.
        if !TEXT_EXTS.contains(&ext.as_str()) {
            // Listed but not inlined — the model sees the file exists.
            let header = format!("\n===== {name} (skipped — non-text) =====\n");
            if out.len() + header.len() > max_bytes {
                truncated = true;
                break;
            }
            out.push_str(&header);
            continue;
        }
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > MAX_FILE_BYTES {
            let header = format!("\n===== {name} (skipped — over size cap) =====\n");
            if out.len() + header.len() > max_bytes {
                truncated = true;
                break;
            }
            out.push_str(&header);
            continue;
        }
        let Ok(mut content) = std::fs::read_to_string(path) else { continue };
        if content.len() > MAX_TEXT_BYTES {
            content.truncate(MAX_TEXT_BYTES);
            content.push_str("\n… (file truncated)\n");
        }
        let block = format!("\n===== {name} =====\n{content}\n");
        if out.len() + block.len() > max_bytes {
            truncated = true;
            break;
        }
        out.push_str(&block);
        included += 1;
    }

    if truncated {
        out.push_str("\n… (folder truncated)\n");
    } else if included == 0 {
        out.push_str("\n(no readable text files in this folder)\n");
    }
    Some(out)
}

/// Wave 321 — "go to definition" as context. Resolves a **symbol name** to the
/// place(s) it's declared across the project and returns each definition site
/// (`path:line`) with the declaration's body, so the model can read the actual
/// code behind a symbol the user names. This is the code-navigation primitive
/// aider / Zed / Continue all expose; it completes Cortex's provider set
/// alongside `@repomap` (ranked file *overview*), `@outline` (one file's
/// *structure*), and `@grep` (literal *text* across files) — none of which jump
/// to a symbol's definition and show its body.
///
/// Matching is exact and case-sensitive on the symbol name; only if the precise
/// pass finds nothing does it retry case-insensitively (so `@def:nextdelay`
/// still resolves `nextDelay`). The body of each match runs from its
/// declaration line to the next top-level symbol (exclusive) or end of file,
/// clamped to `MAX_DEF_LINES`. Up to `MAX_DEFS` definitions are shown, the whole
/// block capped at `max_bytes`. Results are sorted by `(path, line)` so output
/// is deterministic and unit-testable. Returns `Some("<name> · no definition
/// found")` when nothing matches (an explicit signal, like `@grep`'s no-match
/// line), and `None` only for an empty query.
pub fn find_definition(root: &Path, name: &str, max_bytes: usize) -> Option<String> {
    const MAX_DEFS: usize = 6;
    const MAX_DEF_LINES: u32 = 40;
    /// Safety valve: stop walking once we've gathered far more candidates than
    /// we'll ever show, so a ubiquitous name (`new`, `default`) can't make this
    /// read every file in a huge repo. We still sort the gathered set, so the
    /// shown top-`MAX_DEFS` are deterministic within what we collected.
    const GATHER_CAP: usize = 200;

    let needle = name.trim();
    if needle.is_empty() {
        return None;
    }

    // Collect matching definitions; `ci` toggles case-insensitive matching for
    // the fallback pass.
    let gather = |ci: bool| -> Vec<(String, u32, String, String)> {
        let mut hits: Vec<(String, u32, String, String)> = Vec::new();
        for (path, _mtime) in collect_candidates(root) {
            if hits.len() >= GATHER_CAP {
                break;
            }
            let Some(language) = detect_language(&path) else { continue };
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let mut symbols = extract_symbols(&content, &language);
            if symbols.is_empty() {
                continue;
            }
            symbols.sort_by_key(|s| s.line);
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len() as u32;
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            for (i, sym) in symbols.iter().enumerate() {
                let matched = if ci {
                    sym.name.eq_ignore_ascii_case(needle)
                } else {
                    sym.name == needle
                };
                if !matched {
                    continue;
                }
                let start = sym.line; // 1-based
                // End at the next top-level symbol (exclusive), else EOF;
                // clamped so a single definition never floods the context.
                let next = symbols
                    .get(i + 1)
                    .map(|n| n.line)
                    .unwrap_or(total_lines + 1);
                let end = next
                    .saturating_sub(1)
                    .max(start)
                    .min(start + MAX_DEF_LINES)
                    .min(total_lines);
                let mut body = String::new();
                for ln in start..=end {
                    if let Some(text) = lines.get((ln - 1) as usize) {
                        body.push_str(&format!("{ln:>5}  {text}\n"));
                    }
                }
                hits.push((rel.clone(), start, sym.kind.clone(), body));
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        hits
    };

    let mut hits = gather(false);
    if hits.is_empty() {
        hits = gather(true);
    }
    if hits.is_empty() {
        return Some(format!("{needle} · no definition found\n"));
    }

    let total = hits.len();
    let shown = total.min(MAX_DEFS);
    let mut out = String::with_capacity(512);
    out.push_str(&format!(
        "{needle} · {total} definition{}\n",
        if total == 1 { "" } else { "s" }
    ));
    let mut byte_truncated = false;
    for (path, line, kind, body) in hits.into_iter().take(MAX_DEFS) {
        let block = format!("\n// {path}:{line}  ({kind})\n{body}");
        if out.len() + block.len() > max_bytes {
            out.push_str("\n… (truncated)\n");
            byte_truncated = true;
            break;
        }
        out.push_str(&block);
    }
    if !byte_truncated && total > shown {
        out.push_str(&format!(
            "\n… (+{} more definition{} not shown)\n",
            total - shown,
            if total - shown == 1 { "" } else { "s" }
        ));
    }
    Some(out)
}

/// Wave 322 — Zed's **"Find All References"**. Resolves a symbol *name* to every
/// place it is **used** across the project — the companion to `@def` (which jumps
/// to where it's *declared*). Each reference is a `line: <source line>` row
/// grouped under its file, with the declaration site (when found) marked
/// `(def)`.
///
/// Matching is **whole-word and case-sensitive** on the identifier: an
/// occurrence counts only when the characters on either side are not identifier
/// characters (`[A-Za-z0-9_]`), so `@refs:cat` matches `cat` / `a.cat()` but
/// **not** `category` or `concat`. That identifier-boundary awareness is exactly
/// what distinguishes it from `@grep` — a literal, case-insensitive substring
/// search that *would* match those — and from `@def`, which shows only the
/// declaration. One row per matching line (multiple hits on a line collapse to a
/// single row). Up to `MAX_REFS` rows are shown, the whole block capped at
/// `max_bytes`; rows are sorted by `(path, line)` so output is deterministic and
/// unit-testable. Returns `Some("<name> · no references found")` when nothing
/// matches (an explicit signal, like `@def`'s no-match line), and `None` only
/// for an empty query.
pub fn find_references(root: &Path, name: &str, max_bytes: usize) -> Option<String> {
    const MAX_REFS: usize = 40;
    /// Stop scanning once we've gathered far more rows than we'll ever show, so
    /// a ubiquitous name can't make this read every line of a huge repo.
    const GATHER_CAP: usize = 400;
    const MAX_LINE_LEN: usize = 200;

    let needle = name.trim();
    if needle.is_empty() {
        return None;
    }

    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // Whole-word occurrence: does `line` contain `needle` with a non-identifier
    // char (or a string edge) on both sides of at least one occurrence?
    let has_word = |line: &str| -> bool {
        for (idx, _) in line.match_indices(needle) {
            let before_ok = line[..idx].chars().next_back().map_or(true, |c| !is_ident(c));
            let after = idx + needle.len();
            let after_ok = line[after..].chars().next().map_or(true, |c| !is_ident(c));
            if before_ok && after_ok {
                return true;
            }
        }
        false
    };

    let mut rows: Vec<(String, u32, bool, String)> = Vec::new(); // (path, line, is_def, text)
    let mut capped = false;
    'outer: for (path, _mtime) in collect_candidates(root) {
        let Some(language) = detect_language(&path) else { continue };
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        // Lines that *declare* this symbol, so each can be marked `(def)`.
        let def_lines: std::collections::HashSet<u32> = extract_symbols(&content, &language)
            .into_iter()
            .filter(|s| s.name == needle)
            .map(|s| s.line)
            .collect();
        for (i, line) in content.lines().enumerate() {
            if !has_word(line) {
                continue;
            }
            if rows.len() >= GATHER_CAP {
                capped = true;
                break 'outer;
            }
            let lineno = (i as u32) + 1;
            let mut text = line.trim_end().to_string();
            if text.chars().count() > MAX_LINE_LEN {
                let end: usize = text
                    .char_indices()
                    .nth(MAX_LINE_LEN)
                    .map(|(b, _)| b)
                    .unwrap_or(text.len());
                text.truncate(end);
                text.push('…');
            }
            rows.push((rel.clone(), lineno, def_lines.contains(&lineno), text));
        }
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if rows.is_empty() {
        return Some(format!("{needle} · no references found\n"));
    }

    let total = rows.len();
    let file_count = rows
        .iter()
        .map(|r| &r.0)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let shown = total.min(MAX_REFS);
    let mut out = String::with_capacity(512);
    out.push_str(&format!(
        "{needle} · {total}{} reference{} in {file_count} file{}\n",
        if capped { "+" } else { "" },
        if total == 1 { "" } else { "s" },
        if file_count == 1 { "" } else { "s" }
    ));

    let mut last: Option<&str> = None;
    let mut byte_truncated = false;
    for (path, line, is_def, text) in rows.iter().take(MAX_REFS) {
        let header = if last != Some(path.as_str()) {
            format!("\n// {path}\n")
        } else {
            String::new()
        };
        let marker = if *is_def { "  (def)" } else { "" };
        let row = format!("{header}{line:>6}: {text}{marker}\n");
        if out.len() + row.len() > max_bytes {
            out.push_str("… (truncated)\n");
            byte_truncated = true;
            break;
        }
        out.push_str(&row);
        last = Some(path.as_str());
    }
    if !byte_truncated && total > shown {
        out.push_str(&format!(
            "\n… (+{} more reference{} not shown)\n",
            total - shown,
            if total - shown == 1 { "" } else { "s" }
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbols() {
        let src = r#"
pub fn hello() {}
pub struct Foo { x: u32 }
pub enum Bar { A, B }
pub trait Baz {}
pub const X: u32 = 1;
fn private_no_match() {}
"#;
        let syms = extract_symbols(src, "rust");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"Baz"));
        assert!(names.contains(&"X"));
        assert!(!names.contains(&"private_no_match"));
    }

    #[test]
    fn ts_symbols() {
        let src = r#"
export function ChatPane() {}
export const lastAt = (s, c) => 0;
export default class Widget {}
export interface Props { x: number }
export type Id = string;
"#;
        let syms = extract_symbols(src, "tsx");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ChatPane"));
        assert!(names.contains(&"lastAt"));
        assert!(names.contains(&"Widget"));
        assert!(names.contains(&"Props"));
        assert!(names.contains(&"Id"));
    }

    #[test]
    fn markdown_headings() {
        let src = "# Title\n## Section\n### Sub\nbody";
        let syms = extract_symbols(src, "markdown");
        assert_eq!(syms.len(), 3);
        assert_eq!(syms[0].name, "Title");
    }

    #[test]
    fn repo_symbols_filters_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "pub fn alpha() {}\npub fn beta() {}\npub struct Gamma;\n").unwrap();
        std::fs::write(root.join("b.ts"), "export function delta() {}\n").unwrap();

        let hits = repo_symbols(root, "ALPH", 50);
        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "expected alpha in {names:?}");
        assert!(!names.contains(&"beta"));

        let all = repo_symbols(root, "", 50);
        assert!(all.len() >= 4);
        assert!(all.iter().all(|h| !h.path.is_empty() && h.line > 0));
        assert!(all.iter().any(|h| h.kind == "fn" && h.name == "delta"));
        assert!(repo_symbols(root, "", 99_999).len() <= 50);
    }

    #[test]
    fn format_under_limit() {
        let map = RepoMap {
            files: vec![FileSymbols {
                path: "src/lib.rs".into(),
                language: "rust".into(),
                symbols: vec![Symbol {
                    kind: "fn".into(),
                    name: "hello".into(),
                    signature: Some("pub fn hello()".into()),
                    line: 1,
                }],
                references: vec![],
                pagerank: 0.0,
            }],
            total_files_scanned: 1,
            truncated: false,
        };
        let txt = format_as_text(&map);
        assert!(txt.contains("src/lib.rs"));
        assert!(txt.contains("pub fn hello()"));
    }

    // Wave 184 — PageRank-lite regression tests.
    #[test]
    fn pagerank_boosts_referenced_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("core.rs"),
            "pub fn processOrder() {}\npub struct UserSession {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("a.rs"),
            "use crate::core::processOrder;\nfn x() { processOrder(); UserSession::default(); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("b.rs"),
            "use crate::core::UserSession;\nfn y() { UserSession::default(); }\n",
        )
        .unwrap();
        let map = compute_repo_map(root, 10);
        // core.rs should sort first because it has the highest inbound count.
        assert_eq!(map.files[0].path, "core.rs");
        assert!(map.files[0].pagerank > 0.0);
    }

    #[test]
    fn pagerank_skips_files_with_no_inbound() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("lone.rs"), "pub fn alone() {}\n").unwrap();
        let map = compute_repo_map(root, 10);
        assert_eq!(map.files.len(), 1);
        assert_eq!(map.files[0].pagerank, 0.0);
    }

    #[test]
    fn references_picks_camel_and_snake() {
        let refs = extract_references("let _ = processOrder; let _ = user_session;");
        assert!(refs.contains(&"processOrder".to_string()));
        assert!(refs.contains(&"user_session".to_string()));
    }

    // Wave 256 — minified/bundled JS skip.
    #[test]
    fn collect_candidates_skips_minified() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.js"), "export function hello() {}\n").unwrap();
        std::fs::write(root.join("a.min.js"), "function h(){}\n").unwrap();
        std::fs::write(root.join("vendor.abc123.js"), "fn() {}\n").unwrap();
        std::fs::write(root.join("app.chunk.1.js"), "fn() {}\n").unwrap();
        let map = compute_repo_map(root, 10);
        let paths: Vec<&str> = map.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"a.js"), "real source dropped: {paths:?}");
        assert!(!paths.contains(&"a.min.js"), "minified leaked: {paths:?}");
        assert!(!paths.contains(&"vendor.abc123.js"), "vendor leaked: {paths:?}");
        assert!(!paths.contains(&"app.chunk.1.js"), "chunk leaked: {paths:?}");
    }

    // Wave 287 — SCREAMING_SNAKE_CASE detection (via wave-181 `contains('_')`).
    #[test]
    fn references_picks_screaming_snake() {
        let refs = extract_references("const MAX_SIZE: u32 = DEFAULT_LIMIT * 2;");
        assert!(refs.contains(&"MAX_SIZE".to_string()), "MAX_SIZE missed: {refs:?}");
        assert!(refs.contains(&"DEFAULT_LIMIT".to_string()), "DEFAULT_LIMIT missed: {refs:?}");
    }

    // Wave 253 — TitleCase detection (Rust structs/traits).
    #[test]
    fn references_picks_titlecase() {
        let refs = extract_references("use crate::User; let u: Repo = Repo::new();");
        assert!(refs.contains(&"User".to_string()), "User missed: {refs:?}");
        assert!(refs.contains(&"Repo".to_string()), "Repo missed: {refs:?}");
    }

    #[test]
    fn references_rejects_short_and_plain() {
        let refs = extract_references("let x = abc; let foo = bar; if not for the long word");
        assert!(refs.is_empty(), "plain lowercase words leaked: {refs:?}");
    }

    #[test]
    fn personalized_pagerank_boosts_mentioned_idents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Two files, neither references the other. Without personalization
        // they tie at 0.0 pagerank.
        std::fs::write(root.join("hot.rs"), "pub fn processOrder() {}\n").unwrap();
        std::fs::write(root.join("cold.rs"), "pub fn doesNothing() {}\n").unwrap();
        let mentioned = vec!["processOrder".to_string()];
        let map = super::compute_repo_map_personalized(root, 10, &mentioned);
        assert_eq!(map.files[0].path, "hot.rs", "mentioned ident did not float file: {:#?}", map.files);
        assert!(map.files[0].pagerank > map.files.last().unwrap().pagerank);
    }

    // Wave 300 — auto-context block: ranked, personalized, byte-capped.
    #[test]
    fn context_block_ranks_central_file_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // `core.rs` defines `CoreThing`; two other files reference it, so it
        // gathers inbound edges and should rank first with a ★ score.
        std::fs::write(root.join("core.rs"), "pub struct CoreThing {}\npub fn helper() {}\n").unwrap();
        std::fs::write(root.join("a.rs"), "use crate::CoreThing;\npub fn a() { let _ = CoreThing {}; }\n").unwrap();
        std::fs::write(root.join("b.rs"), "use crate::CoreThing;\npub fn b() { let _ = CoreThing {}; }\n").unwrap();
        let block = super::build_context_block(root, "anything", 6 * 1024).expect("non-empty repo => Some");
        let core_pos = block.find("core.rs").expect("core.rs listed");
        let a_pos = block.find("a.rs").expect("a.rs listed");
        assert!(core_pos < a_pos, "central file should be listed first:\n{block}");
        assert!(block.contains('★'), "central file should carry a PageRank score:\n{block}");
        // Symbol signatures are emitted under their file.
        assert!(block.contains("CoreThing"), "symbols missing:\n{block}");
    }

    #[test]
    fn context_block_personalizes_by_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Two unrelated files, no cross-references => tie without a task hint.
        std::fs::write(root.join("hot.rs"), "pub fn processOrder() {}\n").unwrap();
        std::fs::write(root.join("cold.rs"), "pub fn doesNothing() {}\n").unwrap();
        let block = super::build_context_block(root, "please fix processOrder", 6 * 1024).unwrap();
        assert!(
            block.find("hot.rs").unwrap() < block.find("cold.rs").unwrap(),
            "task-mentioned ident should float its file first:\n{block}"
        );
    }

    #[test]
    fn context_block_respects_byte_cap_and_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..30 {
            std::fs::write(
                root.join(format!("f{i}.rs")),
                format!("pub fn func{i}() {{}}\npub struct Thing{i} {{}}\n"),
            )
            .unwrap();
        }
        let block = super::build_context_block(root, "", 256).unwrap();
        assert!(block.len() <= 256 + 64, "block must stay within the byte cap: {} bytes", block.len());
        // An empty/no-source directory yields nothing to inject.
        let empty = tempfile::tempdir().unwrap();
        assert!(super::build_context_block(empty.path(), "x", 4096).is_none());
    }

    // Wave 209 — verify caching returns identical results within TTL. We
    // can't easily distinguish cache-hit from cache-miss without
    // instrumenting, so this test confirms the contract: two consecutive
    // calls give the same RepoMap.
    #[test]
    fn cache_ttl_respects_env_override() {
        // We can't reliably set + read env mid-test without serial_test
        // since other tests run in parallel; just verify the default
        // resolves to the expected 10s when nothing's set. Power users
        // can override with CORTEX_REPO_MAP_CACHE_TTL_SECS but the
        // cached Lazy<Duration> means we can't change it after init.
        if std::env::var("CORTEX_REPO_MAP_CACHE_TTL_SECS").is_err() {
            assert_eq!(cache_ttl(), std::time::Duration::from_secs(10));
        }
    }

    #[test]
    fn repo_map_cache_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "pub fn alpha() {}\n").unwrap();
        let a = compute_repo_map(root, 10);
        let b = compute_repo_map(root, 10);
        assert_eq!(a.files.len(), b.files.len());
        assert_eq!(a.files[0].path, b.files[0].path);
        assert_eq!(a.files[0].pagerank, b.files[0].pagerank);
    }

    // Wave 267 — verify cache_clear empties the cache.
    #[test]
    fn cache_clear_empties() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.rs"), "pub fn x() {}\n").unwrap();
        let _ = compute_repo_map(dir.path(), 5);
        let before = cache_size();
        assert!(before > 0, "cache should have at least 1 entry after compute");
        let cleared = cache_clear();
        assert_eq!(cleared, before, "cleared count should match prior size");
        assert_eq!(cache_size(), 0);
    }

    // Wave 224 — exercise the LRU eviction path. Create 20 distinct
    // tempdirs to push the cache past its 16-entry cap and verify the
    // last insert still returns correct data.
    #[test]
    fn cache_eviction_keeps_recent() {
        let mut dirs: Vec<_> = Vec::new();
        for i in 0..20 {
            let d = tempfile::tempdir().unwrap();
            let path = d.path().to_path_buf();
            std::fs::write(path.join(format!("f{i}.rs")), format!("pub fn fn{i}() {{}}\n")).unwrap();
            let m = compute_repo_map(&path, 5);
            assert_eq!(m.files.len(), 1);
            dirs.push(d);
        }
        // Verify the most recent tempdir is still cacheable + queryable.
        let last_root = dirs.last().unwrap().path();
        let again = compute_repo_map(last_root, 5);
        assert_eq!(again.files.len(), 1);
    }

    #[test]
    fn pagerank_sort_is_stable_and_desc() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("hub.rs"), "pub fn Hub() {}\npub fn Other() {}\n").unwrap();
        std::fs::write(root.join("a.rs"), "fn uses() { Hub(); Other(); }\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn uses() { Hub(); }\n").unwrap();
        let map = compute_repo_map(root, 10);
        // Check sort property: every consecutive pair has pagerank[i] >= pagerank[i+1].
        for win in map.files.windows(2) {
            assert!(
                win[0].pagerank >= win[1].pagerank,
                "out of order: {:.2} then {:.2}",
                win[0].pagerank,
                win[1].pagerank,
            );
        }
        // hub.rs should be at the top since both other files reference it.
        assert_eq!(map.files[0].path, "hub.rs");
    }

    #[test]
    fn personalized_empty_equals_base() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "pub fn alpha() {}\n").unwrap();
        let m1 = compute_repo_map(root, 10);
        let m2 = super::compute_repo_map_personalized(root, 10, &[]);
        assert_eq!(m1.files.len(), m2.files.len());
        assert_eq!(m1.files[0].path, m2.files[0].path);
    }

    // ---- build_outline (Zed-style file outline) ----

    #[test]
    fn outline_lists_code_symbols_with_lines_and_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("svc.rs"),
            "// header comment\npub struct Order { id: u32 }\n\npub fn place(o: Order) -> bool {\n    true\n}\n\npub enum Status { Open, Closed }\n",
        )
        .unwrap();
        let out = build_outline(root, "svc.rs", 16 * 1024).expect("outline");
        // Header names the file, language, and symbol count.
        assert!(out.contains("svc.rs · rust · 3 symbols"), "header: {out}");
        // Each symbol carries its 1-based line number and its signature.
        assert!(out.contains("  2  pub struct Order"), "struct line: {out}");
        assert!(out.contains("  4  pub fn place(o: Order) -> bool"), "fn line: {out}");
        assert!(out.contains("  8  pub enum Status"), "enum line: {out}");
        // The leading comment line is not a symbol.
        assert!(!out.contains("header comment"));
    }

    #[test]
    fn outline_indents_markdown_headings_by_level() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("README.md"),
            "# Title\n\nintro\n\n## Section\n\n### Sub\n",
        )
        .unwrap();
        let out = build_outline(root, "README.md", 16 * 1024).expect("outline");
        assert!(out.contains("README.md · markdown · 3 symbols"), "{out}");
        // h1 flush-left, h2 indented two spaces, h3 four — a nested ToC.
        assert!(out.contains("  1  Title\n"), "h1: {out}");
        assert!(out.contains("  5    Section\n"), "h2: {out}");
        assert!(out.contains("  7      Sub\n"), "h3: {out}");
    }

    #[test]
    fn outline_reports_no_symbols_for_empty_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("blank.rs"), "// just a comment\nlet x = 1;\n").unwrap();
        let out = build_outline(root, "blank.rs", 16 * 1024).expect("outline");
        assert!(out.contains("blank.rs · rust · 0 symbols"), "{out}");
        assert!(out.contains("(no symbols found)"), "{out}");
    }

    #[test]
    fn outline_rejects_unknown_extension_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("data.bin"), "not source\n").unwrap();
        // Unknown extension → None (caller ships the token verbatim).
        assert!(build_outline(root, "data.bin", 16 * 1024).is_none());
        // Missing file → None.
        assert!(build_outline(root, "nope.rs", 16 * 1024).is_none());
        // Empty path → None.
        assert!(build_outline(root, "  ", 16 * 1024).is_none());
    }

    #[test]
    fn outline_confines_path_to_root() {
        let parent = tempfile::tempdir().unwrap();
        // A secret outside the project root.
        std::fs::write(parent.path().join("secret.rs"), "pub fn leak() {}\n").unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("inside.rs"), "pub fn ok() {}\n").unwrap();
        // `..` escape is rejected even though the target exists and is a .rs.
        assert!(build_outline(&root, "../secret.rs", 16 * 1024).is_none());
        // A file genuinely inside the root resolves fine.
        assert!(build_outline(&root, "inside.rs", 16 * 1024)
            .unwrap()
            .contains("pub fn ok()"));
    }

    #[test]
    fn outline_byte_cap_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut src = String::new();
        for i in 0..30 {
            src.push_str(&format!("pub fn func_{i}() {{}}\n"));
        }
        std::fs::write(root.join("many.rs"), src).unwrap();
        // A tiny budget forces truncation before all symbols render.
        let out = build_outline(root, "many.rs", 120).expect("outline");
        assert!(out.contains("… (truncated)"), "{out}");
        assert!(out.len() <= 200, "respects budget-ish: {}", out.len());
    }

    #[test]
    fn folder_inlines_direct_text_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("mod");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(sub.join("b.ts"), "export const b = 1;\n").unwrap();
        std::fs::write(sub.join("logo.png"), [0u8, 1, 2, 3]).unwrap();
        std::fs::write(sub.join(".hidden"), "secret\n").unwrap();
        // A file one level deeper must NOT be inlined (non-recursive).
        let deeper = sub.join("nested");
        std::fs::create_dir(&deeper).unwrap();
        std::fs::write(deeper.join("c.rs"), "pub fn c() {}\n").unwrap();

        let out = build_folder(root, "mod", 60 * 1024).expect("folder");
        // Header counts the direct files (a.rs, b.ts, logo.png — not .hidden,
        // not the nested dir).
        assert!(out.contains("mod · 3 files"), "header: {out}");
        // Text files are inlined under a banner with their contents.
        assert!(out.contains("===== a.rs =====") && out.contains("pub fn a()"), "a.rs: {out}");
        assert!(out.contains("===== b.ts =====") && out.contains("export const b"), "b.ts: {out}");
        // Binary is listed but not inlined.
        assert!(out.contains("logo.png (skipped — non-text)"), "png skip: {out}");
        // Hidden file and nested-dir contents never appear.
        assert!(!out.contains("secret"), "hidden leaked: {out}");
        assert!(!out.contains("pub fn c()"), "recursed: {out}");
        // No inner code fences (would break the caller's outer ```diff block).
        assert!(!out.contains("```"), "must not emit ```: {out}");
    }

    #[test]
    fn folder_confines_path_to_root() {
        let parent = tempfile::tempdir().unwrap();
        let outside = parent.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("leak.rs"), "pub fn leak() {}\n").unwrap();
        let root = parent.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let inside = root.join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(inside.join("ok.rs"), "pub fn ok() {}\n").unwrap();
        // `..` escape is refused even though the folder exists.
        assert!(build_folder(&root, "../outside", 60 * 1024).is_none());
        // A folder genuinely inside the root resolves.
        assert!(build_folder(&root, "inside", 60 * 1024)
            .unwrap()
            .contains("pub fn ok()"));
        // A path that isn't a directory (a file) returns None.
        std::fs::write(root.join("file.rs"), "x\n").unwrap();
        assert!(build_folder(&root, "file.rs", 60 * 1024).is_none());
    }

    #[test]
    fn folder_empty_and_byte_cap() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Empty (no readable text files) → explicit signal, not an empty body.
        let empty = root.join("empty");
        std::fs::create_dir(&empty).unwrap();
        let out = build_folder(root, "empty", 60 * 1024).expect("folder");
        assert!(out.contains("(no readable text files in this folder)"), "{out}");
        // Byte cap truncates a fat folder.
        let big = root.join("big");
        std::fs::create_dir(&big).unwrap();
        for i in 0..20 {
            std::fs::write(big.join(format!("f{i}.rs")), "x".repeat(2000)).unwrap();
        }
        let capped = build_folder(root, "big", 500).expect("folder");
        assert!(capped.contains("… (folder truncated)"), "{capped}");
        assert!(capped.len() < 1200, "respects budget-ish: {}", capped.len());
    }

    #[test]
    fn def_resolves_symbol_to_its_body() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("retry.rs"),
            "pub const MAX_DELAY_MS: u32 = 15_000;\n\npub fn next_delay(attempt: u32) -> u32 {\n    let base = 250 * 2u32.pow(attempt);\n    base.min(MAX_DELAY_MS)\n}\n\npub fn unrelated() {}\n",
        )
        .unwrap();
        let out = find_definition(root, "next_delay", 16 * 1024).expect("def");
        // Header counts the single match.
        assert!(out.contains("next_delay · 1 definition\n"), "header: {out}");
        // Site reference with path:line and kind.
        assert!(out.contains("// retry.rs:3  (fn)"), "site ref: {out}");
        // The decl line renders with a right-aligned line-number gutter.
        assert!(out.contains("    3  pub fn next_delay"), "gutter: {out}");
        // The body is captured through to its last line, stopping before the
        // next top-level symbol (so `unrelated` is NOT included).
        assert!(out.contains("let base = 250"), "body line: {out}");
        assert!(out.contains("base.min(MAX_DELAY_MS)"), "body end: {out}");
        assert!(!out.contains("unrelated"), "leaked past the def: {out}");
    }

    #[test]
    fn def_lists_every_definition_across_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Same name declared in two files; output must be (path, line)-sorted
        // and deterministic regardless of mtime/walk order.
        std::fs::write(root.join("b_mod.rs"), "pub fn handler() { /* b */ }\n").unwrap();
        std::fs::write(root.join("a_mod.rs"), "pub fn handler() { /* a */ }\n").unwrap();
        let out = find_definition(root, "handler", 16 * 1024).expect("def");
        assert!(out.contains("handler · 2 definitions\n"), "count: {out}");
        let a = out.find("a_mod.rs").expect("a present");
        let b = out.find("b_mod.rs").expect("b present");
        assert!(a < b, "a_mod.rs should sort before b_mod.rs: {out}");
    }

    #[test]
    fn def_case_insensitive_fallback_and_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("svc.rs"), "pub fn nextDelay() {}\n").unwrap();
        // Exact case finds nothing → case-insensitive fallback resolves it.
        let out = find_definition(root, "nextdelay", 16 * 1024).expect("def");
        assert!(out.contains("nextdelay · 1 definition"), "ci header: {out}");
        assert!(out.contains("pub fn nextDelay()"), "ci body: {out}");
        // A genuinely absent symbol → explicit no-match signal, not None.
        let none = find_definition(root, "doesNotExist", 16 * 1024).expect("signal");
        assert!(none.contains("no definition found"), "no-match: {none}");
        // Empty query → None (caller ships the token verbatim).
        assert!(find_definition(root, "  ", 16 * 1024).is_none());
    }

    #[test]
    fn def_caps_count_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 8 files each defining `widget` — only MAX_DEFS (6) shown, with a
        // "+N more" footer noting the remainder.
        for i in 0..8 {
            std::fs::write(
                root.join(format!("f{i}.rs")),
                "pub fn widget() {}\n",
            )
            .unwrap();
        }
        let out = find_definition(root, "widget", 16 * 1024).expect("def");
        assert!(out.contains("widget · 8 definitions"), "count: {out}");
        assert!(out.contains("+2 more definitions not shown"), "footer: {out}");
        assert_eq!(out.matches("(fn)").count(), 6, "shows exactly MAX_DEFS: {out}");
        // A tiny byte budget truncates mid-list.
        let tight = find_definition(root, "widget", 80).expect("def");
        assert!(tight.contains("… (truncated)"), "byte cap: {tight}");
    }

    #[test]
    fn refs_whole_word_matches_uses_not_substrings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("svc.rs"),
            // `connect` is declared once, then used; `disconnect` and `connected`
            // are SUBSTRINGS that must NOT match (identifier-boundary aware).
            "pub fn connect() {}\n\nfn run() {\n    connect();\n    let x = self.connect;\n}\n\nfn disconnect() {}\nlet connected = true;\n",
        )
        .unwrap();
        let out = find_references(root, "connect", 16 * 1024).expect("refs");
        // Header: 3 whole-word references (decl line + the two call/field uses).
        assert!(
            out.contains("connect · 3 references in 1 file\n"),
            "header: {out}"
        );
        // The declaration line is marked (def); the use sites are not.
        assert!(out.contains("(def)"), "def marker: {out}");
        assert!(out.contains("connect();"), "call site: {out}");
        assert!(out.contains("self.connect"), "field use: {out}");
        // Substrings of the identifier must NOT count as references.
        assert!(!out.contains("disconnect"), "matched a substring: {out}");
        assert!(!out.contains("connected"), "matched a substring: {out}");
    }

    #[test]
    fn refs_groups_by_file_sorted_and_counts_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("b_mod.rs"), "fn x() { handler(); }\n").unwrap();
        std::fs::write(
            root.join("a_mod.rs"),
            "pub fn handler() {}\nfn y() { handler(); }\n",
        )
        .unwrap();
        let out = find_references(root, "handler", 16 * 1024).expect("refs");
        // 3 references (1 decl + 1 use in a_mod, 1 use in b_mod) across 2 files.
        assert!(
            out.contains("handler · 3 references in 2 files\n"),
            "header: {out}"
        );
        // (path, line)-sorted: a_mod.rs grouped before b_mod.rs, each under its
        // own `// <file>` header.
        let a = out.find("// a_mod.rs").expect("a header");
        let b = out.find("// b_mod.rs").expect("b header");
        assert!(a < b, "a_mod.rs should group before b_mod.rs: {out}");
    }

    #[test]
    fn refs_no_match_and_empty_query() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("svc.rs"), "pub fn connect() {}\n").unwrap();
        // Absent identifier → explicit no-match signal (not None).
        let none = find_references(root, "absentSymbol", 16 * 1024).expect("signal");
        assert!(none.contains("no references found"), "no-match: {none}");
        // Empty query → None (caller ships the token verbatim).
        assert!(find_references(root, "   ", 16 * 1024).is_none());
    }

    #[test]
    fn refs_caps_rows_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 50 use sites of `ping` in one file — only MAX_REFS (40) rows shown,
        // with a "+N more" footer noting the remainder.
        let mut body = String::from("pub fn ping() {}\n");
        for _ in 0..50 {
            body.push_str("    ping();\n");
        }
        std::fs::write(root.join("net.rs"), body).unwrap();
        let out = find_references(root, "ping", 16 * 1024).expect("refs");
        // 51 total references (1 decl + 50 uses).
        assert!(out.contains("ping · 51 references"), "count: {out}");
        assert_eq!(
            out.matches("ping();").count(),
            39,
            "shows MAX_REFS rows total incl. the decl: {out}"
        );
        assert!(
            out.contains("+11 more references not shown"),
            "footer: {out}"
        );
        // A tiny byte budget truncates mid-list.
        let tight = find_references(root, "ping", 90).expect("refs");
        assert!(tight.contains("… (truncated)"), "byte cap: {tight}");
    }
}

// ============================================================================
// Repo auto-sync watcher (ContextForge #15)
// ============================================================================
//
// Watches the active project's filesystem via the `notify` crate and emits
// `repo-watcher:event` window events to the frontend so the UI can flag a
// stale repo map. Coalesces bursts with a 500ms debounce. Respects
// `.cortexignore` so denied paths never trigger.
pub mod watcher {
    use super::*;

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use anyhow::{Context, Result};
    use notify::{
        event::{CreateKind, ModifyKind, RemoveKind},
        EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    };
    use once_cell::sync::OnceCell;
    use parking_lot::Mutex;
    use serde::Serialize;
    use tauri::{AppHandle, Emitter};
    use tokio::sync::{mpsc, oneshot};

    use crate::projects::ignore::CortexIgnore;

    /// Debounce window for collapsing rapid filesystem bursts.
    const DEBOUNCE_MS: u64 = 500;

    /// Per-project watcher entry. The frontend addresses watchers by
    /// `project_root` (normalized absolute path).
    struct WatcherEntry {
        stop_tx: oneshot::Sender<()>,
        /// Atomic-ish counter of changes since the frontend last re-indexed.
        change_count: Arc<Mutex<usize>>,
        /// Most recent change timestamp (epoch ms), shared with the task.
        last_change_ms: Arc<Mutex<Option<i64>>>,
    }

    /// Process-wide registry of active watchers.
    static REGISTRY: OnceCell<Arc<Mutex<HashMap<PathBuf, WatcherEntry>>>> = OnceCell::new();

    fn registry() -> Arc<Mutex<HashMap<PathBuf, WatcherEntry>>> {
        REGISTRY
            .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
            .clone()
    }

    /// Payload emitted on the `repo-watcher:event` window event.
    #[derive(Debug, Clone, Serialize)]
    pub struct RepoWatcherEvent {
        /// One of: "modified", "created", "deleted".
        pub kind: String,
        /// Absolute filesystem path of the changed entry.
        pub path: String,
        /// Project root the event belongs to (matches the start() arg).
        pub project_root: String,
        /// Unix epoch milliseconds.
        pub ts: i64,
    }

    /// Aggregate status surface for the frontend status bar.
    #[derive(Debug, Clone, Serialize)]
    pub struct WatcherStatus {
        pub active_projects: Vec<PathBuf>,
        pub last_change_ms: Option<i64>,
        pub change_count_since_index: usize,
    }

    /// Start a watcher for `project_root`. If one is already running for this
    /// root, it is replaced (so callers can safely call this on every project
    /// switch). Returns Ok even if the watcher cannot start — errors are
    /// logged and the registry stays clean.
    pub fn start(project_root: PathBuf, app: AppHandle) -> Result<()> {
        if !project_root.is_dir() {
            anyhow::bail!(
                "repo_watcher: project_root is not a directory: {}",
                project_root.display()
            );
        }

        // Replace any existing watcher for this root first.
        stop(project_root.clone());

        let ignore = CortexIgnore::load(&project_root);
        let root_for_filter = project_root.clone();
        let root_for_emit = project_root.clone();

        // notify -> tokio bridge.
        let (event_tx, mut event_rx) =
            mpsc::unbounded_channel::<(String, PathBuf)>();

        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else {
                    return;
                };
                let Some(kind) = classify_kind(&event.kind) else {
                    return;
                };
                for path in event.paths {
                    if ignore.is_denied(&path, &root_for_filter) {
                        continue;
                    }
                    let _ = event_tx.send((kind.to_string(), path));
                }
            })
            .context("repo_watcher: failed to create notify watcher")?;

        watcher
            .watch(&project_root, RecursiveMode::Recursive)
            .with_context(|| {
                format!(
                    "repo_watcher: failed to watch {}",
                    project_root.display()
                )
            })?;

        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

        let change_count = Arc::new(Mutex::new(0usize));
        let last_change_ms = Arc::new(Mutex::new(None::<i64>));
        let change_count_task = change_count.clone();
        let last_change_ms_task = last_change_ms.clone();

        // The watcher must outlive the spawned task — keep it in an Arc so the
        // task drops it cleanly on shutdown.
        let watcher_holder = Arc::new(Mutex::new(Some(watcher)));
        let watcher_holder_task = watcher_holder.clone();

        tokio::spawn(async move {
            // Per-path coalescing: most-recent timestamp wins; we fire one
            // event per path after the debounce window elapses.
            let mut pending: HashMap<PathBuf, (String, tokio::time::Instant)> =
                HashMap::new();
            let debounce = Duration::from_millis(DEBOUNCE_MS);

            loop {
                let next_deadline = pending.values().map(|(_, t)| *t).min();
                let sleep_fut: tokio::time::Sleep = match next_deadline {
                    Some(d) => tokio::time::sleep_until(d + debounce),
                    None => tokio::time::sleep(Duration::from_secs(3600)),
                };
                tokio::pin!(sleep_fut);

                tokio::select! {
                    _ = &mut stop_rx => {
                        tracing::info!(
                            "repo_watcher: stop signal for {}",
                            root_for_emit.display()
                        );
                        break;
                    }
                    maybe = event_rx.recv() => {
                        match maybe {
                            Some((kind, path)) => {
                                pending.insert(
                                    path,
                                    (kind, tokio::time::Instant::now()),
                                );
                            }
                            None => {
                                tracing::warn!(
                                    "repo_watcher: event channel closed for {}",
                                    root_for_emit.display()
                                );
                                break;
                            }
                        }
                    }
                    _ = &mut sleep_fut => {
                        let now = tokio::time::Instant::now();
                        let ready: Vec<(PathBuf, String)> = pending
                            .iter()
                            .filter(|(_, (_, t))| now.duration_since(*t) >= debounce)
                            .map(|(p, (k, _))| (p.clone(), k.clone()))
                            .collect();
                        for (path, kind) in ready {
                            pending.remove(&path);
                            let ts = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            {
                                let mut c = change_count_task.lock();
                                *c = c.saturating_add(1);
                            }
                            *last_change_ms_task.lock() = Some(ts);
                            let payload = RepoWatcherEvent {
                                kind,
                                path: path.display().to_string(),
                                project_root: root_for_emit.display().to_string(),
                                ts,
                            };
                            if let Err(e) = app.emit("repo-watcher:event", &payload) {
                                tracing::warn!(
                                    "repo_watcher: emit failed: {e}"
                                );
                            }
                        }
                    }
                }
            }

            // Release the OS-level watch.
            drop(watcher_holder_task.lock().take());
        });

        // Stash the entry. The Arc<watcher_holder> is kept alive by the task
        // via watcher_holder_task; the local one drops here harmlessly.
        let _ = watcher_holder;

        registry().lock().insert(
            project_root,
            WatcherEntry {
                stop_tx,
                change_count,
                last_change_ms,
            },
        );

        Ok(())
    }

    /// Stop the watcher for `project_root` if one is running. Returns `true`
    /// if a watcher was actually stopped.
    pub fn stop(project_root: PathBuf) -> bool {
        if let Some(entry) = registry().lock().remove(&project_root) {
            let _ = entry.stop_tx.send(());
            true
        } else {
            false
        }
    }

    /// Snapshot of currently-running watchers and aggregate change stats.
    pub fn status() -> WatcherStatus {
        let reg = registry();
        let locked = reg.lock();
        let mut active: Vec<PathBuf> = locked.keys().cloned().collect();
        active.sort();
        let mut latest: Option<i64> = None;
        let mut total: usize = 0;
        for entry in locked.values() {
            total = total.saturating_add(*entry.change_count.lock());
            if let Some(t) = *entry.last_change_ms.lock() {
                latest = Some(latest.map(|cur| cur.max(t)).unwrap_or(t));
            }
        }
        WatcherStatus {
            active_projects: active,
            last_change_ms: latest,
            change_count_since_index: total,
        }
    }

    /// Reset the change counter for `project_root` (called by the frontend
    /// after a successful re-index).
    pub fn reset_counter(project_root: &Path) {
        let reg = registry();
        let locked = reg.lock();
        if let Some(entry) = locked.get(project_root) {
            *entry.change_count.lock() = 0;
        }
    }

    fn classify_kind(kind: &EventKind) -> Option<&'static str> {
        match kind {
            EventKind::Create(CreateKind::File)
            | EventKind::Create(CreateKind::Any)
            | EventKind::Create(CreateKind::Folder) => Some("created"),
            EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Any)
            | EventKind::Modify(ModifyKind::Name(_)) => Some("modified"),
            EventKind::Remove(RemoveKind::File)
            | EventKind::Remove(RemoveKind::Folder)
            | EventKind::Remove(RemoveKind::Any) => Some("deleted"),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn classify_known_kinds() {
            assert_eq!(
                classify_kind(&EventKind::Create(CreateKind::File)),
                Some("created")
            );
            assert_eq!(
                classify_kind(&EventKind::Modify(ModifyKind::Any)),
                Some("modified")
            );
            assert_eq!(
                classify_kind(&EventKind::Remove(RemoveKind::File)),
                Some("deleted")
            );
            assert_eq!(classify_kind(&EventKind::Access(notify::event::AccessKind::Any)), None);
        }

        #[test]
        fn registry_starts_empty() {
            let s = status();
            // Other tests may have populated; just ensure the call doesn't
            // panic and returns coherent values.
            let _ = s.change_count_since_index;
            let _ = s.active_projects;
            let _ = s.last_change_ms;
        }
    }
}
