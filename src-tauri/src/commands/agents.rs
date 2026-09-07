use crate::agents::{AgentDescriptor, ALL_CLI_SPECS};
use crate::app_state::AppState;
use crate::commands::keyvault;
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
        Some(a) => {
            let healthy = a.health_check().await;
            // Record so `orchestrator::adapter_available` can consult this
            // as a reachability override the next time something explicitly
            // picks/`@`-mentions/default-routes to this adapter — see
            // `Registry::health_cache`'s doc comment for why this exists.
            state.registry.read().record_health(&agent_id, healthy);
            Ok(healthy)
        }
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

/// Configuration state for one OpenAI-compatible per-token API provider
/// (Groq, Together, Fireworks, DeepSeek, Mistral, xAI, Perplexity,
/// OpenRouter, DashScope, Moonshot, Cohere, Gemini-API, Llama-API — the 13
/// in `agents::openai_compat::PROVIDERS`). No secret ever crosses this
/// boundary — only whether a key is currently stored, mirroring
/// `LocalCliProvider`'s "presence, not the secret itself" contract.
#[derive(Debug, Clone, Serialize)]
pub struct OpenAiCompatProvider {
    /// Stable adapter id (registry key, KeyVault provider, model-prefix).
    pub id: &'static str,
    /// Human label for the row.
    pub label: &'static str,
    /// Base URL shown so the user can sanity-check which endpoint a key
    /// activates (these are fixed per provider, not user-editable).
    pub base_url: &'static str,
    /// Is a key currently stored in the vault under `<id>/api-key`? The
    /// adapter itself falls back to the documented env var
    /// (`api_key_env`) when the vault has nothing — surfaced here too so
    /// the row doesn't claim "not configured" when an env var is actually
    /// what's authenticating it.
    pub key_set: bool,
    /// The documented env var this provider's adapter also checks, so the
    /// row can explain why it might already be "configured" via env even
    /// with nothing in the vault.
    pub api_key_env: &'static str,
}

/// Pure mapping: `PROVIDERS` × vault metadata → `OpenAiCompatProvider` rows.
/// Pulled out of the command specifically so this logic has a direct unit
/// test without touching the real on-disk vault (`vault_list()` reads
/// `~/.cortex/keys.enc` — a `cargo test` must never depend on, or risk
/// perturbing, that real file).
fn build_openai_compat_rows(vault_entries: &[keyvault::KeyMetadata]) -> Vec<OpenAiCompatProvider> {
    crate::agents::PROVIDERS
        .iter()
        .map(|p| {
            let key_set = vault_entries
                .iter()
                .any(|e| e.provider == p.id && e.label == "api-key");
            OpenAiCompatProvider {
                id: p.id,
                label: p.label,
                base_url: p.base_url,
                key_set,
                api_key_env: p.api_key_env,
            }
        })
        .collect()
}

/// Report every OpenAI-compatible per-token provider with its current
/// vault-key presence. Drives the Settings "API providers" section. One
/// `vault_list()` call shared across all 13 rows rather than 13 round
/// trips.
#[tauri::command]
pub async fn list_openai_compat_providers() -> Result<Vec<OpenAiCompatProvider>, String> {
    // A vault read error must NOT be masked as "13 × not configured" — that
    // fake-honest state is exactly how the undecryptable-vault bug hid. A
    // missing vault file still reads as an empty list; a decrypt/keyring
    // failure surfaces to the UI's error banner instead.
    let vault_entries = keyvault::vault_list().await?;
    Ok(build_openai_compat_rows(&vault_entries))
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

#[cfg(test)]
mod openai_compat_rows_tests {
    use super::*;

    fn entry(provider: &str, label: &str) -> keyvault::KeyMetadata {
        keyvault::KeyMetadata { provider: provider.into(), label: label.into(), added_unix_ms: 0 }
    }

    #[test]
    fn covers_all_13_providers_with_no_key_set_when_vault_empty() {
        let rows = build_openai_compat_rows(&[]);
        assert_eq!(rows.len(), 13, "must cover every PROVIDERS entry");
        assert!(rows.iter().all(|r| !r.key_set), "empty vault → nothing configured");
        // Every row carries real, non-empty metadata — no blank pills.
        for r in &rows {
            assert!(!r.id.is_empty());
            assert!(!r.label.is_empty());
            assert!(!r.base_url.is_empty());
            assert!(!r.api_key_env.is_empty());
        }
    }

    #[test]
    fn marks_key_set_only_for_matching_provider_and_api_key_label() {
        let vault = vec![entry("groq", "api-key")];
        let rows = build_openai_compat_rows(&vault);
        let groq = rows.iter().find(|r| r.id == "groq").unwrap();
        assert!(groq.key_set);
        // Every other provider stays unconfigured.
        assert!(rows.iter().filter(|r| r.id != "groq").all(|r| !r.key_set));
    }

    /// A vault entry for the right provider under the WRONG label (e.g. a
    /// `default-model` override, not the key itself) must not read as
    /// "key saved" — that would be a fake-ready status.
    #[test]
    fn wrong_label_does_not_count_as_key_set() {
        let vault = vec![entry("groq", "default-model")];
        let rows = build_openai_compat_rows(&vault);
        let groq = rows.iter().find(|r| r.id == "groq").unwrap();
        assert!(!groq.key_set);
    }

    #[test]
    fn provider_ids_match_the_documented_13() {
        let rows = build_openai_compat_rows(&[]);
        let ids: std::collections::HashSet<&str> = rows.iter().map(|r| r.id).collect();
        for expected in [
            "groq", "together", "fireworks", "deepseek", "mistral", "xai", "perplexity",
            "openrouter", "dashscope", "moonshot", "cohere", "gemini-api", "llama-api",
        ] {
            assert!(ids.contains(expected), "missing provider: {expected}");
        }
    }
}
