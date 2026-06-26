use crate::app_state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct GatewayConfig {
    pub base_url: String,
    pub model: String,
    pub has_api_key: bool,
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub obsidian_vault: Option<String>,
    pub git_server_url: Option<String>,
    pub git_server_cloned_path: Option<String>,
}

#[tauri::command]
pub async fn get_gateway_config(state: State<'_, AppState>) -> Result<GatewayConfig, String> {
    let cfg = state.config.read();
    Ok(GatewayConfig {
        base_url: cfg.gateway_base_url.clone(),
        model: cfg.gateway_model.clone(),
        has_api_key: AppState::get_gateway_api_key().filter(|s| !s.is_empty()).is_some(),
        ollama_base_url: cfg.ollama_base_url.clone(),
        ollama_model: cfg.ollama_model.clone(),
        obsidian_vault: cfg.obsidian_vault.as_ref().map(|p| p.display().to_string()),
        git_server_url: cfg.git_server_url.clone(),
        git_server_cloned_path: cfg
            .git_server_cloned_path
            .as_ref()
            .map(|p| p.display().to_string()),
    })
}

#[derive(Debug, Deserialize)]
pub struct SetKeyArgs {
    pub api_key: String,
}

#[tauri::command]
pub async fn set_gateway_api_key(args: SetKeyArgs) -> Result<(), String> {
    if args.api_key.trim().is_empty() {
        return Err("API key cannot be empty".into());
    }
    AppState::set_gateway_api_key(&args.api_key).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct UpdateConfigArgs {
    pub gateway_base_url: Option<String>,
    pub gateway_model: Option<String>,
    pub ollama_base_url: Option<String>,
    pub ollama_model: Option<String>,
}

#[tauri::command]
pub async fn update_gateway_config(
    args: UpdateConfigArgs,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut cfg = state.config.write();
    if let Some(v) = args.gateway_base_url { cfg.gateway_base_url = v; }
    if let Some(v) = args.gateway_model { cfg.gateway_model = v; }
    if let Some(v) = args.ollama_base_url { cfg.ollama_base_url = v; }
    if let Some(v) = args.ollama_model { cfg.ollama_model = v; }
    Ok(())
}

/// Direct-provider configuration surfaced to the Settings → Providers tab.
/// Only key *presence* is reported — secrets never cross the bridge.
#[derive(Debug, Serialize)]
pub struct ProviderConfig {
    pub anthropic_key_set: bool,
    pub openai_key_set: bool,
    /// Whether the local `claude` CLI binary is resolvable (Claude Code login).
    pub claude_cli_available: bool,
    /// `"homelab"` (Cortex Gateway) or `"cloud"` (direct providers).
    pub runtime_mode: String,
    /// Whether this binary was built with the `standalone` feature, i.e. the
    /// direct adapters are actually compiled in. The UI uses this to decide
    /// whether the cloud-mode toggle can do anything.
    pub standalone_build: bool,
    /// Default-model override the direct adapters read per run (vault entry
    /// `anthropic/default-model`). `None` = adapter built-in default. Model
    /// slugs are not secrets, so round-tripping them is fine.
    pub anthropic_default_model: Option<String>,
    /// Mirror of the above for `openai/default-model`.
    pub openai_default_model: Option<String>,
}

/// Read a non-secret vault entry, treating "missing" and "blank" the same.
fn vault_entry(provider: &str, label: &str) -> Option<String> {
    crate::commands::keyvault::lookup_key_sync(provider, label)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[tauri::command]
pub async fn get_provider_config(state: State<'_, AppState>) -> Result<ProviderConfig, String> {
    let anthropic_key_set = vault_entry("anthropic", "api-key").is_some();
    let openai_key_set = vault_entry("openai", "api-key").is_some();
    let runtime_mode = state.config.read().runtime_mode.clone();
    Ok(ProviderConfig {
        anthropic_key_set,
        openai_key_set,
        claude_cli_available: crate::agents::claude_cli::claude_bin().is_some(),
        runtime_mode,
        standalone_build: cfg!(feature = "standalone"),
        anthropic_default_model: vault_entry("anthropic", "default-model"),
        openai_default_model: vault_entry("openai", "default-model"),
    })
}

#[derive(Debug, Deserialize)]
pub struct SetProviderKeyArgs {
    /// `"anthropic"` | `"openai"`.
    pub provider: String,
    pub key: String,
}

/// Store a direct-provider API key in the encrypted key vault under
/// `(provider, "api-key")`. The direct adapters read it back on every run.
#[tauri::command]
pub async fn set_provider_key(args: SetProviderKeyArgs) -> Result<(), String> {
    let provider = args.provider.trim().to_lowercase();
    if !matches!(provider.as_str(), "anthropic" | "openai") {
        return Err(format!("unsupported provider: {provider}"));
    }
    if args.key.trim().is_empty() {
        return Err("API key cannot be empty".into());
    }
    crate::commands::keyvault::vault_set(provider, "api-key".to_string(), args.key.trim().to_string())
        .await
}

#[derive(Debug, Deserialize)]
pub struct SetProviderDefaultModelArgs {
    /// `"anthropic"` | `"openai"`.
    pub provider: String,
    /// Model slug, or blank to clear the override (adapter falls back to its
    /// env-var / built-in default).
    pub model: String,
}

/// Persist the per-provider default model to the vault entry
/// `(provider, "default-model")` — exactly the coordinates
/// `anthropic_direct.rs` / `openai_direct.rs` re-resolve on every run, so the
/// change takes effect on the next message without a restart.
#[tauri::command]
pub async fn set_provider_default_model(args: SetProviderDefaultModelArgs) -> Result<(), String> {
    let provider = args.provider.trim().to_lowercase();
    if !matches!(provider.as_str(), "anthropic" | "openai") {
        return Err(format!("unsupported provider: {provider}"));
    }
    let model = args.model.trim().to_string();
    if model.is_empty() {
        // Clearing an override that was never set is a no-op, not an error.
        match crate::commands::keyvault::vault_remove(provider, "default-model".to_string()).await
        {
            Ok(()) => Ok(()),
            Err(e) if e.starts_with("no key for") => Ok(()),
            Err(e) => Err(e),
        }
    } else {
        crate::commands::keyvault::vault_set(provider, "default-model".to_string(), model).await
    }
}

#[derive(Debug, Deserialize)]
pub struct SetRuntimeModeArgs {
    /// `"homelab"` (Cortex Gateway) | `"cloud"` (direct provider adapters).
    pub mode: String,
}

/// Persist the runtime mode chosen in Settings → Providers
/// (`~/.cortex/runtime-mode.json`) and update the in-memory config so the UI
/// reflects the choice immediately. Adapter registration happens once at
/// startup (`lib.rs`), so the new mode takes effect on the next app launch —
/// the frontend surfaces that via a restart toast.
#[tauri::command]
pub async fn set_runtime_mode(
    args: SetRuntimeModeArgs,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mode = args.mode.trim().to_lowercase();
    if !matches!(mode.as_str(), "homelab" | "cloud") {
        return Err(format!("invalid runtime mode: {mode} (expected homelab | cloud)"));
    }
    AppState::save_runtime_mode(&mode).map_err(|e| e.to_string())?;
    state.config.write().runtime_mode = mode;
    Ok(())
}

/// Outcome of a live provider-key check, shaped for inline display in the
/// Providers tab.
#[derive(Debug, Serialize)]
pub struct ProviderValidation {
    /// True when the saved key authenticated against the provider's API.
    pub ok: bool,
    /// One-line humanized outcome (success or failure).
    pub message: String,
    /// Model IDs reported by the provider's live `GET /v1/models` — feeds the
    /// default-model picker. Empty on failure.
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidateProviderKeyArgs {
    /// `"anthropic"` | `"openai"`.
    pub provider: String,
}

/// Cheap live check of the saved provider key. Both providers expose
/// `GET /v1/models` (Anthropic with `x-api-key` + `anthropic-version`
/// headers, OpenAI with a Bearer token) — it authenticates the key without
/// burning any completion tokens and returns the live model catalog as a
/// bonus. Failures come back as `ok: false` with a humanized message rather
/// than `Err`, so the UI renders them inline instead of as a thrown error.
#[tauri::command]
pub async fn validate_provider_key(
    args: ValidateProviderKeyArgs,
) -> Result<ProviderValidation, String> {
    let provider = args.provider.trim().to_lowercase();
    let (display, url): (&str, &str) = match provider.as_str() {
        "anthropic" => ("Anthropic", "https://api.anthropic.com/v1/models"),
        "openai" => ("OpenAI", "https://api.openai.com/v1/models"),
        other => return Err(format!("unsupported provider: {other}")),
    };

    let Some(key) = vault_entry(&provider, "api-key") else {
        return Ok(ProviderValidation {
            ok: false,
            message: format!(
                "No {display} API key saved yet — enter one above and save it first."
            ),
            models: vec![],
        });
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let req = if provider == "anthropic" {
        client
            .get(url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
    } else {
        client.get(url).bearer_auth(&key)
    };

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Ok(ProviderValidation {
                ok: false,
                message: format!(
                    "Could not reach the {display} API ({e}). Check your network connection and try again."
                ),
                models: vec![],
            })
        }
    };

    let status = resp.status();
    if status.is_success() {
        let body = resp.json::<serde_json::Value>().await.unwrap_or_default();
        let models = parse_model_ids(&body);
        return Ok(ProviderValidation {
            ok: true,
            message: format!(
                "Key verified — the {display} API answered with {} models.",
                models.len()
            ),
            models,
        });
    }

    let message = match status.as_u16() {
        401 => format!(
            "{display} rejected the key (401 unauthorized). It may be mistyped, revoked, or belong to a different account — re-enter it above."
        ),
        403 => format!(
            "The key authenticated but lacks permission (403). Check its workspace and scopes in the {display} console."
        ),
        429 => format!(
            "{display} rate-limited the check (429). The key authenticated — try again in a minute."
        ),
        s if s >= 500 => format!(
            "The {display} API is having trouble right now ({status}). This is not a problem with your key — try again later."
        ),
        _ => format!("The {display} API returned an unexpected {status} for the key check."),
    };
    // 429 means auth succeeded before the limiter kicked in.
    Ok(ProviderValidation {
        ok: status.as_u16() == 429,
        message,
        models: vec![],
    })
}

/// Pull sorted model IDs out of a `GET /v1/models` response body. Both
/// Anthropic and OpenAI use the same `{"data": [{"id": ...}, …]}` envelope.
fn parse_model_ids(body: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = body
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::parse_model_ids;
    use serde_json::json;

    #[test]
    fn parses_model_envelope() {
        let body = json!({
            "data": [
                {"id": "gpt-4o", "object": "model"},
                {"id": "claude-opus-4-8", "object": "model"},
                {"id": "gpt-4o"},
                {"object": "model"}
            ]
        });
        assert_eq!(
            parse_model_ids(&body),
            vec!["claude-opus-4-8".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn tolerates_missing_or_malformed_data() {
        assert!(parse_model_ids(&json!({})).is_empty());
        assert!(parse_model_ids(&json!({"data": "nope"})).is_empty());
        assert!(parse_model_ids(&json!(null)).is_empty());
    }
}
