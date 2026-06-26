//! Tauri commands backing `<project>/.cortex/profiles/*.toml`.
//!
//! `list_profiles` is a pure on-disk read. `apply_profile` additionally
//! mutates `AppState::config` so subsequent chat requests pick up the new
//! model / sandbox tier / reasoning effort. We return the loaded `Profile`
//! so the frontend can render "now using <name>" without re-querying.

use std::path::PathBuf;

use tauri::{Emitter, State};

use crate::app_state::AppState;
use crate::orchestrator::{self, ApprovalRules, Decision, Profile, SandboxTier};

fn project_root_from(arg: &str) -> Result<PathBuf, String> {
    if arg.trim().is_empty() {
        return Err("project_root is required".into());
    }
    let p = PathBuf::from(arg);
    if !p.exists() || !p.is_dir() {
        return Err(format!("not a directory: {arg}"));
    }
    Ok(p)
}

/// List every profile found under `<project_root>/.cortex/profiles/`.
/// Returns an empty vec when the dir is missing — never an error.
#[tauri::command]
pub async fn list_profiles(project_root: String) -> Result<Vec<Profile>, String> {
    let root = project_root_from(&project_root)?;
    Ok(orchestrator::list_profiles(&root))
}

/// Load `<project_root>/.cortex/profiles/<name>.toml` and apply its
/// model / sandbox_tier / reasoning_effort to `AppState::config`. Returns
/// the loaded profile so the UI can confirm what it just applied.
///
/// Missing fields on the profile leave the existing config value alone —
/// a "scratch" profile that only flips `sandbox_tier` doesn't wipe the
/// model selection.
#[tauri::command]
pub async fn apply_profile(
    project_root: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<Profile, String> {
    let root = project_root_from(&project_root)?;
    let profile = orchestrator::load_profile(&root, &name)
        .ok_or_else(|| format!("profile '{name}' not found or malformed"))?;

    {
        let mut cfg = state.config.write();
        if let Some(model) = profile.model.as_ref() {
            cfg.gateway_model = model.clone();
        }
        if let Some(tier) = profile.sandbox_tier.as_ref() {
            cfg.sandbox_tier = Some(tier.clone());
        }
        if let Some(effort) = profile.reasoning_effort.as_ref() {
            cfg.reasoning_effort = Some(effort.clone());
        }
        cfg.active_profile = Some(profile.name.clone());
    }

    tracing::info!("applied profile '{}'", profile.name);
    Ok(profile)
}

// ── Per-agent custom instructions ───────────────────────────────────────────
//
// Thin wrappers over `orchestrator::profiles::{get,set}_agent_instructions`.
// Storage lives at `~/.cortex/agent-instructions.json`. See module docs on
// the orchestrator side for the on-disk schema.

fn validate_agent_id(agent_id: &str) -> Result<(), String> {
    if agent_id.trim().is_empty() {
        return Err("agent_id is required".into());
    }
    Ok(())
}

/// Returns the saved custom instructions for an agent, or `None` if unset.
/// Empty strings on disk are normalized to `None`.
#[tauri::command]
pub async fn get_agent_instructions(agent_id: String) -> Result<Option<String>, String> {
    validate_agent_id(&agent_id)?;
    Ok(orchestrator::get_agent_instructions(&agent_id))
}

/// Persist custom instructions for an agent. Passing an empty / blank `text`
/// removes the entry. Returns the trimmed value (or empty string after a
/// remove) so the UI can confirm what landed on disk.
#[tauri::command]
pub async fn set_agent_instructions(
    agent_id: String,
    text: String,
) -> Result<String, String> {
    validate_agent_id(&agent_id)?;
    orchestrator::set_agent_instructions(&agent_id, &text).map_err(|e| e.to_string())
}

// ── Plan-mode: approve_plan ────────────────────────────────────────────────
//
// When the agent emits a structured `tool: "plan"` message, the UI renders a
// PlanCard with an "Approve plan" CTA. Clicking it calls `approve_plan`,
// which emits a `plan_approved` event on the session channel — the
// orchestrator (or whatever plan-aware adapter is listening) picks that up
// to advance from plan-iteration into act mode.
//
// We deliberately keep this dumb: no state mutation, no agent invocation
// here. The orchestrator owns the workflow; we just fan out the user's
// approval.

#[derive(Debug, serde::Deserialize)]
pub struct ApprovePlanArgs {
    pub session_id: String,
    pub plan_id: String,
}

#[tauri::command]
pub async fn approve_plan(
    args: ApprovePlanArgs,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if args.session_id.trim().is_empty() {
        return Err("session_id is required".into());
    }
    if args.plan_id.trim().is_empty() {
        return Err("plan_id is required".into());
    }
    let payload = serde_json::json!({
        "type": "plan_approved",
        "plan_id": args.plan_id,
        "session_id": args.session_id,
    });
    app.emit(&format!("agent-event:{}", args.session_id), payload)
        .map_err(|e| format!("emit plan_approved: {e}"))?;
    tracing::info!(
        session_id = %args.session_id,
        plan_id = %args.plan_id,
        "plan approved",
    );
    Ok(())
}

// ── Codex #10 — profile bundling (v2) ──────────────────────────────────────
//
// `apply_profile` only mutates AppState::config. The v2 variant bundles all
// four dimensions a profile can flip:
//
//   1. `model`            → AppState::config.gateway_model
//   2. `sandbox_mode`     → `.cortex/sandbox.toml` via `write_tier`
//   3. `approval_policy`  → `.cortex/approvals.toml` via `append_rule`
//   4. `reasoning_effort` → AppState::config.reasoning_effort
//
// Each dimension reports independently — a missing field is skipped, and a
// per-step failure is captured in `errors` so partial-success ("model + tier
// applied, approval write failed") is visible to the UI instead of swallowed.
//
// We deliberately re-read the on-disk Profile here instead of recycling the
// v1 command's return value: the v1 command applies model/tier/effort to
// AppState only, and we want v2 to *also* persist sandbox + approvals to
// disk so they survive a relaunch.

/// Mirrors the v1 toml schema plus the `sandbox_mode` / `approval_policy`
/// fields that profile bundling exposes. The existing `Profile` struct uses
/// `sandbox_tier`; we accept either spelling so v1 profiles keep loading.
#[derive(Debug, serde::Deserialize, Default)]
struct ProfileV2File {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    sandbox_tier: Option<String>,
    #[serde(default)]
    sandbox_mode: Option<String>,
    #[serde(default)]
    approval_policy: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ProfileApplyResult {
    /// Names of dimensions that were successfully applied
    /// (e.g. `["model", "sandbox_mode", "reasoning_effort"]`).
    pub applied: Vec<String>,
    /// Per-dimension failure messages. Empty on full success.
    pub errors: Vec<String>,
    /// The resolved profile name (filename stem if `name` was omitted).
    pub name: String,
}

/// Bundle-apply a profile: model, sandbox mode, approval policy, reasoning
/// effort. Each dimension is independent — a failure in one doesn't roll back
/// the others. Returns a per-dimension report so the UI can show partial
/// success.
#[tauri::command]
pub async fn apply_profile_v2(
    project_root: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<ProfileApplyResult, String> {
    let root = project_root_from(&project_root)?;
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!("invalid profile name '{name}'"));
    }

    let path = root.join(".cortex").join("profiles").join(format!("{name}.toml"));
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("profile '{name}' not found: {e}"))?;
    let file: ProfileV2File =
        toml::from_str(&raw).map_err(|e| format!("profile '{name}' malformed: {e}"))?;

    let resolved_name = file.name.clone().unwrap_or_else(|| name.clone());
    let mut applied: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    // 1. Model — straight into AppState::config.
    if let Some(model) = file.model.as_ref() {
        state.config.write().gateway_model = model.clone();
        applied.push("model".into());
    }

    // 2. Sandbox — accept `sandbox_mode` (v2) and fall back to `sandbox_tier`
    //    (v1) for backward-compat. Persist via the existing `write_tier` and
    //    mirror into AppState so chat picks it up without a reload.
    let sandbox_raw = file.sandbox_mode.as_deref().or(file.sandbox_tier.as_deref());
    if let Some(tier_str) = sandbox_raw {
        match SandboxTier::parse(tier_str) {
            Some(tier) => match orchestrator::write_tier(&root, tier) {
                Ok(()) => {
                    state.config.write().sandbox_tier = Some(tier.as_str().to_string());
                    applied.push("sandbox_mode".into());
                }
                Err(e) => errors.push(format!("sandbox_mode: {e}")),
            },
            None => errors.push(format!("sandbox_mode: invalid value '{tier_str}'")),
        }
    }

    // 3. Approval policy — persisted to `.cortex/approvals.toml` so it
    //    survives restart. We map the high-level keyword onto the storage
    //    pattern `add_approval_rule` already uses.
    //
    //    "always"     → catch-all approve rule
    //    "never"      → catch-all deny rule
    //    "on-failure" → no persisted rule (the default policy already
    //                   prompts only on hard failures; we just record it as
    //                   applied so the UI confirms the choice).
    //    "untrusted"  → approve the safe set (read_*/list_*), prompt on
    //                   everything else. We approximate with one rule
    //                   covering safe reads.
    if let Some(policy) = file.approval_policy.as_ref() {
        match policy.as_str() {
            "always" => match ApprovalRules::append_rule(&root, ".*", Decision::Approve) {
                Ok(()) => applied.push("approval_policy".into()),
                Err(e) => errors.push(format!("approval_policy: {e}")),
            },
            "never" => match ApprovalRules::append_rule(&root, ".*", Decision::Deny) {
                Ok(()) => applied.push("approval_policy".into()),
                Err(e) => errors.push(format!("approval_policy: {e}")),
            },
            "on-failure" | "on_failure" => {
                applied.push("approval_policy".into());
            }
            "untrusted" => match ApprovalRules::append_rule(
                &root,
                "^(read_file|list_files|grep|ripgrep) ",
                Decision::Approve,
            ) {
                Ok(()) => applied.push("approval_policy".into()),
                Err(e) => errors.push(format!("approval_policy: {e}")),
            },
            other => errors.push(format!(
                "approval_policy: unknown value '{other}' (use always|never|on-failure|untrusted)"
            )),
        }
    }

    // 4. Reasoning effort — validated against the same allow-list the v1
    //    loader uses so a typo doesn't silently apply garbage.
    if let Some(effort) = file.reasoning_effort.as_ref() {
        if orchestrator::profiles::is_valid_reasoning_effort(effort) {
            state.config.write().reasoning_effort = Some(effort.clone());
            applied.push("reasoning_effort".into());
        } else {
            errors.push(format!(
                "reasoning_effort: invalid value '{effort}' (use low|medium|high)"
            ));
        }
    }

    // Track the active profile name regardless of which dimensions applied.
    state.config.write().active_profile = Some(resolved_name.clone());

    tracing::info!(
        profile = %resolved_name,
        applied = ?applied,
        errors = ?errors,
        "apply_profile_v2",
    );

    Ok(ProfileApplyResult {
        applied,
        errors,
        name: resolved_name,
    })
}
