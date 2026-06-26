//! Tauri command wrappers around `crate::terminal::pty`.
//!
//! The frontend (xterm.js in `TerminalPane.tsx`) calls:
//!   - `terminal_open` on mount,
//!   - `terminal_write` on every keystroke,
//!   - `terminal_resize` whenever the FitAddon recomputes geometry,
//!   - `terminal_close` on unmount.
//!
//! User input is sent as a base64 string to keep the IPC payload binary-safe
//! (xterm sends raw UTF-8 sequences for things like arrow keys that we
//! don't want JSON to mangle).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::terminal::pty::{self, PtyHandle};

#[tauri::command]
pub async fn terminal_open(
    app: tauri::AppHandle,
    cols: u16,
    rows: u16,
) -> Result<PtyHandle, String> {
    pty::open(app, cols, rows)
}

#[tauri::command]
pub async fn terminal_write(id: String, data_b64: String) -> Result<(), String> {
    let bytes = B64
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("invalid base64: {e}"))?;
    pty::write(&id, &bytes)
}

#[tauri::command]
pub async fn terminal_resize(id: String, cols: u16, rows: u16) -> Result<(), String> {
    pty::resize(&id, cols, rows)
}

#[tauri::command]
pub async fn terminal_close(id: String) -> Result<(), String> {
    pty::close(&id)
}

#[tauri::command]
pub async fn terminal_list_active() -> Result<Vec<PtyHandle>, String> {
    Ok(pty::list_active())
}
