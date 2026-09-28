//! Native CLI session continuity — the Cortex-session → native-session map.
//!
//! Headless CLIs (`claude -p`, `codex exec`) are stateless per spawn, so
//! `local_cli::build_prompt` re-folds the whole transcript into every turn.
//! Claude Code and Codex both persist their own sessions and can resume them
//! (`claude --resume <id>`, `codex exec resume <id>`), in which case only the
//! NEW user message needs to be sent: cheaper on subscription quota, and the
//! CLI keeps its own tool-result memory and compaction.
//!
//! This module owns the small JSON store at `~/.cortex/cli-sessions.json`
//! (`{ "sessions": { "<cortex session id>": { agent_id, native_id, model, cwd,
//! updated_ms } } }`) plus the pure decision helpers. An entry is only ever
//! handed back when its `agent_id`, `model` and `cwd` all still match — a
//! model switch or a project change invalidates the native session, since the
//! resumed CLI would otherwise silently keep the old model / working tree.
//!
//! Toggle: `CORTEX_CLI_NATIVE_RESUME` — see [`resume_enabled`]. Per-spec
//! defaults live on `CliSpec::resume_default_on` (Claude on, Codex off).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Env toggle. Unset → each spec's own default. `0|off|false|no` → off for
/// every CLI; `1|on|true|yes|all` → on for every CLI that supports resume; a
/// comma-separated list of spec ids (`claude-cli,codex-cli`) → on for exactly
/// those.
pub const ENV_TOGGLE: &str = "CORTEX_CLI_NATIVE_RESUME";

/// Upper bound on stored entries; the oldest (by `updated_ms`) are pruned so
/// a long-lived install doesn't accumulate one row per chat ever opened.
const MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeSession {
    /// Registry id of the CLI adapter (`claude-cli`, `codex-cli`).
    pub agent_id: String,
    /// The CLI's own session/thread id.
    pub native_id: String,
    /// Resolved model slug the session was started with (may be empty).
    #[serde(default)]
    pub model: String,
    /// Working directory the CLI ran in (lossy string of the path).
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub updated_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    sessions: BTreeMap<String, NativeSession>,
}

/// Decide whether native resume is on for `spec_id`. Pure — `env_value` is
/// the raw `CORTEX_CLI_NATIVE_RESUME` value (or `None` when unset).
pub fn resume_enabled(spec_id: &str, env_value: Option<&str>, spec_default: bool) -> bool {
    let Some(raw) = env_value.map(str::trim).filter(|s| !s.is_empty()) else {
        return spec_default;
    };
    match raw.to_ascii_lowercase().as_str() {
        "0" | "off" | "false" | "no" => false,
        "1" | "on" | "true" | "yes" | "all" => true,
        list => list
            .split(',')
            .map(str::trim)
            .any(|s| !s.is_empty() && s.eq_ignore_ascii_case(spec_id)),
    }
}

/// A native id is passed straight to the CLI as an argument (`--resume <id>`),
/// so only accept the shapes the CLIs actually emit (UUIDs / ULIDs / opaque
/// tokens): ASCII alphanumerics, `-` and `_`, 8..=128 chars. Anything else is
/// treated as "no id".
pub fn valid_native_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Does a CLI error message look like "that session no longer exists"? Used
/// to decide whether a failed resume attempt should fall back to the full
/// history fold (and drop the stored id). Substring-based and deliberately
/// broad — a false positive only costs one extra spawn with the fold.
pub fn looks_like_stale_session(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const HINTS: &[&str] = &[
        "no conversation found",
        "conversation not found",
        "session not found",
        "no session",
        "unknown session",
        "invalid session",
        "session id",
        "session_id",
        "thread not found",
        "no such thread",
        "unknown thread",
        "could not resume",
        "cannot resume",
        "failed to resume",
        "resume",
    ];
    HINTS.iter().any(|h| m.contains(h))
}

/// Does `entry` still apply to a run of `agent_id` with `model` in `cwd`?
pub(crate) fn matches(entry: &NativeSession, agent_id: &str, model: &str, cwd: &str) -> bool {
    entry.agent_id == agent_id && entry.model == model && entry.cwd == cwd
}

/// `~/.cortex/cli-sessions.json`.
pub fn store_path() -> Option<PathBuf> {
    crate::paths::cortex_dir().map(|d| d.join("cli-sessions.json"))
}

/// Process-wide lock around read-modify-write of the store file: concurrent
/// runs (Teams / Arena fan-out) must not clobber each other's entries.
fn store_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Missing or malformed file → empty store (never an error: continuity is a
/// best-effort optimization, the fold path always works).
fn read_store(path: &Path) -> StoreFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Atomic write (temp file in the same dir + rename), mirroring
/// `local_cli::persist_claude_limit`.
fn write_store(path: &Path, store: &StoreFile) -> std::io::Result<()> {
    let Some(dir) = path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "store path has no parent",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = dir.join(format!(
        "cli-sessions.json.tmp.{}.{}",
        std::process::id(),
        now_ms()
    ));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Drop the oldest entries until at most `max` remain. Pure.
pub(crate) fn prune(sessions: &mut BTreeMap<String, NativeSession>, max: usize) {
    while sessions.len() > max {
        let oldest = sessions
            .iter()
            .min_by_key(|(_, s)| s.updated_ms)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                sessions.remove(&k);
            }
            None => break,
        }
    }
}

/// The native id to resume for this Cortex session, if one is stored AND it
/// was created by the same adapter, with the same model, in the same cwd.
pub fn lookup(session_id: &str, agent_id: &str, model: &str, cwd: &str) -> Option<String> {
    let path = store_path()?;
    let _g = store_lock();
    let store = read_store(&path);
    store
        .sessions
        .get(session_id)
        .filter(|e| matches(e, agent_id, model, cwd) && valid_native_id(&e.native_id))
        .map(|e| e.native_id.clone())
}

/// Record (or refresh) the native id for `session_id`. Best-effort: an
/// unwritable `~/.cortex` just means the next turn folds history as before.
pub fn remember(session_id: &str, agent_id: &str, native_id: &str, model: &str, cwd: &str) {
    if session_id.is_empty() || !valid_native_id(native_id) {
        return;
    }
    let Some(path) = store_path() else {
        return;
    };
    let _g = store_lock();
    let mut store = read_store(&path);
    store.sessions.insert(
        session_id.to_string(),
        NativeSession {
            agent_id: agent_id.to_string(),
            native_id: native_id.to_string(),
            model: model.to_string(),
            cwd: cwd.to_string(),
            updated_ms: now_ms(),
        },
    );
    prune(&mut store.sessions, MAX_ENTRIES);
    if let Err(e) = write_store(&path, &store) {
        tracing::debug!(target: "local_cli", "cli-sessions.json write failed: {e}");
    }
}

/// Forget the native id for `session_id` (stale id, fresh conversation, or
/// an explicit "start fresh"). No-op when nothing is stored.
pub fn forget(session_id: &str) {
    let Some(path) = store_path() else {
        return;
    };
    let _g = store_lock();
    let mut store = read_store(&path);
    if store.sessions.remove(session_id).is_none() {
        return;
    }
    if let Err(e) = write_store(&path, &store) {
        tracing::debug!(target: "local_cli", "cli-sessions.json write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_toggle_matrix() {
        // Unset → the spec's own default.
        assert!(resume_enabled("claude-cli", None, true));
        assert!(!resume_enabled("codex-cli", None, false));
        assert!(!resume_enabled("codex-cli", Some("  "), false));
        // Global off / on.
        assert!(!resume_enabled("claude-cli", Some("0"), true));
        assert!(!resume_enabled("claude-cli", Some("OFF"), true));
        assert!(resume_enabled("codex-cli", Some("1"), false));
        assert!(resume_enabled("codex-cli", Some("all"), false));
        // Explicit list.
        assert!(resume_enabled(
            "codex-cli",
            Some("claude-cli, codex-cli"),
            false
        ));
        assert!(!resume_enabled(
            "gemini-cli",
            Some("claude-cli,codex-cli"),
            true
        ));
    }

    #[test]
    fn native_id_shape_is_enforced() {
        assert!(valid_native_id("9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d"));
        assert!(valid_native_id("01J8ZK2M3N4P5Q6R7S8T9V0W1X"));
        assert!(!valid_native_id("short"));
        assert!(!valid_native_id("has space here"));
        assert!(!valid_native_id("--resume-injected;rm"));
        assert!(!valid_native_id(&"x".repeat(129)));
    }

    #[test]
    fn stale_session_detection() {
        assert!(looks_like_stale_session(
            "No conversation found with session ID: abc"
        ));
        assert!(looks_like_stale_session("error: thread not found"));
        assert!(!looks_like_stale_session(
            "rate limit exceeded, try again later"
        ));
        assert!(!looks_like_stale_session(""));
    }

    #[test]
    fn prune_drops_oldest_first() {
        let mut m = BTreeMap::new();
        for (i, k) in ["a", "b", "c", "d"].iter().enumerate() {
            m.insert(
                k.to_string(),
                NativeSession {
                    agent_id: "claude-cli".into(),
                    native_id: format!("id-{k}-00000000"),
                    model: String::new(),
                    cwd: String::new(),
                    updated_ms: 100 - i as i64, // "a" is newest, "d" oldest
                },
            );
        }
        prune(&mut m, 2);
        assert_eq!(m.keys().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn store_round_trip_and_invalidation() {
        crate::paths::test_home::with_temp_home(|home| {
            let sid = "sess-1";
            let nid = "9a1b2c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d";
            assert_eq!(lookup(sid, "claude-cli", "claude-sonnet-4-6", "/p"), None);

            remember(sid, "claude-cli", nid, "claude-sonnet-4-6", "/p");
            assert!(home.join(".cortex").join("cli-sessions.json").is_file());
            assert_eq!(
                lookup(sid, "claude-cli", "claude-sonnet-4-6", "/p").as_deref(),
                Some(nid)
            );
            // Model switch, cwd change or a different adapter all invalidate.
            assert_eq!(lookup(sid, "claude-cli", "claude-opus-4-8", "/p"), None);
            assert_eq!(lookup(sid, "claude-cli", "claude-sonnet-4-6", "/q"), None);
            assert_eq!(lookup(sid, "codex-cli", "claude-sonnet-4-6", "/p"), None);

            // Forget clears it; forgetting again is a no-op.
            forget(sid);
            assert_eq!(lookup(sid, "claude-cli", "claude-sonnet-4-6", "/p"), None);
            forget(sid);

            // A malformed id is never stored.
            remember(sid, "claude-cli", "bad id", "claude-sonnet-4-6", "/p");
            assert_eq!(lookup(sid, "claude-cli", "claude-sonnet-4-6", "/p"), None);

            // Garbage on disk reads as an empty store rather than an error.
            std::fs::write(home.join(".cortex").join("cli-sessions.json"), "{not json").unwrap();
            assert_eq!(lookup(sid, "claude-cli", "claude-sonnet-4-6", "/p"), None);
            remember(sid, "claude-cli", nid, "", "/p");
            assert_eq!(lookup(sid, "claude-cli", "", "/p").as_deref(), Some(nid));
        });
    }
}
