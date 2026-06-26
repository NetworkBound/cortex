//! PRP (Product Requirement Prompt) subsystem.
//!
//! A PRP is a staged feature spec stored at `<project_root>/.cortex/prps/<name>.md`
//! as a Markdown file with YAML frontmatter declaring stage progress and gate
//! statuses, plus a body with Goal / Gotchas / Curated docs / Acceptance sections.
//!
//! The flow is:
//!   1. `loader::load_prps` walks the prps directory and returns every parseable
//!      record. Bad files are skipped (never panic — the panel stays usable).
//!   2. `loader::create_prp` writes a fresh stage-1 template, validating the
//!      slug so we never resolve `..` into a sibling directory.
//!   3. `loader::update_prp_stage` advances `status:` in the frontmatter
//!      (last-write-wins — the file is small enough that we don't bother with
//!      locking).
//!   4. `validator::run_gates` executes the 5 gates (syntax / tests / coverage /
//!      build / security) best-effort against the project root, returning a
//!      `pass | fail | skipped` verdict + message per gate.
//!   5. `progress::current_progress` returns one row per PRP showing current
//!      stage + per-gate statuses for the activity-panel summary view.
//!
//! All filesystem I/O happens on `spawn_blocking` from the command surface so
//! the Tauri main thread never stalls on a slow `cargo check`.

pub mod loader;
pub mod progress;
pub mod validator;

pub use loader::{
    create_prp, get_prp, list_prps, load_prps, prps_dir_for, update_prp_stage, GateStatuses, Prp,
    PrpStage,
};
pub use progress::{current_progress, PrpProgress};
pub use validator::{run_gates, GateResult, GateVerdict, ValidationReport};
