//! Fragment listing — surfaces `~/.cortex/fragments/*.md` names for the
//! @-vocab autocomplete picker. Bodies are inlined at chat-send time via
//! `commands::chat::expand_at_tokens` (`@frag:<name>`).

/// Persist a fragment under `~/.cortex/fragments/<sanitised-name>.md`.
/// Name allowed chars: `[a-z0-9_-]+`. Overwrites silently — fragments are
/// meant to be edited freely.
#[tauri::command]
pub async fn save_fragment(name: String, body: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let safe: String = name
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if safe.is_empty() {
            return Err("name is empty after sanitisation".into());
        }
        let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
        let dir = home.join(".cortex").join("fragments");
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        let path = dir.join(format!("{safe}.md"));
        std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path.display().to_string())
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn list_fragments() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(|| {
        let Some(home) = dirs::home_dir() else { return Vec::<String>::new() };
        let dir = home.join(".cortex").join("fragments");
        let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
        let mut out: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("md") { return None; }
                p.file_stem().and_then(|s| s.to_str().map(String::from))
            })
            .collect();
        out.sort();
        out
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}
