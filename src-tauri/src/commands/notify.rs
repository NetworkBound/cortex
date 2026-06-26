//! Desktop notification command (Codex parity — "task complete" pings).
//!
//! The orchestrator fires this on the `task.complete` hook event so users
//! can switch away from the Cortex window during long runs and still get
//! pinged when something needs their attention. Manual `/notify` from the
//! composer is exposed for smoke-testing the notification pipeline.
//!
//! We use `notify-rust` because it covers the three platforms Cortex
//! supports (Linux/libnotify, macOS/NSUserNotification, Windows/toast)
//! without us shipping per-OS code. Failures are non-fatal — a missing
//! notification daemon shouldn't crash the chat loop — so we surface
//! errors as `Result::Err(String)` and let the frontend toast them.

use notify_rust::Notification;
use serde::Deserialize;

/// App name shown on the notification (Linux/Windows). macOS ignores this
/// and uses the bundle identifier from `Info.plist`.
const APP_NAME: &str = "Cortex";

#[derive(Debug, Deserialize)]
pub struct NotifyArgs {
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// Pop an OS-level desktop notification. Returns `Ok(())` on success;
/// errors carry the underlying `notify_rust` failure string so the UI
/// can fall back to an in-app toast.
///
/// The title is required and length-capped at 256 chars to stay inside
/// the libnotify/Windows summary limits; the body is truncated at 1024
/// chars for the same reason. Truncation is silent because the
/// alternative — rejecting the notification — leaves the user with no
/// signal at all.
#[tauri::command]
pub async fn desktop_notify(args: NotifyArgs) -> Result<(), String> {
    let title = clip(&args.title, 256);
    if title.trim().is_empty() {
        return Err("notification title is empty".to_string());
    }
    let body = clip(&args.body, 1024);

    let mut n = Notification::new();
    n.appname(APP_NAME).summary(&title).body(&body);
    n.show().map(|_| ()).map_err(|e| format!("notify: {e}"))
}

/// Internal helper so other Rust code (the routines scheduler's
/// failed-scheduled-run ping, the orchestrator's `task.complete` hook) can
/// fire a notification without going through Tauri's command dispatch. Same
/// semantics as [`desktop_notify`] but synchronous.
pub fn fire(title: &str, body: &str) -> Result<(), String> {
    let title = clip(title, 256);
    if title.trim().is_empty() {
        return Err("notification title is empty".to_string());
    }
    let body = clip(body, 1024);
    Notification::new()
        .appname(APP_NAME)
        .summary(&title)
        .body(&body)
        .show()
        .map(|_| ())
        .map_err(|e| format!("notify: {e}"))
}

/// Clip `s` at `cap` chars (not bytes) on a UTF-8 boundary. We chose chars
/// because the platform limits are spec'd in display-width, not bytes, and
/// truncating at 256 bytes can produce visibly half-cut emoji.
fn clip(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    s.chars().take(cap).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_respects_char_boundary() {
        let s = "héllo 🌍 world";
        let out = clip(s, 7);
        assert_eq!(out.chars().count(), 7);
    }

    #[test]
    fn clip_passes_through_short_strings() {
        assert_eq!(clip("hi", 100), "hi");
    }

    #[tokio::test]
    async fn rejects_empty_title() {
        let err = desktop_notify(NotifyArgs {
            title: "   ".to_string(),
            body: "x".to_string(),
        })
        .await
        .unwrap_err();
        assert!(err.contains("empty"));
    }
}
