use crate::agents::Registry;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const KEYRING_SERVICE: &str = "com.networkbound.cortex";
const KEYRING_USER_GATEWAY_KEY: &str = "gateway_backend_api_key";
/// Legacy keychain entry read as a back-compat fallback so a key stored before
/// the Cortex Gateway rebrand keeps authenticating with zero manual migration.
const KEYRING_USER_HERMES_KEY_LEGACY: &str = "hermes_backend_api_key";

/// Cortex Gateway backend API key fallback. Resolution order across the whole
/// app is: OS keychain → runtime `CORTEX_GATEWAY_API_KEY` env (legacy
/// `HERMES_API_KEY` honored) → compile-time `CORTEX_GATEWAY_API_KEY` /
/// `HERMES_API_KEY` → none.
///
/// NEVER hardcode the key as a string literal here — the repo has leaked
/// secrets before, so the value must live only in the build/launch
/// environment, never in committed source.
///
/// To make a *downloaded* build self-connect on a fresh machine with no setup,
/// build the release with the key in the environment:
///     CORTEX_GATEWAY_API_KEY=<key> pnpm tauri build
/// `option_env!` then bakes it into the binary at compile time. Source stays
/// secret-free. CAVEAT: an embedded key is recoverable from the binary via
/// `strings` — only distribute such a build to trusted users, and rotate the
/// gateway key if a build ever escapes.
fn baked_gateway_api_key() -> Option<String> {
    // 1. Runtime env (launcher script / systemd unit / .desktop Exec) wins so a
    //    key can be swapped without rebuilding. The legacy `HERMES_API_KEY`
    //    name is still honored so an existing launcher keeps working.
    if let Some(k) = std::env::var("CORTEX_GATEWAY_API_KEY")
        .or_else(|_| std::env::var("HERMES_API_KEY"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    // 2. Compile-time embed for distributable downloads (see doc comment).
    //    Back-compat: fall back to a build embedded under the old env name.
    option_env!("CORTEX_GATEWAY_API_KEY")
        .or(option_env!("HERMES_API_KEY"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<Registry>>,
    pub config: Arc<RwLock<Config>>,
}

#[derive(Clone, Default)]
pub struct Config {
    /// Cortex Gateway base URL. Resolved via `CORTEX_GATEWAY_BASE_URL` /
    /// `GATEWAY_BASE_URL` (legacy `HERMES_BASE_URL`) env →
    /// `~/.cortex/infra.json` (`gateway_base_url`, legacy `hermes_base_url`) →
    /// empty string. Empty means "not configured": the gateway adapter reports
    /// unavailable and surfaces a humanized setup hint instead of dialing.
    pub gateway_base_url: String,
    pub gateway_model: String,
    pub default_project_root: Option<PathBuf>,
    /// Ollama server base URL. Resolved via `OLLAMA_BASE_URL` env →
    /// `~/.cortex/infra.json` (`ollama_base_url`) → empty string. Empty means
    /// "not configured" and the Ollama adapter reports unavailable.
    pub ollama_base_url: String,
    pub ollama_model: String,
    pub obsidian_vault: Option<PathBuf>,
    /// Active sandbox tier — `read-only` | `workspace-write` |
    /// `danger-full-access`. `None` means "default" (gateway decides).
    pub sandbox_tier: Option<String>,
    /// Active reasoning effort — `low` | `medium` | `high`. `None` means
    /// "use the gateway/model default".
    pub reasoning_effort: Option<String>,
    /// Active profile name, if any was applied via `apply_profile`.
    pub active_profile: Option<String>,
    /// Git server (Gitea/GitHub) URL the user connected via the setup wizard.
    pub git_server_url: Option<String>,
    /// Local path of a repo cloned/connected via the setup wizard.
    pub git_server_cloned_path: Option<PathBuf>,
    /// Adapter registration mode: `"homelab"` (default) routes through the
    /// Cortex Gateway; `"cloud"` registers the direct provider adapters
    /// instead (only effective in the `standalone` build variant). Sourced
    /// from `CORTEX_RUNTIME_MODE` at startup.
    pub runtime_mode: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(Registry::new())),
            config: Arc::new(RwLock::new(Config::default())),
        }
    }

    pub fn load_config_from_env() -> Config {
        Config {
            // No baked-in default endpoint: shipped builds carry no homelab
            // addresses. Resolution is env → ~/.cortex/infra.json → empty
            // ("not configured", handled gracefully downstream).
            gateway_base_url: crate::infra_config::gateway_base_url().unwrap_or_default(),
            // Legacy `HERMES_MODEL` env still honored for back-compat.
            gateway_model: std::env::var("CORTEX_GATEWAY_MODEL")
                .or_else(|_| std::env::var("HERMES_MODEL"))
                .unwrap_or_else(|_| "gateway-agent".to_string()),
            default_project_root: std::env::var("CORTEX_DEFAULT_PROJECT")
                .ok()
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .or_else(Self::load_default_project_root)
                .or_else(|| dirs::home_dir().map(|h| h.join("projects"))),
            ollama_base_url: crate::infra_config::ollama_base_url().unwrap_or_default(),
            ollama_model: std::env::var("OLLAMA_MODEL")
                .unwrap_or_else(|_| "qwen2.5:14b".to_string()),
            obsidian_vault: std::env::var("OBSIDIAN_VAULT")
                .ok()
                .map(PathBuf::from)
                .or_else(default_obsidian_vault),
            sandbox_tier: std::env::var("CORTEX_SANDBOX_TIER").ok(),
            reasoning_effort: std::env::var("CORTEX_REASONING_EFFORT").ok(),
            active_profile: None,
            // Env wins (matches gateway behavior), then the persisted
            // ~/.cortex/git-config.json from the setup wizard.
            git_server_url: std::env::var("GIT_SERVER_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(Self::load_git_server_url),
            git_server_cloned_path: std::env::var("GIT_SERVER_CLONED_PATH")
                .ok()
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .or_else(Self::load_git_server_cloned_path),
            runtime_mode: Self::get_runtime_mode(),
        }
    }

    /// Active adapter registration mode — `"homelab"` (default) or `"cloud"`.
    ///
    /// Resolution order:
    ///   1. The persisted in-app choice (`~/.cortex/runtime-mode.json`, written
    ///      by Settings → Providers). When present it wins, so a stranger can
    ///      switch modes without ever touching env vars.
    ///   2. The `CORTEX_RUNTIME_MODE` env var (legacy launcher-script path).
    ///   3. `"homelab"`. Anything other than an exact `"cloud"` resolves to
    ///      `"homelab"` so a typo can never accidentally drop the gateway.
    pub fn get_runtime_mode() -> String {
        if let Some(mode) = Self::load_runtime_mode() {
            return mode;
        }
        match std::env::var("CORTEX_RUNTIME_MODE")
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str()
        {
            "cloud" => "cloud".to_string(),
            _ => "homelab".to_string(),
        }
    }

    /// Persist the in-app runtime-mode choice to `~/.cortex/runtime-mode.json`
    /// (read back by [`Self::get_runtime_mode`] at the next startup — adapter
    /// registration happens once in `lib.rs`, so a restart applies the switch).
    /// Mirrors the `last-project.json` pattern. Only exact `"homelab"` /
    /// `"cloud"` values are accepted.
    pub fn save_runtime_mode(mode: &str) -> anyhow::Result<()> {
        if !matches!(mode, "homelab" | "cloud") {
            anyhow::bail!("invalid runtime mode: {mode} (expected homelab | cloud)");
        }
        let cfg_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join(".cortex");
        std::fs::create_dir_all(&cfg_dir)?;
        let json = serde_json::json!({ "runtime_mode": mode });
        std::fs::write(
            cfg_dir.join("runtime-mode.json"),
            serde_json::to_vec_pretty(&json)?,
        )?;
        Ok(())
    }

    /// Read the persisted runtime mode, ignoring missing/malformed files and
    /// any value that isn't an exact `"homelab"` / `"cloud"`.
    pub fn load_runtime_mode() -> Option<String> {
        let path = dirs::home_dir()?.join(".cortex/runtime-mode.json");
        let raw = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        match v.get("runtime_mode")?.as_str()? {
            m @ ("homelab" | "cloud") => Some(m.to_string()),
            _ => None,
        }
    }

    /// Ordered, de-duplicated list of Cortex Gateway base URLs to try, most
    /// preferred first. Powers the resilient failover resolver
    /// ([`crate::connectivity`]) so Cortex can reach the gateway from any
    /// network.
    ///
    /// Sources, in order:
    ///   1. `CORTEX_GATEWAY_BASE_URLS` / `GATEWAY_BASE_URLS` (legacy
    ///      `HERMES_BASE_URLS` honored) — comma/whitespace-separated, ordered
    ///      by preference (e.g. `LAN IP, tailnet MagicDNS, public tunnel`).
    ///   2. Fallback: a single-element list from the single-URL env /
    ///      `~/.cortex/infra.json` resolution, preserving the historical
    ///      single-endpoint behavior. Empty when nothing is configured.
    ///
    /// Entries are trimmed, have trailing slashes removed, empties dropped, and
    /// duplicates collapsed while keeping first-seen order. The first element is
    /// guaranteed to equal the value `load_config_from_env` puts in
    /// `Config.gateway_base_url`, so synchronous startup behavior is unchanged.
    pub fn gateway_base_url_candidates() -> Vec<String> {
        // Legacy `HERMES_BASE_URLS` read as a back-compat fallback.
        let raw_list = std::env::var("CORTEX_GATEWAY_BASE_URLS")
            .or_else(|_| std::env::var("GATEWAY_BASE_URLS"))
            .or_else(|_| std::env::var("HERMES_BASE_URLS"))
            .unwrap_or_default();
        let mut parsed: Vec<String> = raw_list
            .split([',', ' ', '\t', '\n', '\r'])
            .map(|s| s.trim().trim_end_matches('/'))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        // Fall back to the single-URL resolution (env → ~/.cortex/infra.json)
        // when the list var is unset or contained nothing usable. When neither
        // is configured the list is empty and the failover loop is a no-op.
        if parsed.is_empty() {
            if let Some(single) = crate::infra_config::gateway_base_url() {
                parsed.push(single);
            }
        }

        // De-dupe while preserving first-seen (preference) order.
        let mut seen = std::collections::HashSet::new();
        parsed.retain(|u| seen.insert(u.clone()));
        parsed
    }

    pub fn get_gateway_api_key() -> Option<String> {
        // Order: OS keychain (new entry, then legacy entry) → env → None.
        let read_entry = |user: &str| {
            keyring::Entry::new(KEYRING_SERVICE, user)
                .ok()
                .and_then(|e| e.get_password().ok())
                .filter(|s| !s.trim().is_empty())
        };
        // Back-compat: a key saved under the pre-rebrand `hermes_backend_api_key`
        // entry keeps working until the user re-saves under the new entry.
        if let Some(k) = read_entry(KEYRING_USER_GATEWAY_KEY).or_else(|| read_entry(KEYRING_USER_HERMES_KEY_LEGACY)) {
            return Some(k);
        }
        baked_gateway_api_key()
    }

    pub fn set_gateway_api_key(key: &str) -> anyhow::Result<()> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER_GATEWAY_KEY)?
            .set_password(key)?;
        Ok(())
    }

    /// Persist the user's selected project root to `~/.cortex/last-project.json`
    /// so the choice survives app restarts. Returns the absolute path that was
    /// saved (after canonicalization fallback to the input on failure).
    pub fn save_default_project_root(root: &std::path::Path) -> anyhow::Result<()> {
        let cfg_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join(".cortex");
        std::fs::create_dir_all(&cfg_dir)?;
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let json = serde_json::json!({ "root": canonical.to_string_lossy() });
        std::fs::write(cfg_dir.join("last-project.json"), serde_json::to_vec_pretty(&json)?)?;
        Ok(())
    }

    /// Read the persisted active project root, ignoring missing/malformed files
    /// and entries that no longer exist on disk.
    pub fn load_default_project_root() -> Option<PathBuf> {
        let path = dirs::home_dir()?.join(".cortex/last-project.json");
        let raw = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let root = v.get("root")?.as_str()?;
        let p = PathBuf::from(root);
        if p.is_dir() { Some(p) } else { None }
    }

    /// Persist the setup-wizard git-server config to
    /// `~/.cortex/git-config.json`. Either argument may be `None` to leave the
    /// existing persisted value untouched (so the URL-only and path-only flows
    /// don't clobber each other). Mirrors the `last-project.json` pattern.
    pub fn save_git_server(url: Option<&str>, cloned_path: Option<&Path>) -> anyhow::Result<()> {
        let cfg_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join(".cortex");
        std::fs::create_dir_all(&cfg_dir)?;
        let file = cfg_dir.join("git-config.json");

        // Merge over any existing file so a partial update preserves the rest.
        let existing: serde_json::Value = std::fs::read_to_string(&file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut out = existing.as_object().cloned().unwrap_or_default();
        if let Some(u) = url {
            out.insert(
                "git_server_url".into(),
                serde_json::Value::String(u.to_string()),
            );
        }
        if let Some(p) = cloned_path {
            out.insert(
                "git_server_cloned_path".into(),
                serde_json::Value::String(p.to_string_lossy().into_owned()),
            );
        }
        std::fs::write(&file, serde_json::to_vec_pretty(&out)?)?;
        Ok(())
    }

    /// Read the persisted git-server URL, ignoring missing/malformed files.
    pub fn load_git_server_url() -> Option<String> {
        let path = dirs::home_dir()?.join(".cortex/git-config.json");
        let raw = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v.get("git_server_url")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
    }

    /// Read the persisted cloned-repo path, dropping entries that no longer
    /// exist on disk.
    pub fn load_git_server_cloned_path() -> Option<PathBuf> {
        let path = dirs::home_dir()?.join(".cortex/git-config.json");
        let raw = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let p = PathBuf::from(v.get("git_server_cloned_path")?.as_str()?);
        if p.is_dir() {
            Some(p)
        } else {
            None
        }
    }

    /// Best-effort: ensure the keychain has the baked-in default the very
    /// first time the app runs, so users coming to a fresh machine get a
    /// working connection without opening Settings.
    pub fn seed_keychain_if_empty() {
        // Treat either the new or the legacy (`hermes_backend_api_key`) entry as
        // "already provisioned" so a user upgrading across the rebrand is never
        // re-seeded over a key they already saved.
        let read_entry = |user: &str| {
            keyring::Entry::new(KEYRING_SERVICE, user)
                .ok()
                .and_then(|e| e.get_password().ok())
                .filter(|s| !s.trim().is_empty())
        };
        let existing = read_entry(KEYRING_USER_GATEWAY_KEY)
            .or_else(|| read_entry(KEYRING_USER_HERMES_KEY_LEGACY));
        if existing.is_none() {
            if let Some(key) = baked_gateway_api_key() {
                if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER_GATEWAY_KEY) {
                    let _ = entry.set_password(&key);
                }
            }
        }
    }
}

/// Auto-detect the user's Obsidian vault so the app self-connects on launch.
///
/// Strategy, most-authoritative first:
///   0. Ask Obsidian itself — parse its `obsidian.json` registry and use the
///      vault the user actually has open (or most recently opened). This is
///      how the app "automatically connects to your Obsidian" on any machine
///      without a separate installer step.
///   1-3. Fall back to well-known layout conventions (Windows `Cortex Brain`,
///      `~/vault`, `~/Obsidian-Vault`, the WSL cross-mount for dev builds).
pub(crate) fn default_obsidian_vault() -> Option<PathBuf> {
    // 0. The source of truth: whatever vault Obsidian has registered.
    if let Some(p) = vault_from_obsidian_config() {
        return Some(p);
    }

    // 1. Windows: %USERPROFILE%\Documents\Cortex Brain (also Documents\Obsidian).
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for sub in [["Documents", "Cortex Brain"], ["Documents", "Obsidian"]] {
            let p = profile.join(sub[0]).join(sub[1]);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    // 2. Linux/macOS home + WSL fallback. `vault` / `Obsidian-Vault` are the
    //    homelab clone names; the others are common Obsidian defaults.
    if let Some(home) = dirs::home_dir() {
        for sub in [
            "vault",
            "Obsidian-Vault",
            "Documents/Cortex Brain",
            "Documents/Obsidian",
            "obsidian",
            "Obsidian",
        ] {
            let p = home.join(sub);
            if is_obsidian_vault(&p) || p.is_dir() {
                return Some(p);
            }
        }
    }
    // 3. WSL: explicit cross-mount probe so cargo-xwin dev builds running
    //    from WSL still find user's Windows vault.
    for guess in [
        "/mnt/c/Users/you/Documents/Cortex Brain",
        "/mnt/c/Users/you/Documents/Obsidian",
    ] {
        let p = PathBuf::from(guess);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// A directory is an Obsidian vault iff it contains a `.obsidian` config dir.
fn is_obsidian_vault(dir: &Path) -> bool {
    dir.join(".obsidian").is_dir()
}

/// Candidate locations of Obsidian's `obsidian.json` (the app-level registry of
/// every vault the user has opened), across install flavors and platforms.
fn obsidian_config_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        // Linux native (XDG) and Flatpak.
        out.push(home.join(".config/obsidian/obsidian.json"));
        out.push(home.join(".var/app/md.obsidian.Obsidian/config/obsidian/obsidian.json"));
        // macOS.
        out.push(home.join("Library/Application Support/obsidian/obsidian.json"));
    }
    // Windows / XDG override.
    if let Some(cfg) = dirs::config_dir() {
        out.push(cfg.join("obsidian/obsidian.json"));
    }
    out
}

/// Parse Obsidian's registry and return the vault the user most likely wants:
/// the one currently `open`, else the most recently opened (`ts`), among paths
/// that still exist on disk. Returns `None` if Obsidian isn't installed or no
/// registered vault exists anymore.
fn vault_from_obsidian_config() -> Option<PathBuf> {
    for cfg in obsidian_config_files() {
        let Ok(raw) = std::fs::read_to_string(&cfg) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if let Some(path) = select_registered_vault(&json, |p| p.is_dir()) {
            return Some(path);
        }
    }
    None
}

/// Pure selection over a parsed `obsidian.json`: among registered vaults whose
/// path passes `exists`, prefer the one Obsidian has `open`, then the most
/// recently opened (`ts`). Factored out of [`vault_from_obsidian_config`] so the
/// preference order is unit-testable without touching the filesystem.
fn select_registered_vault(
    json: &serde_json::Value,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let vaults = json.get("vaults").and_then(|v| v.as_object())?;
    let mut best: Option<(bool, i64, PathBuf)> = None; // (is_open, ts, path)
    for entry in vaults.values() {
        let Some(path) = entry.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let pb = PathBuf::from(path);
        if !exists(&pb) {
            continue;
        }
        let is_open = entry.get("open").and_then(|o| o.as_bool()).unwrap_or(false);
        let ts = entry.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        let better = match &best {
            None => true,
            Some((bo, bts, _)) => (is_open, ts) > (*bo, *bts),
        };
        if better {
            best = Some((is_open, ts, pb));
        }
    }
    best.map(|(_, _, path)| path)
}

#[cfg(test)]
mod tests {
    use super::select_registered_vault;
    use std::path::{Path, PathBuf};

    #[test]
    fn picks_open_vault_over_more_recent_closed() {
        let json = serde_json::json!({
            "vaults": {
                "a": {"path": "/home/u/old",   "ts": 100, "open": true},
                "b": {"path": "/home/u/newer", "ts": 999}
            }
        });
        // Both exist → the open one wins even though "newer" has a higher ts.
        let pick = select_registered_vault(&json, |_p| true);
        assert_eq!(pick, Some(PathBuf::from("/home/u/old")));
    }

    #[test]
    fn falls_back_to_most_recent_when_none_open() {
        let json = serde_json::json!({
            "vaults": {
                "a": {"path": "/home/u/old",   "ts": 100},
                "b": {"path": "/home/u/newer", "ts": 999}
            }
        });
        let pick = select_registered_vault(&json, |_p| true);
        assert_eq!(pick, Some(PathBuf::from("/home/u/newer")));
    }

    #[test]
    fn skips_vaults_that_no_longer_exist() {
        let json = serde_json::json!({
            "vaults": { "a": {"path": "/gone", "ts": 999, "open": true} }
        });
        let only_real = |p: &Path| p != Path::new("/gone");
        assert_eq!(select_registered_vault(&json, only_real), None);
    }
}
