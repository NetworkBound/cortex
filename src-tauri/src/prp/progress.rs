//! Per-PRP progress summary — one row per PRP showing its current stage and
//! the verdict of every gate. The frontend renders this as a compact list at
//! the top of the PRP panel before the user expands a specific PRP.
//!
//! Computed on-demand from the on-disk frontmatter — we never cache here so
//! external tooling (e.g. CI-generated gate updates) shows up in the UI on the
//! next refresh.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::prp::loader::{load_prps, GateStatuses, PrpStage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrpProgress {
    pub name: String,
    pub status: PrpStage,
    pub created_unix_ms: i64,
    pub gates: GateStatuses,
    /// Convenience: how many gates have a non-`pending`, non-`skipped` verdict.
    pub gates_resolved: u32,
    /// Convenience: how many gates passed.
    pub gates_passed: u32,
}

pub fn current_progress(project_root: &Path) -> Vec<PrpProgress> {
    load_prps(project_root)
        .into_iter()
        .map(|p| {
            let mut resolved = 0u32;
            let mut passed = 0u32;
            for v in p.gates.values() {
                match v.as_str() {
                    "pass" => {
                        resolved += 1;
                        passed += 1;
                    }
                    "fail" => resolved += 1,
                    _ => {}
                }
            }
            PrpProgress {
                name: p.name,
                status: p.status,
                created_unix_ms: p.created_unix_ms,
                gates: p.gates,
                gates_resolved: resolved,
                gates_passed: passed,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prp::loader::{create_prp, update_prp_gates};
    use tempfile::tempdir;

    #[test]
    fn progress_counts_resolved_and_passed() {
        let dir = tempdir().unwrap();
        create_prp(dir.path(), "foo", "x").unwrap();
        let mut g = GateStatuses::new();
        g.insert("syntax".into(), "pass".into());
        g.insert("tests".into(), "fail".into());
        g.insert("coverage".into(), "skipped".into());
        g.insert("build".into(), "pass".into());
        g.insert("security".into(), "pending".into());
        update_prp_gates(dir.path(), "foo", g).unwrap();

        let rows = current_progress(dir.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gates_resolved, 3);
        assert_eq!(rows[0].gates_passed, 2);
    }

    #[test]
    fn progress_empty_when_no_prps() {
        let dir = tempdir().unwrap();
        assert!(current_progress(dir.path()).is_empty());
    }
}
