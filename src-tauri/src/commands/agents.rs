use crate::agents::{AgentDescriptor, ALL_CLI_SPECS};
use crate::app_state::AppState;
use crate::terminal::pty::{self, PtyHandle};
use serde::Serialize;
use tauri::State;

#[tauri::command]
pub async fn list_agents(state: State<'_, AppState>) -> Result<Vec<AgentDescriptor>, String> {
    Ok(state.registry.read().list_descriptors())
}

#[tauri::command]
pub async fn check_agent_health(agent_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let agent = state.registry.read().get(&agent_id);
    match agent {
        Some(a) => Ok(a.health_check().await),
        None => Err(format!("unknown agent: {agent_id}")),
    }
}

/// Detection + sign-in state for one local AI-maker CLI, surfaced in
/// Settings → Providers → "Local AI providers". No secret ever crosses this
/// boundary — only install/auth *presence* and the public install URL / login
/// command string.
#[derive(Debug, Clone, Serialize)]
pub struct LocalCliProvider {
    /// Registry id (`"claude-cli"`, `"codex-cli"`, …).
    pub id: &'static str,
    /// Human label for the row (`"Claude (CLI)"`).
    pub label: &'static str,
    /// One-line description.
    pub description: &'static str,
    /// Is the binary resolvable on this machine?
    pub installed: bool,
    /// `Some(true/false)` from a best-effort auth-file probe, or `None` when
    /// auth state isn't file-detectable (e.g. aider uses env API keys).
    pub authenticated: Option<bool>,
    /// Where to send a user who needs to install the CLI.
    pub install_url: &'static str,
    /// One-line install hint.
    pub install_hint: &'static str,
    /// The login command (program + args), joined with spaces for display, e.g.
    /// `"codex login"`. Empty when the CLI has no login flow (env-key auth).
    pub login_cmd: String,
    /// True when there is a runnable login command (so the UI shows "Sign in").
    pub has_login: bool,
}

/// Report every local AI-maker CLI Cortex can drive, with install + sign-in
/// status. Drives the Settings "Local AI providers" section. Pure, fast, and
/// network-free (filesystem probes only).
#[tauri::command]
pub async fn list_local_cli_providers() -> Result<Vec<LocalCliProvider>, String> {
    let mut out = Vec::with_capacity(ALL_CLI_SPECS.len());
    for spec in ALL_CLI_SPECS {
        out.push(LocalCliProvider {
            id: spec.id,
            label: spec.label,
            description: spec.description,
            installed: spec.discover().is_some(),
            authenticated: spec.authenticated(),
            install_url: spec.install_url,
            install_hint: spec.install_hint,
            login_cmd: spec.login_cmd.join(" "),
            has_login: !spec.login_cmd.is_empty(),
        });
    }
    Ok(out)
}

/// Launch a local CLI's own login flow inside Cortex, in a real PTY terminal,
/// so the user can complete the provider's OAuth / device-code / key prompt
/// without leaving the app. Returns a [`PtyHandle`] the frontend attaches an
/// xterm.js view to (same plumbing as the embedded terminal). The argv is the
/// spec's `login_cmd` verbatim — never a shell string — so nothing is
/// interpolated.
///
/// Rejects:
///   * an unknown `provider_id`,
///   * a CLI that isn't installed (nothing to log into),
///   * a CLI with no login flow (env-key auth) — the UI shows the key hint
///     instead.
#[tauri::command]
pub async fn cli_provider_login(
    app: tauri::AppHandle,
    provider_id: String,
    cols: u16,
    rows: u16,
) -> Result<PtyHandle, String> {
    let Some(spec) = ALL_CLI_SPECS.iter().find(|s| s.id == provider_id) else {
        return Err(format!("unknown provider: {provider_id}"));
    };
    if spec.discover().is_none() {
        return Err(format!(
            "`{}` is not installed. {} ({})",
            spec.tag, spec.install_hint, spec.install_url
        ));
    }
    if spec.login_cmd.is_empty() {
        return Err(format!(
            "{} has no in-app sign-in — it authenticates via your provider API key. {}",
            spec.label, spec.install_hint
        ));
    }
    let program = spec.login_cmd[0].to_string();
    let args: Vec<String> = spec.login_cmd[1..].iter().map(|s| s.to_string()).collect();
    pty::open_command(app, cols, rows, Some((program, args)))
}
