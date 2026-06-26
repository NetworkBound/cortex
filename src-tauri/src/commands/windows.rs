//! Commands for spawning additional main-app windows on demand.
//!
//! Each secondary window loads the same `index.html` with no hash, so it
//! mounts the full <App /> tree — independent of the original window.
//! Per-window state is isolated because every renderer has its own Zustand
//! heap; the shared backend (sqlite + gateway registry) is the single source
//! of truth for anything that needs to persist across windows.

use std::sync::atomic::{AtomicU32, Ordering};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Monotonically increasing counter so every spawned window gets a unique
/// label (`secondary-1`, `secondary-2`, …). Kept process-local — windows
/// from a previous session never need to collide with new ones because
/// Tauri only tracks labels for currently-living webview windows.
static SECONDARY_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Spawn a new top-level Cortex window loading the full app (no hash route).
/// Returns the label of the newly created window so the renderer can refer
/// to it later if needed (focus, close, etc.).
#[tauri::command]
pub async fn open_secondary_window(app: AppHandle) -> Result<String, String> {
    let label = next_label(&app);

    // Use WebviewUrl::App with an empty path so Tauri serves the same
    // index.html the main window loads.
    let url = WebviewUrl::App("index.html".into());

    WebviewWindowBuilder::new(&app, label.clone(), url)
        .title("Cortex")
        .inner_size(1200.0, 800.0)
        .decorations(true)
        .resizable(true)
        .build()
        .map_err(|e| {
            tracing::error!("open_secondary_window: build failed: {e}");
            format!("failed to open window: {e}")
        })?;

    tracing::info!("opened secondary window: {label}");
    Ok(label)
}

/// Generate the next unused `secondary-<N>` label. The counter never
/// rewinds, but we still defensively skip labels that Tauri reports as
/// already taken — that way a future caller (e.g. a restored session)
/// can hand us a label without breaking us.
fn next_label(app: &AppHandle) -> String {
    loop {
        let n = SECONDARY_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let label = format!("secondary-{n}");
        if app.get_webview_window(&label).is_none() {
            return label;
        }
    }
}
