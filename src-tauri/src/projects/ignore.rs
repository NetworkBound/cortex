//! `.cortexignore` parser + matcher — gitignore-syntax repo-local deny-list.
//! Lifted from Cline's `.clineignore` (and similar `.codexignore` /
//! `.cursorignore`). Layered on top of the always-on hardcoded denylist
//! (`.git`, `node_modules`, `target`, `dist`).
//!
//! Layered lookup order (later wins):
//!   1. Built-in always-deny list (see `BUILTIN_DENY`).
//!   2. `~/.cortex/cortexignore` (global, optional).
//!   3. `<project>/.cortexignore` (per-project).
//!
//! Patterns follow gitignore semantics via the `globset` crate: `*.log`,
//! `secrets/`, `**/.env*`, etc.

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;

/// File and directory names that are ALWAYS denied regardless of
/// `.cortexignore` — these never make it into the file tree, repo map, or
/// agent-visible context. Matches by exact basename.
pub const BUILTIN_DENY: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".next",
    ".turbo",
    ".vite",
    ".cache",
    "__pycache__",
    ".venv",
    "venv",
];

/// Compiled deny-list with both basename and full-path matchers. Use
/// `is_denied(path, root)` to check a candidate path.
#[derive(Debug, Clone)]
pub struct CortexIgnore {
    /// Patterns relative to the project root (gitignore-style).
    relative: GlobSet,
    /// Did we successfully load at least one user pattern? Useful for the UI
    /// "X patterns active" pill.
    pub user_pattern_count: usize,
}

impl Default for CortexIgnore {
    fn default() -> Self {
        Self {
            relative: GlobSetBuilder::new()
                .build()
                .expect("empty globset never fails"),
            user_pattern_count: 0,
        }
    }
}

impl CortexIgnore {
    /// Load and merge global + per-project `.cortexignore` files. Returns an
    /// empty ignore if neither file exists or both are unreadable.
    pub fn load(project_root: &Path) -> Self {
        let mut builder = GlobSetBuilder::new();
        let mut count = 0usize;

        // Global ignore at ~/.cortex/cortexignore.
        if let Some(home) = dirs::home_dir() {
            let global = home.join(".cortex").join("cortexignore");
            count += merge_file(&mut builder, &global);
        }

        // Per-project ignore.
        let project_ignore = project_root.join(".cortexignore");
        count += merge_file(&mut builder, &project_ignore);

        let relative = builder.build().unwrap_or_else(|_| {
            // Bad pattern — fall back to empty so we don't ALL-deny the
            // workspace on a typo.
            GlobSetBuilder::new().build().expect("empty ok")
        });
        Self {
            relative,
            user_pattern_count: count,
        }
    }

    /// Is this path denied? Checked against (a) the always-on basename list
    /// and (b) the merged user patterns. `path` may be absolute (then trimmed
    /// to `project_root`) or already relative.
    pub fn is_denied(&self, path: &Path, project_root: &Path) -> bool {
        // User patterns matched against the path relative to project_root.
        let rel = path.strip_prefix(project_root).unwrap_or(path);
        // Always-on denylist: deny if ANY path component (relative to root) is
        // a built-in deny name, so files nested inside e.g. `target/` or
        // `node_modules/` are denied too — not just the leaf basename.
        if rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .any(|name| BUILTIN_DENY.contains(&name))
        {
            return true;
        }
        if self.relative.is_match(rel) {
            return true;
        }
        // Also try with a leading `/` form — gitignore commonly anchors with `/`.
        let with_slash = format!("/{}", rel.display());
        if self.relative.is_match(with_slash) {
            return true;
        }
        false
    }

    /// Returns true if at least one user pattern was loaded.
    pub fn has_user_patterns(&self) -> bool {
        self.user_pattern_count > 0
    }
}

/// Append every non-blank, non-comment line from `path` into `builder`.
/// Returns the number of patterns added. Best-effort: bad globs are skipped
/// silently rather than poisoning the whole set.
fn merge_file(builder: &mut GlobSetBuilder, path: &Path) -> usize {
    let Ok(body) = std::fs::read_to_string(path) else {
        return 0;
    };
    let mut added = 0usize;
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Trailing-slash directory marker — treat `foo/` as `foo/**` too.
        let mut variants = vec![line.to_string()];
        if let Some(stripped) = line.strip_suffix('/') {
            variants.push(format!("{stripped}/**"));
        }
        // Bare names like `secrets` should also match `secrets/**`.
        if !line.contains('/') && !line.contains('*') {
            variants.push(format!("{line}/**"));
        }
        // Count the source line as a single user pattern (only if at least one
        // of its compiled glob variants is valid), not each compiled variant.
        let mut any_valid = false;
        for v in variants {
            if let Ok(g) = Glob::new(&v) {
                builder.add(g);
                any_valid = true;
            }
        }
        if any_valid {
            added += 1;
        }
    }
    added
}
