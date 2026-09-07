//! Git history viewer + source control panel backend.
//!
//! Two thin shell-out modules over the local `git` CLI. Nothing here keeps
//! state — callers pass `project_root` explicitly and we just spawn `git`.
//!
//! - [`history`] — `git log` walker capped at 200 commits.
//! - [`working`] — `git status --porcelain` + stage/unstage/discard/commit.
//!
//! Errors are intentionally lossy: when `git` isn't installed or the project
//! isn't a repo we surface an empty result rather than a hard error, so the UI
//! degrades gracefully (the panel just shows "no commits"/"clean tree").

pub mod history;
pub mod working;

pub use history::{commit_file_diff, commit_files, history, show_commit, Commit, CommitFile};
pub use working::{
    commit_staged, discard_changes, file_diff, stage_file, unstage_file, working_status, DiffMode,
    FileEntry, WorkingStatus,
};
