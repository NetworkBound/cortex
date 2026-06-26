//! Persisted multi-provider lane runs (P0-FINAL "Lanes: stop fire-and-forget").
//!
//! `run_provider_lanes` used to return gateway run ids and forget them — the
//! run list lived in component state and died on tab switch. Every dispatched
//! lane is now a `lane_runs` row (same sqlite file as the tracing store), and
//! a per-lane watcher follows the gateway SSE stream, folding events into the
//! row via [`lane_transition`] so the UI can render live status from
//! `list_lane_runs` + the `lanes:updated` event alone.

use crate::gateway::client::RunStreamItem;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Statuses a lane can settle into. Once a lane reaches one of these, no
/// later (out-of-order / post-stop) stream event may overwrite it.
pub const TERMINAL_STATUSES: [&str; 4] = ["done", "error", "stopped", "interrupted"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneRunRecord {
    pub run_id: String,
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub task: String,
    /// gateway-side worktree branch (`cortex/<run>/<provider>`); `None` for
    /// lanes that failed to start (there is no run, hence no branch).
    pub branch: Option<String>,
    /// running | done | error | stopped | interrupted
    pub status: String,
    /// Last humanized progress line (tool activity, status, error text).
    pub detail: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// When the lane branch was merged into the project's default branch via
    /// the in-app review ("merge winner"). `None` = not merged from Cortex.
    #[serde(default)]
    pub merged_at: Option<i64>,
}

#[derive(Clone)]
pub struct LaneStore {
    conn: Arc<Mutex<Connection>>,
}

impl LaneStore {
    /// Share the tracing-store sqlite connection; the `lane_runs` table is
    /// part of `observability/schema.sql`.
    pub fn new(shared_conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn: shared_conn }
    }

    pub fn insert(&self, r: &LaneRunRecord) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO lane_runs (run_id, provider, owner, repo, task, branch, status, detail, created_at, updated_at, merged_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                r.run_id, r.provider, r.owner, r.repo, r.task, r.branch, r.status, r.detail,
                r.created_at, r.updated_at, r.merged_at
            ],
        )?;
        Ok(())
    }

    /// Newest first. `limit` defaults to 100 — the pane shows recent history,
    /// not an unbounded archive.
    pub fn list(&self, limit: Option<u32>) -> anyhow::Result<Vec<LaneRunRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT run_id, provider, owner, repo, task, branch, status, detail, created_at, updated_at, merged_at
             FROM lane_runs ORDER BY created_at DESC, run_id LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit.unwrap_or(100)], |r| {
                Ok(LaneRunRecord {
                    run_id: r.get(0)?,
                    provider: r.get(1)?,
                    owner: r.get(2)?,
                    repo: r.get(3)?,
                    task: r.get(4)?,
                    branch: r.get(5)?,
                    status: r.get(6)?,
                    detail: r.get(7)?,
                    created_at: r.get(8)?,
                    updated_at: r.get(9)?,
                    merged_at: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get(&self, run_id: &str) -> anyhow::Result<Option<LaneRunRecord>> {
        Ok(self.list(Some(u32::MAX))?.into_iter().find(|r| r.run_id == run_id))
    }

    /// Fold a status transition into the row. Terminal statuses win: once a
    /// lane is done/error/stopped/interrupted, late or out-of-order events
    /// (a straggling `Status` after a stop, the watcher's stream-ended
    /// fallback after `Done`) must not resurrect it. Returns whether a row
    /// actually changed, so callers only emit `lanes:updated` on real change.
    pub fn update_status(
        &self,
        run_id: &str,
        status: &str,
        detail: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE lane_runs SET status = ?1, detail = COALESCE(?2, detail), updated_at = ?3
             WHERE run_id = ?4 AND status NOT IN ('done', 'error', 'stopped', 'interrupted')",
            params![status, detail, now, run_id],
        )?;
        Ok(n > 0)
    }

    /// Stamp a lane merged from the in-app review. Unlike [`update_status`]
    /// this is allowed on settled (terminal) rows — merging happens AFTER a
    /// lane is done — and never touches `status`.
    pub fn mark_merged(&self, run_id: &str, detail: &str) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE lane_runs SET merged_at = ?1, detail = ?2, updated_at = ?1
             WHERE run_id = ?3",
            params![now, detail, run_id],
        )?;
        Ok(n > 0)
    }

    /// Replace the humanized detail line without touching `status` — used for
    /// progress/outcome notes on settled rows (e.g. a failed reattach on an
    /// `interrupted` lane), which the terminal-wins guard in
    /// [`update_status`] would otherwise drop.
    pub fn set_detail(&self, run_id: &str, detail: &str) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE lane_runs SET detail = ?1, updated_at = ?2 WHERE run_id = ?3",
            params![detail, now, run_id],
        )?;
        Ok(n > 0)
    }

    /// Flip an `interrupted` lane back to `running` — the ONE sanctioned exit
    /// from a terminal status, taken only after a reattached event stream has
    /// actually delivered an event (the run is provably live on the gateway again).
    pub fn reattach_to_running(&self, run_id: &str) -> anyhow::Result<bool> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE lane_runs SET status = 'running',
                    detail = 'reattached to the live event stream',
                    updated_at = ?1
             WHERE run_id = ?2 AND status = 'interrupted'",
            params![now, run_id],
        )?;
        Ok(n > 0)
    }

    pub fn delete(&self, run_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM lane_runs WHERE run_id = ?1", params![run_id])?;
        Ok(())
    }

    /// Startup sweep: any lane still `running` belonged to a previous app
    /// session — its watcher died with that process, so the row would show
    /// "running" forever. The gateway may well have finished the work (the branch
    /// is still there); we just can't stream it anymore, so say exactly that.
    pub fn mark_stale_interrupted(&self) -> anyhow::Result<usize> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE lane_runs SET status = 'interrupted',
                    detail = 'Cortex closed while this lane was running — check the branch on the gateway.',
                    updated_at = ?1
             WHERE status = 'running'",
            params![now],
        )?;
        Ok(n)
    }
}

/// Map one gateway stream event to a lane status transition. `None` means the
/// event carries no lane-level progress (token deltas, reasoning, raw noise)
/// and the row is left untouched — the watcher persists transitions, not the
/// transcript.
pub fn lane_transition(item: &RunStreamItem) -> Option<(String, Option<String>)> {
    match item {
        RunStreamItem::Started { .. }
        | RunStreamItem::Delta(_)
        | RunStreamItem::Reasoning(_)
        | RunStreamItem::Raw(_) => None,
        RunStreamItem::ToolStarted { tool, .. } => {
            Some(("running".into(), Some(format!("running {tool}…"))))
        }
        RunStreamItem::ToolCompleted { tool, error, .. } => Some((
            "running".into(),
            Some(if *error {
                format!("{tool} failed — agent continuing")
            } else {
                format!("{tool} finished")
            }),
        )),
        RunStreamItem::ApprovalRequest { tool, .. } => Some((
            "running".into(),
            Some(match tool {
                Some(t) => format!("waiting for approval on the gateway ({t})"),
                None => "waiting for approval on the gateway".into(),
            }),
        )),
        RunStreamItem::ApprovalResponded { choice } => {
            Some(("running".into(), Some(format!("approval: {choice}"))))
        }
        RunStreamItem::Status(s) if !s.trim().is_empty() => {
            Some(("running".into(), Some(s.trim().to_string())))
        }
        RunStreamItem::Status(_) => None,
        RunStreamItem::Done => Some(("done".into(), Some("completed".into()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> LaneStore {
        let conn = Connection::open_in_memory().expect("in-mem sqlite");
        conn.execute_batch(include_str!("observability/schema.sql")).expect("schema");
        LaneStore::new(Arc::new(Mutex::new(conn)))
    }

    fn rec(run_id: &str, status: &str, created_at: i64) -> LaneRunRecord {
        LaneRunRecord {
            run_id: run_id.into(),
            provider: "claude".into(),
            owner: "NetworkBound".into(),
            repo: "cortex".into(),
            task: "do the thing".into(),
            branch: Some(format!("cortex/{run_id}/claude")),
            status: status.into(),
            detail: None,
            created_at,
            updated_at: created_at,
            merged_at: None,
        }
    }

    #[test]
    fn insert_then_list_newest_first() {
        let s = store();
        s.insert(&rec("r1", "running", 100)).unwrap();
        s.insert(&rec("r2", "running", 200)).unwrap();
        let rows = s.list(None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].run_id, "r2");
        assert_eq!(rows[1].run_id, "r1");
        assert_eq!(rows[0].branch.as_deref(), Some("cortex/r2/claude"));
    }

    #[test]
    fn list_respects_limit() {
        let s = store();
        for i in 0..5 {
            s.insert(&rec(&format!("r{i}"), "running", i)).unwrap();
        }
        assert_eq!(s.list(Some(2)).unwrap().len(), 2);
    }

    #[test]
    fn update_status_changes_running_rows() {
        let s = store();
        s.insert(&rec("r1", "running", 100)).unwrap();
        assert!(s.update_status("r1", "done", Some("completed")).unwrap());
        let row = s.get("r1").unwrap().unwrap();
        assert_eq!(row.status, "done");
        assert_eq!(row.detail.as_deref(), Some("completed"));
        assert!(row.updated_at >= 100);
    }

    #[test]
    fn update_status_keeps_detail_when_none() {
        let s = store();
        let mut r = rec("r1", "running", 100);
        r.detail = Some("running bash…".into());
        s.insert(&r).unwrap();
        assert!(s.update_status("r1", "running", None).unwrap());
        assert_eq!(s.get("r1").unwrap().unwrap().detail.as_deref(), Some("running bash…"));
    }

    #[test]
    fn terminal_status_wins_over_late_events() {
        let s = store();
        s.insert(&rec("r1", "running", 100)).unwrap();
        assert!(s.update_status("r1", "stopped", Some("stopped from Cortex")).unwrap());
        // A straggling Status/Done after the stop must not resurrect the lane.
        assert!(!s.update_status("r1", "running", Some("late status")).unwrap());
        assert!(!s.update_status("r1", "done", Some("late done")).unwrap());
        let row = s.get("r1").unwrap().unwrap();
        assert_eq!(row.status, "stopped");
        assert_eq!(row.detail.as_deref(), Some("stopped from Cortex"));
    }

    #[test]
    fn update_status_missing_row_is_noop() {
        let s = store();
        assert!(!s.update_status("nope", "done", None).unwrap());
    }

    #[test]
    fn mark_merged_stamps_terminal_rows() {
        let s = store();
        s.insert(&rec("r1", "done", 100)).unwrap();
        assert!(s.mark_merged("r1", "Merged into master (PR #3)").unwrap());
        let row = s.get("r1").unwrap().unwrap();
        assert_eq!(row.status, "done", "merge must not change status");
        assert!(row.merged_at.is_some());
        assert_eq!(row.detail.as_deref(), Some("Merged into master (PR #3)"));
        assert!(!s.mark_merged("nope", "x").unwrap());
    }

    #[test]
    fn set_detail_updates_terminal_rows() {
        let s = store();
        s.insert(&rec("r1", "interrupted", 100)).unwrap();
        assert!(s.set_detail("r1", "The gateway is no longer streaming this run").unwrap());
        let row = s.get("r1").unwrap().unwrap();
        assert_eq!(row.status, "interrupted");
        assert_eq!(row.detail.as_deref(), Some("The gateway is no longer streaming this run"));
        assert!(!s.set_detail("nope", "x").unwrap());
    }

    #[test]
    fn reattach_only_flips_interrupted() {
        let s = store();
        s.insert(&rec("int", "interrupted", 100)).unwrap();
        s.insert(&rec("fin", "done", 100)).unwrap();
        s.insert(&rec("err", "error", 100)).unwrap();
        assert!(s.reattach_to_running("int").unwrap());
        assert_eq!(s.get("int").unwrap().unwrap().status, "running");
        assert!(!s.reattach_to_running("fin").unwrap());
        assert!(!s.reattach_to_running("err").unwrap());
        assert_eq!(s.get("fin").unwrap().unwrap().status, "done");
    }

    #[test]
    fn delete_removes_row() {
        let s = store();
        s.insert(&rec("r1", "done", 100)).unwrap();
        s.delete("r1").unwrap();
        assert!(s.get("r1").unwrap().is_none());
    }

    #[test]
    fn stale_sweep_only_touches_running() {
        let s = store();
        s.insert(&rec("live", "running", 100)).unwrap();
        s.insert(&rec("finished", "done", 100)).unwrap();
        s.insert(&rec("failed", "error", 100)).unwrap();
        assert_eq!(s.mark_stale_interrupted().unwrap(), 1);
        assert_eq!(s.get("live").unwrap().unwrap().status, "interrupted");
        assert_eq!(s.get("finished").unwrap().unwrap().status, "done");
        assert_eq!(s.get("failed").unwrap().unwrap().status, "error");
    }

    #[test]
    fn transitions_ignore_transcript_noise() {
        for item in [
            RunStreamItem::Started { run_id: "r".into() },
            RunStreamItem::Delta("tok".into()),
            RunStreamItem::Reasoning("hmm".into()),
            RunStreamItem::Raw(serde_json::json!({"event": "weird"})),
            RunStreamItem::Status("  ".into()),
        ] {
            assert!(lane_transition(&item).is_none(), "{item:?} should not transition");
        }
    }

    #[test]
    fn transitions_map_progress_and_done() {
        let (st, d) = lane_transition(&RunStreamItem::ToolStarted {
            tool: "bash".into(),
            preview: None,
        })
        .unwrap();
        assert_eq!(st, "running");
        assert_eq!(d.as_deref(), Some("running bash…"));

        let (st, d) = lane_transition(&RunStreamItem::ToolCompleted {
            tool: "bash".into(),
            duration_s: 1.0,
            error: true,
        })
        .unwrap();
        assert_eq!(st, "running");
        assert_eq!(d.as_deref(), Some("bash failed — agent continuing"));

        let (st, d) = lane_transition(&RunStreamItem::Status("planning".into())).unwrap();
        assert_eq!(st, "running");
        assert_eq!(d.as_deref(), Some("planning"));

        let (st, _) = lane_transition(&RunStreamItem::Done).unwrap();
        assert_eq!(st, "done");
    }

    #[test]
    fn worktrees_table_exists_in_schema() {
        // Regression guard for the missing-DDL bug: WorktreeStore has always
        // queried this table; schema.sql must actually declare it.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("observability/schema.sql")).unwrap();
        conn.prepare("SELECT id FROM worktrees LIMIT 1").expect("worktrees table declared");
    }
}
