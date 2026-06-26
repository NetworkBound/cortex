//! Tauri commands backing `~/.cortex/roles/*.yaml` — pre-built agent personas.
//!
//! Personas (a.k.a. "roles") are pure data. The only side-effect command in
//! here is `apply_role_to_agent`, which translates a role's `system_prompt`
//! into the existing per-agent custom-instructions mechanism so the rest of
//! the chat pipeline picks it up automatically.

use crate::agents::roles::{self, Role};
use crate::orchestrator;

/// List every role under `~/.cortex/roles/*.yaml`, sorted by name. Missing
/// directory yields `[]` — never an error.
#[tauri::command]
pub async fn list_roles() -> Result<Vec<Role>, String> {
    Ok(roles::list_roles())
}

/// Load a single role by filename stem.
#[tauri::command]
pub async fn get_role(name: String) -> Result<Role, String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    roles::get_role(&name).ok_or_else(|| format!("role '{name}' not found"))
}

/// Create or update a role on disk. Returns the role as persisted.
#[tauri::command]
pub async fn set_role(role: Role) -> Result<Role, String> {
    roles::set_role(&role).map_err(|e| e.to_string())?;
    Ok(role)
}

/// Delete a role file. Missing files are a no-op.
#[tauri::command]
pub async fn delete_role(name: String) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    roles::delete_role(&name).map_err(|e| e.to_string())
}

/// Apply a role's `system_prompt` to `agent_id` by writing it into the
/// per-agent custom instructions store. The chat pipeline already composes
/// those into the outgoing system prompt — no other plumbing required.
///
/// Returns the trimmed prompt that landed on disk so the UI can confirm.
#[tauri::command]
pub async fn apply_role_to_agent(
    role_name: String,
    agent_id: String,
) -> Result<String, String> {
    if role_name.trim().is_empty() {
        return Err("role_name is required".into());
    }
    if agent_id.trim().is_empty() {
        return Err("agent_id is required".into());
    }
    let role = roles::get_role(&role_name)
        .ok_or_else(|| format!("role '{role_name}' not found"))?;
    let prompt = role.system_prompt.unwrap_or_default();
    orchestrator::set_agent_instructions(&agent_id, &prompt).map_err(|e| e.to_string())
}
