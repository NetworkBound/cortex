//! PTY lifecycle: open / write / resize / close.
//!
//! Each `open()` call:
//!   1. Creates a portable-pty master/slave pair sized to (cols, rows).
//!   2. Spawns the platform default shell (PowerShell/cmd.exe on Windows,
//!      `$SHELL`/bash on POSIX; `CORTEX_SHELL` overrides — see
//!      [`default_shell_program`]) as the child attached to the slave.
//!   3. Spawns a background reader thread that drains the master and emits
//!      `terminal:output:<id>` window events carrying base64-encoded chunks.
//!   4. Stores `master_writer`, `child`, `master` (for resize) in the global
//!      `SESSIONS` map keyed by the returned UUID `id`.
//!
//! Why base64? Tauri events serialize through JSON, which cannot carry
//! arbitrary bytes. xterm.js can consume Uint8Array or string; we decode
//! on the JS side before calling `term.write`.
//!
//! Failure model: every public function returns `Result<_, String>` so
//! callers can surface errors to the user without exposing internal types.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Per-session state. The master is wrapped in a `Mutex` because resize
/// happens on the command thread and the reader thread reads continuously
/// on its own thread — but only one path mutates it at a time.
struct Session {
    /// Held so we can call `resize()` after open. `MasterPty` is `Send`
    /// but not `Sync`; `Mutex` is fine because resize is rare.
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Writes from `write()` go here. portable-pty hands us a writer that
    /// is `Send` but not `Sync`; the `Mutex` makes it shareable for the
    /// lifetime of the session.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Child shell process. Kept so `close()` can kill it on demand.
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// PID exposed to the frontend purely for display.
    child_pid: u32,
}

static SESSIONS: once_cell::sync::Lazy<Mutex<HashMap<String, Session>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// Upper bound on concurrently open PTY sessions. Each session spawns a real
/// shell process plus an OS reader thread, so an unbounded `open()` loop (buggy
/// or malicious renderer) could exhaust PIDs/FDs/memory. This caps the blast
/// radius; the frontend never needs anywhere near this many terminals.
const MAX_SESSIONS: usize = 64;

/// Handle returned to the frontend after a successful `open()`.
#[derive(Debug, Serialize, Clone)]
pub struct PtyHandle {
    pub id: String,
    pub child_pid: u32,
}

/// Open a new PTY sized to `(cols, rows)` and spawn the platform default
/// shell as the child. A background reader thread starts immediately and
/// will emit `terminal:output:<id>` events with base64 chunks.
pub fn open(app: AppHandle, cols: u16, rows: u16) -> Result<PtyHandle, String> {
    open_command(app, cols, rows, None)
}

/// Like [`open`], but spawns an explicit command instead of the default shell.
/// `program_and_args` is `Some((program, args))` — used by the in-app provider
/// sign-in flow to launch e.g. `claude /login` or `codex login` interactively
/// in a real terminal so the user can complete OAuth. `None` falls back to the
/// default shell. The program is run via the platform shell as an argv vector
/// (never a concatenated string), so nothing is shell-interpolated.
pub fn open_command(
    app: AppHandle,
    cols: u16,
    rows: u16,
    program_and_args: Option<(String, Vec<String>)>,
) -> Result<PtyHandle, String> {
    // Clamp to sensible bounds. xterm.js can ask for 0x0 during initial
    // mount before the FitAddon has measured the container.
    let cols = cols.max(1);
    let rows = rows.max(1);

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let cmd = match program_and_args {
        Some((program, args)) => command_for(&program, &args)?,
        None => default_shell_command(),
    };
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn shell failed: {e}"))?;
    // Drop the slave; the child now owns it. Keeping it open would leave a
    // dangling fd that prevents EOF detection on the master.
    drop(pair.slave);

    // A missing PID means the child is in an unexpected state; rather than
    // reporting a bogus 0 (which means "this process group" to POSIX kill),
    // tear the child down and surface a real error to the caller.
    let child_pid = match child.process_id() {
        Some(pid) => pid,
        None => {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err("spawned shell reported no PID".to_string());
        }
    };
    let id = Uuid::new_v4().to_string();

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take writer failed: {e}"))?;

    let session = Session {
        master: Arc::new(Mutex::new(pair.master)),
        writer: Arc::new(Mutex::new(writer)),
        child: Arc::new(Mutex::new(child)),
        child_pid,
    };

    {
        let mut sessions = SESSIONS
            .lock()
            .map_err(|_| "sessions mutex poisoned".to_string())?;
        if sessions.len() >= MAX_SESSIONS {
            // Reap the just-spawned child before bailing so we don't leak it.
            if let Ok(mut child) = session.child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(format!("too many open terminals (max {MAX_SESSIONS})"));
        }
        sessions.insert(id.clone(), session);
    }

    // Spawn the reader on a dedicated OS thread — portable-pty's reader
    // is blocking, and we don't want to tie up a tokio worker.
    spawn_reader(app, id.clone(), reader);

    Ok(PtyHandle { id, child_pid })
}

/// Write user keystrokes (already decoded from base64 on the JS side OR
/// passed straight through as UTF-8) to the PTY master.
pub fn write(id: &str, bytes: &[u8]) -> Result<(), String> {
    let sessions = SESSIONS
        .lock()
        .map_err(|_| "sessions mutex poisoned".to_string())?;
    let session = sessions
        .get(id)
        .ok_or_else(|| format!("unknown pty id: {id}"))?;
    let writer = session.writer.clone();
    drop(sessions);

    let mut w = writer
        .lock()
        .map_err(|_| "writer mutex poisoned".to_string())?;
    w.write_all(bytes).map_err(|e| format!("pty write: {e}"))?;
    w.flush().map_err(|e| format!("pty flush: {e}"))?;
    Ok(())
}

/// Resize the PTY. xterm.js fires this on every container resize via the
/// FitAddon.
pub fn resize(id: &str, cols: u16, rows: u16) -> Result<(), String> {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let sessions = SESSIONS
        .lock()
        .map_err(|_| "sessions mutex poisoned".to_string())?;
    let session = sessions
        .get(id)
        .ok_or_else(|| format!("unknown pty id: {id}"))?;
    let master = session.master.clone();
    drop(sessions);

    let m = master
        .lock()
        .map_err(|_| "master mutex poisoned".to_string())?;
    m.resize(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
    .map_err(|e| format!("pty resize: {e}"))?;
    Ok(())
}

/// Kill the child shell and drop the session. The reader thread will exit
/// on its next read once the master sees EOF.
pub fn close(id: &str) -> Result<(), String> {
    let mut sessions = SESSIONS
        .lock()
        .map_err(|_| "sessions mutex poisoned".to_string())?;
    let Some(session) = sessions.remove(id) else {
        // Idempotent: closing an already-closed session is fine.
        return Ok(());
    };
    drop(sessions);

    if let Ok(mut child) = session.child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// Snapshot of currently active sessions for debug/UX surfaces.
pub fn list_active() -> Vec<PtyHandle> {
    let Ok(sessions) = SESSIONS.lock() else {
        return Vec::new();
    };
    sessions
        .iter()
        .map(|(id, s)| PtyHandle {
            id: id.clone(),
            child_pid: s.child_pid,
        })
        .collect()
}

/// Returns the command to launch as the child of the PTY.
///
/// Resolution order (see [`default_shell_program`]): an explicit `CORTEX_SHELL`
/// override, then the platform default — PowerShell (`pwsh`, then Windows
/// PowerShell) falling back to `%COMSPEC%`/`cmd.exe` on Windows; `$SHELL`
/// falling back to `/bin/bash` then `/bin/sh` on POSIX.
fn default_shell_command() -> CommandBuilder {
    let (program, args) = default_shell_program();
    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    apply_session_env(&mut cmd);
    cmd
}

/// Pick the interactive shell to spawn and its startup args.
///
/// `CORTEX_SHELL` (a program name or absolute path) wins on every platform so a
/// user can pin e.g. `fish` or `nu`. Otherwise:
///
/// - **Windows**: PowerShell is what Windows Terminal opens by default and what
///   most users expect, so prefer `pwsh.exe` (PowerShell 7) then
///   `powershell.exe` (5.1, on every supported Windows), each with `-NoLogo` so
///   the banner doesn't eat the first rows. `%COMSPEC%` (normally `cmd.exe`) is
///   the last resort — every host has it.
/// - **POSIX**: honour the user's login shell via `$SHELL` when it names an
///   existing file, else `/bin/bash`, else `/bin/sh` (minimal containers and
///   some BSDs have no bash).
fn default_shell_program() -> (String, Vec<&'static str>) {
    if let Some(custom) = std::env::var_os("CORTEX_SHELL") {
        let custom = custom.to_string_lossy().trim().to_string();
        if !custom.is_empty() {
            return (custom, Vec::new());
        }
    }
    #[cfg(windows)]
    {
        for candidate in ["pwsh.exe", "powershell.exe"] {
            if let Ok(p) = which::which(candidate) {
                return (p.to_string_lossy().into_owned(), vec!["-NoLogo"]);
            }
        }
        let comspec = std::env::var_os("COMSPEC")
            .map(|c| c.to_string_lossy().trim().to_string())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "cmd.exe".to_string());
        (comspec, Vec::new())
    }
    #[cfg(not(windows))]
    {
        (posix_shell_from(std::env::var_os("SHELL")), Vec::new())
    }
}

/// POSIX shell choice given the raw `$SHELL` value: use it when it names an
/// existing file, else `/bin/bash`, else `/bin/sh`. Split out so the fallback
/// chain is unit-testable without touching the process environment.
#[cfg(not(windows))]
fn posix_shell_from(shell_env: Option<std::ffi::OsString>) -> String {
    if let Some(sh) = shell_env {
        let sh = sh.to_string_lossy().trim().to_string();
        if !sh.is_empty() && std::path::Path::new(&sh).is_file() {
            return sh;
        }
    }
    for fallback in ["/bin/bash", "/bin/sh"] {
        if std::path::Path::new(fallback).is_file() {
            return fallback.to_string();
        }
    }
    "/bin/sh".to_string()
}

/// Environment + cwd shared by every PTY child (default shell or explicit
/// program).
fn apply_session_env(cmd: &mut CommandBuilder) {
    // TERM tells the shell + readline what escape sequences to emit.
    // xterm.js advertises itself as xterm-256color compatible.
    cmd.env("TERM", "xterm-256color");
    // Start in the user's home directory. `dirs::home_dir()` is the canonical,
    // cross-platform resolution used elsewhere in the app (chat_history,
    // editor): on Windows it consults the known-folder API + HOMEDRIVE/HOMEPATH
    // rather than `$HOME` (which is usually unset there, leaving the terminal
    // in an arbitrary cwd); on POSIX it reads `$HOME`.
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
}

/// Build a [`CommandBuilder`] for an explicit `program` + `args`, mirroring the
/// shell-command env/cwd setup. The program and each arg are passed as distinct
/// argv elements (portable-pty does not invoke a shell), so user-facing values
/// are never re-parsed or interpolated. Used by the in-app login flow, whose
/// argv is a CLI spec's fixed `login_cmd` literals.
fn command_for(program: &str, args: &[String]) -> Result<CommandBuilder, String> {
    // On Windows, npm-installed CLIs are `.cmd`/`.bat` shims that conpty
    // (CreateProcess) cannot launch by bare name — resolve the shim via PATHEXT
    // (`which`) and run it through `cmd.exe /C`. `.exe` shims and all POSIX
    // binaries run directly. Without this the in-app "Sign in" flow (which spawns
    // e.g. `codex login`) fails on Windows with a spawn error.
    //
    // `cmd.exe /C` re-parses its command line with cmd's own quoting rules, not
    // the CreateProcess rules portable-pty escapes for, so this path is only
    // safe for fixed literal tokens. Every caller today passes exactly that
    // (`CliSpec::login_cmd`); refuse anything carrying a cmd metacharacter so a
    // future caller can't turn an arg into an injected command.
    #[cfg(windows)]
    let mut cmd = {
        let resolved = which::which(program).ok();
        let is_shim = resolved
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "cmd" | "bat"))
            .unwrap_or(false);
        match (is_shim, resolved) {
            (true, Some(shim)) => {
                if let Some(bad) = args.iter().find(|a| has_cmd_metachar(a)) {
                    return Err(format!(
                        "refusing to run '{program}' via cmd.exe: argument {bad:?} contains shell metacharacters"
                    ));
                }
                let mut c = CommandBuilder::new("cmd");
                c.arg("/C");
                c.arg(shim);
                c
            }
            _ => CommandBuilder::new(program),
        }
    };
    #[cfg(not(windows))]
    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    apply_session_env(&mut cmd);
    Ok(cmd)
}

/// Characters `cmd.exe` interprets when re-parsing a `/C` command line.
#[cfg(windows)]
fn has_cmd_metachar(arg: &str) -> bool {
    arg.chars()
        .any(|c| matches!(c, '&' | '|' | '<' | '>' | '^' | '"' | '%' | '!' | '(' | ')'))
}

/// Background reader: pulls bytes off the PTY master and emits them as
/// base64-encoded payloads on `terminal:output:<id>`. Exits cleanly on
/// EOF (child died) or on read error.
fn spawn_reader(app: AppHandle, id: String, mut reader: Box<dyn Read + Send>) {
    let event_name = format!("terminal:output:{id}");
    thread::spawn(move || {
        // 4 KiB chunks balance latency against per-event overhead. Smaller
        // chunks give snappier streaming on slow output; bigger chunks
        // reduce JSON-encoding cost on `npm run build` floods.
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: child exited
                Ok(n) => {
                    let encoded = B64.encode(&buf[..n]);
                    if let Err(e) = app.emit(&event_name, encoded) {
                        tracing::warn!("terminal: emit failed for {id}: {e}");
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("terminal: reader exit for {id}: {e}");
                    break;
                }
            }
        }
        // Best-effort cleanup so the SESSIONS map doesn't leak entries
        // when the shell exits on its own (e.g. user typed `exit`). We must
        // also reap the child here: dropping the Arc<Mutex<Child>> does not
        // guarantee the OS reaps it, so a self-exited shell would otherwise
        // linger as a zombie until process exit.
        if let Ok(mut sessions) = SESSIONS.lock() {
            if let Some(session) = sessions.remove(&id) {
                if let Ok(mut child) = session.child.lock() {
                    let _ = child.wait();
                }
            }
        }
        let _ = app.emit(&format!("terminal:closed:{id}"), ());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn posix_shell_honours_existing_shell_env() {
        // `/bin/sh` exists on every POSIX CI runner; a `$SHELL` pointing at it
        // must be used verbatim.
        assert_eq!(
            posix_shell_from(Some(std::ffi::OsString::from("/bin/sh"))),
            "/bin/sh"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn posix_shell_ignores_missing_or_blank_shell_env() {
        let bogus = posix_shell_from(Some(std::ffi::OsString::from("/nonexistent/zsh-9")));
        assert!(bogus == "/bin/bash" || bogus == "/bin/sh", "got {bogus}");
        let blank = posix_shell_from(Some(std::ffi::OsString::from("   ")));
        assert!(blank == "/bin/bash" || blank == "/bin/sh", "got {blank}");
        let unset = posix_shell_from(None);
        assert!(unset == "/bin/bash" || unset == "/bin/sh", "got {unset}");
    }

    #[test]
    fn default_shell_program_is_never_empty() {
        let (program, _args) = default_shell_program();
        assert!(!program.trim().is_empty());
    }
}
