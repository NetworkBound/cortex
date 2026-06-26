//! Custom theme + background-image store for Cortex.
//!
//! - Custom user themes live at `~/.cortex/themes/<name>.json` (one JSON
//!   document per theme, matching the `Theme` shape in
//!   `src/lib/themes-custom.ts`).
//! - The active theme name and the persisted background-image path live in a
//!   single root file: `~/.cortex/themes.json` (shape: `ActiveThemeState`).
//! - Background-image bytes are copied into `~/.cortex/bg/active.<ext>` so the
//!   image survives the user moving/deleting the source file. The renderer
//!   then loads it via the Tauri asset protocol (see `bg-image.ts`).
//!
//! Everything degrades to "no themes / no background" rather than panic when
//! files are missing or corrupt — the UI still works on a fresh install with
//! zero custom themes on disk.
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

/// User-authored theme definition. Mirrors the `Theme` shape in
/// `src/lib/themes-custom.ts` — kept loose with `String` colors so the user
/// can plug in any CSS-valid value (hex, rgb(), oklch(), …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomTheme {
    pub name: String,
    pub accent: String,
    #[serde(default, alias = "accentStrong")]
    pub accent_strong: String,
    #[serde(default, alias = "accentDim")]
    pub accent_dim: String,
    pub bg: String,
    #[serde(default, alias = "bgElevated")]
    pub bg_elevated: String,
    #[serde(default, alias = "bgSunken")]
    pub bg_sunken: String,
    pub text: String,
    #[serde(default, alias = "textDim")]
    pub text_dim: String,
    #[serde(default, alias = "textMuted")]
    pub text_muted: String,
    #[serde(default)]
    pub success: String,
    #[serde(default)]
    pub warning: String,
    #[serde(default)]
    pub danger: String,
    #[serde(default, alias = "fontSans")]
    pub font_sans: String,
    #[serde(default, alias = "fontMono")]
    pub font_mono: String,
}

/// Root state file — small and read on every theme/background change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveThemeState {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub bg_image_path: Option<String>,
}

fn cortex_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    Ok(home.join(".cortex"))
}

fn themes_dir() -> Result<PathBuf, String> {
    Ok(cortex_dir()?.join("themes"))
}

fn state_path() -> Result<PathBuf, String> {
    Ok(cortex_dir()?.join("themes.json"))
}

fn bg_dir() -> Result<PathBuf, String> {
    Ok(cortex_dir()?.join("bg"))
}

/// Theme names map straight to filenames, so guard against path traversal and
/// keep them filesystem-friendly.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn load_state() -> ActiveThemeState {
    let Ok(path) = state_path() else {
        return ActiveThemeState::default();
    };
    let Ok(bytes) = fs::read(&path) else {
        return ActiveThemeState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_state(state: &ActiveThemeState) -> Result<(), String> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(state).map_err(|e| format!("serialize failed: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn list_themes() -> Result<Vec<CustomTheme>, String> {
    tokio::task::spawn_blocking(|| {
        let dir = match themes_dir() {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<CustomTheme> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(OsStr::to_str) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let Ok(theme) = serde_json::from_slice::<CustomTheme>(&bytes) else {
                continue;
            };
            out.push(theme);
        }
        // Stable order for the picker so the UI doesn't reshuffle on every
        // call. Built-in presets are appended on the frontend.
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn get_active_theme() -> Result<ActiveThemeState, String> {
    tokio::task::spawn_blocking(load_state)
        .await
        .map_err(|e| format!("join error: {e}"))
}

#[tauri::command]
pub async fn set_active_theme(name: String) -> Result<ActiveThemeState, String> {
    if !is_valid_name(&name) {
        return Err(format!(
            "invalid theme name '{name}': use letters, digits, _, -, ."
        ));
    }
    tokio::task::spawn_blocking(move || {
        let mut state = load_state();
        state.active = name;
        save_state(&state)?;
        Ok::<ActiveThemeState, String>(state)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Set or clear the background image. Passing `None` clears the persisted
/// path; passing `Some(src)` copies the source file into
/// `~/.cortex/bg/active.<ext>` and persists that path.
#[tauri::command]
pub async fn set_bg_image(source: Option<String>) -> Result<ActiveThemeState, String> {
    tokio::task::spawn_blocking(move || {
        let mut state = load_state();
        match source {
            None => {
                state.bg_image_path = None;
                // Best-effort cleanup of any prior active.* files. We don't
                // care if there isn't one.
                if let Ok(dir) = bg_dir() {
                    if let Ok(entries) = fs::read_dir(&dir) {
                        for e in entries.flatten() {
                            let p = e.path();
                            let stem = p.file_stem().and_then(OsStr::to_str);
                            if stem == Some("active") {
                                let _ = fs::remove_file(&p);
                            }
                        }
                    }
                }
            }
            Some(src) => {
                let src_path = PathBuf::from(&src);
                if !src_path.is_file() {
                    return Err(format!("source is not a file: {src}"));
                }
                // Cheap sanity cap so we don't copy a 4 GB MKV by mistake.
                if let Ok(meta) = fs::metadata(&src_path) {
                    if meta.len() > 32 * 1024 * 1024 {
                        return Err("background image exceeds 32 MB limit".to_string());
                    }
                }
                let ext = src_path
                    .extension()
                    .and_then(OsStr::to_str)
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_else(|| "png".to_string());
                // Whitelist common image formats — keeps us off random binary
                // blobs the user accidentally points us at.
                if !matches!(
                    ext.as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "avif"
                ) {
                    return Err(format!("unsupported image extension: {ext}"));
                }
                let dir = bg_dir()?;
                fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
                // Wipe any previous active.* before writing the new one so we
                // don't end up with stale active.jpg + active.png coexisting.
                if let Ok(entries) = fs::read_dir(&dir) {
                    for e in entries.flatten() {
                        let p = e.path();
                        let stem = p.file_stem().and_then(OsStr::to_str);
                        if stem == Some("active") {
                            let _ = fs::remove_file(&p);
                        }
                    }
                }
                let dest = dir.join(format!("active.{ext}"));
                fs::copy(&src_path, &dest).map_err(|e| format!("copy failed: {e}"))?;
                state.bg_image_path = Some(dest.to_string_lossy().to_string());
            }
        }
        save_state(&state)?;
        Ok::<ActiveThemeState, String>(state)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(is_valid_name("zinc-amber"));
        assert!(is_valid_name("My.Theme_v2"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name("with space"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }
}
