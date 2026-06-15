//! Tauri command surface for the PRP (Product Requirement Prompt) subsystem.
//!
//! Six commands match the actions the panel needs:
//!   * `list_prps` — populate the panel on mount.
//!   * `get_prp` — fetch one expanded PRP.
//!   * `create_prp` — write a fresh stage-1 PRP file.
//!   * `advance_prp_stage` — bump `status:` to the next stage (or an explicit one).
//!   * `run_prp_gates` — execute the 5 gates and write verdicts back to disk.
//!   * `prp_progress` — return one row per PRP for the summary list.
//!
//! All commands hop to `spawn_blocking` because the underlying loader touches
//! the filesystem (and `run_prp_gates` shells out). We keep parity with the
//! skills command surface so the same patterns apply across all "user-data
//! file" subsystems.

use std::path::PathBuf;

use crate::prp::{
    create_prp as create_prp_inner, current_progress, get_prp as get_prp_inner,
    list_prps as list_prps_inner, run_gates, update_prp_stage as update_prp_stage_inner, Prp,
    PrpProgress, PrpStage, ValidationReport,
};

fn parse_stage(stage: &str) -> Result<PrpStage, String> {
    PrpStage::from_str(stage).ok_or_else(|| format!("invalid stage '{stage}'"))
}

#[tauri::command]
pub async fn list_prps(project_root: String) -> Result<Vec<Prp>, String> {
    let root = PathBuf::from(project_root);
    tokio::task::spawn_blocking(move || list_prps_inner(&root))
        .await
        .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn get_prp(project_root: String, name: String) -> Result<Option<Prp>, String> {
    let root = PathBuf::from(project_root);
    tokio::task::spawn_blocking(move || get_prp_inner(&root, &name))
        .await
        .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn create_prp(
    project_root: String,
    name: String,
    body_hint: Option<String>,
) -> Result<Prp, String> {
    let root = PathBuf::from(project_root);
    let hint = body_hint.unwrap_or_default();
    tokio::task::spawn_blocking(move || create_prp_inner(&root, &name, &hint))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn advance_prp_stage(
    project_root: String,
    name: String,
    stage: Option<String>,
) -> Result<Prp, String> {
    let root = PathBuf::from(project_root);
    tokio::task::spawn_blocking(move || {
        let current = get_prp_inner(&root, &name)
            .ok_or_else(|| format!("PRP '{name}' not found"))?;
        let next = match stage {
            Some(s) => parse_stage(&s)?,
            None => current
                .status
                .next()
                .ok_or_else(|| "PRP already at final stage".to_string())?,
        };
        update_prp_stage_inner(&root, &name, next)?;
        get_prp_inner(&root, &name).ok_or_else(|| "failed to reload PRP".to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn run_prp_gates(
    project_root: String,
    name: String,
) -> Result<ValidationReport, String> {
    let root = PathBuf::from(project_root);
    tokio::task::spawn_blocking(move || {
        let prp = get_prp_inner(&root, &name)
            .ok_or_else(|| format!("PRP '{name}' not found"))?;
        Ok::<ValidationReport, String>(run_gates(&root, &prp))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn prp_progress(project_root: String) -> Result<Vec<PrpProgress>, String> {
    let root = PathBuf::from(project_root);
    tokio::task::spawn_blocking(move || current_progress(&root))
        .await
        .map_err(|e| format!("join error: {e}"))
}
