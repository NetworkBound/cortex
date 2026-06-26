//! Git worktree management for parallel non-clashing agent sessions.
//!
//! Each worktree is a separate working tree of the same repository under
//! `<project_root>/../.cortex-worktrees/<id>/`. The agent gets that path
//! as its cwd, so two sessions can work on the same project concurrently
//! without stomping on each other.

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub id: String,
    pub project_root: String,
    pub branch: String,
    pub path: String,
    pub session_id: Option<String>,
    pub created_at: i64,
    pub archived_at: Option<i64>,
    pub status: String, // "active" | "archived"
    pub notes: Option<String>,
}

#[derive(Clone)]
pub struct WorktreeStore {
    conn: Arc<Mutex<Connection>>,
}

impl WorktreeStore {
    /// Share the existing tracing-store sqlite connection. This module's
    /// schema is part of `observability/schema.sql` so the table already exists.
    pub fn new(shared_conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn: shared_conn }
    }

    pub fn list_active(&self, project_root: Option<&str>) -> anyhow::Result<Vec<Worktree>> {
        let conn = self.conn.lock();
        let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match project_root {
            Some(p) => (
                "SELECT id, project_root, branch, path, session_id, created_at, archived_at, status, notes
                 FROM worktrees WHERE project_root = ?1 AND status = 'active'
                 ORDER BY created_at DESC",
                vec![Box::new(p.to_string())],
            ),
            None => (
                "SELECT id, project_root, branch, path, session_id, created_at, archived_at, status, notes
                 FROM worktrees WHERE status = 'active'
                 ORDER BY created_at DESC",
                vec![],
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let row_to_wt = |r: &rusqlite::Row| -> rusqlite::Result<Worktree> {
            Ok(Worktree {
                id: r.get(0)?,
                project_root: r.get(1)?,
                branch: r.get(2)?,
                path: r.get(3)?,
                session_id: r.get(4)?,
                created_at: r.get(5)?,
                archived_at: r.get(6)?,
                status: r.get(7)?,
                notes: r.get(8)?,
            })
        };
        let rows: Vec<Worktree> = if project_root.is_some() {
            stmt.query_map(rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())), row_to_wt)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map([], row_to_wt)?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<Worktree>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT id, project_root, branch, path, session_id, created_at, archived_at, status, notes
             FROM worktrees WHERE id = ?1",
            params![id],
            |r| {
                Ok(Worktree {
                    id: r.get(0)?,
                    project_root: r.get(1)?,
                    branch: r.get(2)?,
                    path: r.get(3)?,
                    session_id: r.get(4)?,
                    created_at: r.get(5)?,
                    archived_at: r.get(6)?,
                    status: r.get(7)?,
                    notes: r.get(8)?,
                })
            },
        );
        match row {
            Ok(w) => Ok(Some(w)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn insert(&self, w: &Worktree) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO worktrees (id, project_root, branch, path, session_id, created_at, archived_at, status, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 'active', ?7)",
            params![w.id, w.project_root, w.branch, w.path, w.session_id, w.created_at, w.notes],
        )?;
        Ok(())
    }

    pub fn mark_archived(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE worktrees SET status = 'archived', archived_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn assign_session(&self, id: &str, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE worktrees SET session_id = ?1 WHERE id = ?2",
            params![session_id, id],
        )?;
        Ok(())
    }
}

/// Create a new git worktree off the project's HEAD.
/// Returns the new worktree's metadata.
pub fn create_worktree(
    store: &WorktreeStore,
    project_root: &Path,
    note: Option<String>,
) -> anyhow::Result<Worktree> {
    /// Maximum length (in bytes) for a persisted worktree note.
    const MAX_NOTE_LEN: usize = 4096;
    if let Some(n) = note.as_ref() {
        if n.len() > MAX_NOTE_LEN {
            anyhow::bail!("worktree note too long: {} bytes (max {MAX_NOTE_LEN})", n.len());
        }
    }
    if !project_root.exists() {
        anyhow::bail!("project root does not exist: {}", project_root.display());
    }
    let dot_git = project_root.join(".git");
    if !dot_git.exists() {
        anyhow::bail!("not a git repository: {}", project_root.display());
    }

    let id = ulid::Ulid::new().to_string().to_lowercase();
    let short_id: String = id.chars().take(10).collect();
    let branch = format!("cortex/{}", short_id);
    let path = project_root
        .parent()
        .map(|p| p.join(".cortex-worktrees").join(&id))
        .ok_or_else(|| anyhow::anyhow!("no parent dir for project root"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // git worktree add -b <branch> <path>
    let output = crate::sys::no_window("git")
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(&path)
        .output()
        .map_err(|e| anyhow::anyhow!("git not found or failed to spawn: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }

    let wt = Worktree {
        id: id.clone(),
        project_root: project_root.display().to_string(),
        branch: branch.clone(),
        path: path.display().to_string(),
        session_id: None,
        created_at: chrono::Utc::now().timestamp_millis(),
        archived_at: None,
        status: "active".to_string(),
        notes: note,
    };
    store.insert(&wt)?;
    Ok(wt)
}

/// Remove a worktree: optionally commit WIP first, then `git worktree remove`,
/// then mark archived in the store.
pub fn remove_worktree(
    store: &WorktreeStore,
    id: &str,
    archive_commit: bool,
) -> anyhow::Result<()> {
    let wt = store
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("worktree not found: {id}"))?;
    let project_root = PathBuf::from(&wt.project_root);
    let path = PathBuf::from(&wt.path);

    if archive_commit && path.exists() {
        // Commit any WIP changes to the cortex/ branch so they aren't lost.
        let _ = crate::sys::no_window("git").arg("-C").arg(&path).arg("add").arg("-A").output();
        let _ = crate::sys::no_window("git")
            .arg("-C")
            .arg(&path)
            .arg("-c")
            .arg("user.name=cortex")
            .arg("-c")
            .arg("user.email=cortex@local")
            .arg("commit")
            .arg("-m")
            .arg(format!("cortex: archive worktree {id}"))
            .arg("--allow-empty")
            .output();
    }

    // git worktree remove --force <path>
    let _ = crate::sys::no_window("git")
        .arg("-C")
        .arg(&project_root)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(&wt.path)
        .output();

    store.mark_archived(id)?;
    Ok(())
}
