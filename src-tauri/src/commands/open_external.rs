//! "Open in…" bridge: hand a project, worktree or file to the user's editor,
//! file manager or a terminal, and open http(s) URLs in the default browser.
//!
//! Everything here spawns a detached child through [`crate::sys::no_window`]
//! (no console flash on Windows) with the target passed as a separate argv
//! entry — there is no shell string building anywhere, so a path with spaces,
//! `&` or `$(...)` in it is just a path. The one place a shell would be
//! convenient (Windows "start powershell in this folder") is replaced by
//! spawning the shell itself with `current_dir` set and `CREATE_NEW_CONSOLE`,
//! so it inherits the working directory without a `cd` command line.
//!
//! Paths coming from the webview are only accepted when they exist and lie
//! inside a discovered project root (plus each project's `.cortex-worktrees`
//! sibling), the configured default project / cloned repo, `~/.cortex`, the
//! Cortex Brain vault or the Obsidian vault. A compromised renderer therefore
//! can't turn `open_in_editor` into "run an arbitrary program on any file".
//!
//! The per-platform argument building is pure (an [`Os`] value is passed in
//! rather than read from `cfg!`) so every branch is unit-tested on every CI
//! host.

use crate::app_state::AppState;
use crate::projects::discover_projects;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::State;

/// Target platform for the pure argument builders. `current_os()` picks the
/// real one; tests pass each variant explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Linux,
    Mac,
}

fn current_os() -> Os {
    if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Linux
    }
}

/// A fully described child process: program (bare name or path — resolved
/// through [`crate::sys::resolve_program`] at spawn time so `.cmd` shims are
/// found on Windows), argv, optional working directory, and whether the child
/// needs its own visible console (terminals on Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub console: bool,
}

impl Launch {
    fn new(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_string(),
            args,
            cwd: None,
            console: false,
        }
    }
}

// ----- path confinement -----

/// Roots a path may live under without walking the (slow) project discovery:
/// the app's own state dirs and whatever the config already names.
fn cheap_roots(cfg: &ConfigRoots) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(c) = crate::paths::cortex_dir() {
        v.push(c);
    }
    if let Some(b) = crate::paths::brain_dir() {
        v.push(b);
    }
    if let Some(vault) = cfg
        .obsidian_vault
        .clone()
        .or_else(crate::projects::vault_root)
    {
        v.push(vault);
    }
    if let Some(p) = &cfg.default_project_root {
        v.push(p.clone());
        push_worktree_sibling(&mut v, p);
    }
    if let Some(p) = &cfg.git_server_cloned_path {
        v.push(p.clone());
        push_worktree_sibling(&mut v, p);
    }
    v
}

/// `<root>/../.cortex-worktrees` — where `worktrees.rs` creates agent
/// worktrees, deliberately outside the project tree.
fn push_worktree_sibling(v: &mut Vec<PathBuf>, root: &Path) {
    if let Some(parent) = root.parent() {
        v.push(parent.join(".cortex-worktrees"));
    }
}

/// The config fields the confinement needs, copied out of `AppState` before
/// the blocking section so no lock is held while discovery walks disks.
#[derive(Debug, Clone, Default)]
struct ConfigRoots {
    default_project_root: Option<PathBuf>,
    git_server_cloned_path: Option<PathBuf>,
    obsidian_vault: Option<PathBuf>,
}

fn config_roots(state: &AppState) -> ConfigRoots {
    let cfg = state.config.read();
    ConfigRoots {
        default_project_root: cfg.default_project_root.clone(),
        git_server_cloned_path: cfg.git_server_cloned_path.clone(),
        obsidian_vault: cfg.obsidian_vault.clone(),
    }
}

/// `true` when `path` (already canonical) is `root` or lies beneath it.
/// Roots are canonicalized here too so `~/projects` vs a symlinked spelling,
/// or a Windows `\\?\` verbatim prefix, can't cause a false negative.
pub(crate) fn is_within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| {
        let r = crate::paths::canonicalize_lossy(r);
        path.starts_with(&r)
    })
}

/// Basic shape checks shared by every path-taking command: non-empty, no
/// NUL, absolute, and it exists. Returns the canonical (non-verbatim) path.
fn check_shape(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("path is empty".into());
    }
    if raw.contains('\0') {
        return Err("path contains NUL".into());
    }
    let p = PathBuf::from(raw);
    if !p.is_absolute() {
        return Err(format!("path must be absolute: {raw}"));
    }
    if !p.exists() {
        return Err(format!("path does not exist: {raw}"));
    }
    Ok(crate::paths::canonicalize_lossy(&p))
}

/// Full confinement: shape checks, then the cheap roots, then (only if
/// needed) every discovered project root and its worktree sibling.
/// Blocking — call from `spawn_blocking`.
fn confine(raw: &str, cfg: &ConfigRoots) -> Result<PathBuf, String> {
    let canon = check_shape(raw)?;
    if is_within(&canon, &cheap_roots(cfg)) {
        return Ok(canon);
    }
    let mut roots = Vec::new();
    for p in discover_projects(cfg.obsidian_vault.clone()) {
        push_worktree_sibling(&mut roots, &p.root);
        roots.push(p.root);
    }
    if is_within(&canon, &roots) {
        return Ok(canon);
    }
    Err(format!(
        "refusing to open `{raw}`: it is not inside a known project, worktree or Cortex directory"
    ))
}

// ----- spawning -----

/// Does a candidate program exist? Bare names go through `which` (which
/// applies `PATHEXT`, so `code.cmd` counts); anything with a separator is
/// checked on disk.
fn program_exists(name: &str) -> bool {
    if name.contains('/') || name.contains('\\') {
        return Path::new(name).exists();
    }
    which::which(name).is_ok()
}

/// Spawn `launch` detached: stdio to null, never waited on by the caller (a
/// helper thread reaps it so a quick-exit `xdg-open` doesn't linger as a
/// zombie on unix). Errors only when the process cannot be started.
fn spawn_detached(launch: &Launch) -> Result<(), String> {
    let program = crate::sys::resolve_program(&launch.program);
    let mut cmd: Command = if launch.console {
        // A terminal must own a visible console on Windows; elsewhere the
        // GUI terminal emulator creates its own window and this is plain
        // `Command::new`.
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut c = Command::new(&program);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            c.creation_flags(CREATE_NEW_CONSOLE);
        }
        c
    } else {
        crate::sys::no_window(&program)
    };
    cmd.args(&launch.args);
    if let Some(cwd) = &launch.cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start `{}`: {e}", launch.program))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ----- editors -----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorFamily {
    /// VS Code and forks: `--goto path:line`.
    VsCode,
    /// Zed / Sublime: `path:line`.
    PathColonLine,
    /// JetBrains launchers: `--line N path`.
    JetBrains,
    /// vi/nano/emacs/…: needs a TTY, so it is skipped when it comes from
    /// `$VISUAL`/`$EDITOR` (we have no terminal to give it).
    Terminal,
    Other,
}

/// Classify a program by its basename (case-insensitive, `.exe`/`.cmd`/`.bat`
/// stripped, so `Code.cmd` and `/usr/bin/codium` both read as VS Code).
pub fn editor_family(program: &str) -> EditorFamily {
    // Split on both separators by hand: `Path::file_name` only knows the
    // host's separator, so a Windows path handed over as a string (from a
    // setting or the E2E fixtures) would keep its directories on Linux.
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".cmd"))
        .or_else(|| base.strip_suffix(".bat"))
        .unwrap_or(&base);
    match base {
        "code" | "code-insiders" | "codium" | "vscodium" | "cursor" | "windsurf" | "positron" => {
            EditorFamily::VsCode
        }
        "zed" | "zeditor" | "zed-editor" | "subl" | "sublime_text" => EditorFamily::PathColonLine,
        "idea" | "idea64" | "pycharm" | "pycharm64" | "webstorm" | "webstorm64" | "rustrover"
        | "rustrover64" | "clion" | "clion64" | "goland" | "goland64" | "phpstorm" | "rider" => {
            EditorFamily::JetBrains
        }
        "vi" | "vim" | "nvim" | "nano" | "pico" | "emacs" | "micro" | "hx" | "helix" | "joe"
        | "ne" | "kak" => EditorFamily::Terminal,
        _ => EditorFamily::Other,
    }
}

/// Arguments that open `path` (at `line`, when the editor supports it) in a
/// program of the given family.
pub fn editor_args(family: EditorFamily, path: &str, line: Option<u32>) -> Vec<String> {
    match (family, line) {
        (EditorFamily::VsCode, Some(l)) => vec!["--goto".into(), format!("{path}:{l}")],
        (EditorFamily::PathColonLine, Some(l)) => vec![format!("{path}:{l}")],
        (EditorFamily::JetBrains, Some(l)) => vec!["--line".into(), l.to_string(), path.into()],
        _ => vec![path.to_string()],
    }
}

/// Ordered editor candidates: an explicit override, then `CORTEX_EDITOR`,
/// `$VISUAL`, `$EDITOR` (each may carry leading args, e.g. `code --wait`;
/// terminal editors are skipped — there is no TTY to hand them), then the
/// well-known GUI editors. Each entry is `(program, leading args)`.
pub fn editor_candidates(
    override_editor: Option<&str>,
    env: &[(&str, Option<String>)],
) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut push = |raw: &str, allow_terminal: bool| {
        let mut parts = raw.split_whitespace();
        let Some(prog) = parts.next() else {
            return;
        };
        if !allow_terminal && editor_family(prog) == EditorFamily::Terminal {
            return;
        }
        if out.iter().any(|(p, _)| p == prog) {
            return;
        }
        out.push((prog.to_string(), parts.map(str::to_string).collect()));
    };
    if let Some(o) = override_editor {
        push(o, true);
    }
    for (_, val) in env {
        if let Some(v) = val {
            push(v, false);
        }
    }
    for known in ["code", "code-insiders", "codium", "cursor", "zed"] {
        push(known, true);
    }
    out
}

/// Pick the first candidate that exists and build its launch.
fn editor_launch(
    candidates: &[(String, Vec<String>)],
    path: &str,
    line: Option<u32>,
    exists: impl Fn(&str) -> bool,
) -> Option<Launch> {
    candidates.iter().find(|(p, _)| exists(p)).map(|(p, lead)| {
        let mut args = lead.clone();
        args.extend(editor_args(editor_family(p), path, line));
        Launch::new(p, args)
    })
}

/// OS default handler for a file or URL when no editor was found.
pub fn default_opener(target: &str, os: Os) -> Launch {
    match os {
        // `rundll32 url.dll,FileProtocolHandler <target>` is the shell-free
        // equivalent of `start <target>`: no cmd.exe, no quoting rules.
        Os::Windows => Launch::new(
            "rundll32",
            vec!["url.dll,FileProtocolHandler".into(), target.into()],
        ),
        Os::Linux => Launch::new("xdg-open", vec![target.into()]),
        Os::Mac => Launch::new("open", vec![target.into()]),
    }
}

// ----- file manager -----

/// Reveal `path` in the platform file manager. Windows selects the item in
/// its parent (`explorer /select, <path>`), macOS does the same via
/// `open -R`; Linux has no portable "select", so the parent folder is opened
/// (the path itself when it has no parent).
pub fn reveal_launch(path: &Path, os: Os) -> Launch {
    let s = path.to_string_lossy().to_string();
    match os {
        Os::Windows => Launch::new("explorer.exe", vec!["/select,".into(), s]),
        Os::Mac => Launch::new("open", vec!["-R".into(), s]),
        Os::Linux => {
            let target = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(path);
            Launch::new("xdg-open", vec![target.to_string_lossy().to_string()])
        }
    }
}

// ----- terminals -----

/// Linux terminal emulators and the flag that sets their working directory
/// (`None` = no flag; the child inherits `current_dir`). Order is the probe
/// order after `$TERMINAL`.
const LINUX_TERMINALS: &[(&str, Option<&[&str]>)] = &[
    ("x-terminal-emulator", None),
    ("gnome-terminal", Some(&["--working-directory"])),
    ("konsole", Some(&["--workdir"])),
    ("xfce4-terminal", Some(&["--working-directory"])),
    ("alacritty", Some(&["--working-directory"])),
    ("kitty", Some(&["--directory"])),
    ("wezterm", Some(&["start", "--cwd"])),
    ("foot", Some(&["--working-directory"])),
    ("ptyxis", Some(&["--working-directory"])),
    ("tilix", Some(&["--working-directory"])),
    ("terminator", Some(&["--working-directory"])),
    ("mate-terminal", Some(&["--working-directory"])),
    ("lxterminal", Some(&["--working-directory"])),
    ("xterm", None),
];

/// The cwd flag for a terminal program, by basename. Used for `$TERMINAL` and
/// for whatever `x-terminal-emulator` resolves to.
fn terminal_cwd_flag(program: &str) -> Option<&'static [&'static str]> {
    let base = Path::new(program)
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    LINUX_TERMINALS
        .iter()
        .find(|(name, _)| *name == base)
        .and_then(|(_, flag)| *flag)
}

fn terminal_with_flag(program: &str, flag: Option<&[&str]>, dir: &Path) -> Launch {
    let mut args: Vec<String> = Vec::new();
    if let Some(f) = flag {
        args.extend(f.iter().map(|s| s.to_string()));
        args.push(dir.to_string_lossy().to_string());
    }
    Launch {
        program: program.to_string(),
        args,
        cwd: Some(dir.to_path_buf()),
        console: false,
    }
}

/// Build the "terminal here" launch for `dir`. `env_terminal` is `$TERMINAL`
/// (Linux only); `exists` answers "is this program installed?" and `resolve`
/// maps a name to its real path (used to see through the
/// `x-terminal-emulator` alternatives symlink). `None` when nothing usable is
/// installed.
pub fn terminal_launch(
    dir: &Path,
    os: Os,
    env_terminal: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<Launch> {
    let d = dir.to_string_lossy().to_string();
    match os {
        Os::Mac => Some(Launch::new("open", vec!["-a".into(), "Terminal".into(), d])),
        Os::Windows => {
            if exists("wt.exe") || exists("wt") {
                return Some(Launch {
                    program: "wt.exe".into(),
                    args: vec!["-d".into(), d],
                    cwd: Some(dir.to_path_buf()),
                    console: false,
                });
            }
            // A shell spawned with its own console inherits `current_dir`,
            // so no `cd`/`Set-Location` command line is needed.
            let shell = ["pwsh.exe", "powershell.exe", "cmd.exe"]
                .into_iter()
                .find(|s| exists(s))?;
            let args = if shell == "cmd.exe" {
                vec![]
            } else {
                vec!["-NoExit".to_string(), "-NoLogo".to_string()]
            };
            Some(Launch {
                program: shell.into(),
                args,
                cwd: Some(dir.to_path_buf()),
                console: true,
            })
        }
        Os::Linux => {
            if let Some(t) = env_terminal.map(str::trim).filter(|t| !t.is_empty()) {
                if exists(t) {
                    return Some(terminal_with_flag(t, terminal_cwd_flag(t), dir));
                }
            }
            for (name, flag) in LINUX_TERMINALS {
                if !exists(name) {
                    continue;
                }
                // `x-terminal-emulator` is a symlink chain to the real
                // emulator; use the target's flag when we know it.
                let flag = if *name == "x-terminal-emulator" {
                    resolve(name)
                        .and_then(|p| std::fs::canonicalize(p).ok())
                        .and_then(|real| terminal_cwd_flag(&real.to_string_lossy()))
                } else {
                    *flag
                };
                return Some(terminal_with_flag(name, flag, dir));
            }
            None
        }
    }
}

// ----- URLs -----

/// Accept only absolute `http://` / `https://` URLs with no whitespace or
/// control characters (a `file:` or custom scheme must not reach the OS
/// handler from the webview).
pub fn validate_http_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    if u.is_empty() {
        return Err("url is empty".into());
    }
    if u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("url contains whitespace or control characters".into());
    }
    let lower = u.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or_else(|| format!("only http(s) URLs can be opened: {u}"))?;
    if rest.is_empty() || rest.starts_with('/') {
        return Err(format!("url has no host: {u}"));
    }
    Ok(u.to_string())
}

// ----- Tauri commands -----

/// Open `path` (optionally at `line`) in the user's editor. `editor` is an
/// optional override (a program name or path, may carry args like
/// `code --wait`); otherwise `CORTEX_EDITOR`, `$VISUAL`, `$EDITOR` and the
/// well-known GUI editors are tried in order, and finally the OS default
/// handler for the file. Returns the program that was launched.
#[tauri::command]
pub async fn open_in_editor(
    path: String,
    line: Option<u32>,
    editor: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let cfg = config_roots(&state);
    tokio::task::spawn_blocking(move || {
        let canon = confine(&path, &cfg)?;
        let target = canon.to_string_lossy().to_string();
        let env = [
            ("CORTEX_EDITOR", std::env::var("CORTEX_EDITOR").ok()),
            ("VISUAL", std::env::var("VISUAL").ok()),
            ("EDITOR", std::env::var("EDITOR").ok()),
        ];
        let cands = editor_candidates(editor.as_deref(), &env);
        let launch = editor_launch(&cands, &target, line, program_exists)
            .unwrap_or_else(|| default_opener(&target, current_os()));
        spawn_detached(&launch)?;
        Ok(launch.program)
    })
    .await
    .map_err(|e| format!("open_in_editor task failed: {e}"))?
}

/// Reveal `path` in Explorer / Finder / the default Linux file manager.
#[tauri::command]
pub async fn reveal_in_file_manager(
    path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let cfg = config_roots(&state);
    tokio::task::spawn_blocking(move || {
        let canon = confine(&path, &cfg)?;
        spawn_detached(&reveal_launch(&canon, current_os()))
    })
    .await
    .map_err(|e| format!("reveal_in_file_manager task failed: {e}"))?
}

/// Open a terminal window with `dir` as its working directory (a file path
/// opens the terminal in the file's folder). Returns the program launched.
#[tauri::command]
pub async fn open_terminal_here(dir: String, state: State<'_, AppState>) -> Result<String, String> {
    let cfg = config_roots(&state);
    tokio::task::spawn_blocking(move || {
        let canon = confine(&dir, &cfg)?;
        let folder = if canon.is_dir() {
            canon
        } else {
            canon
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("no parent folder for {dir}"))?
        };
        let term = std::env::var("TERMINAL").ok();
        let launch = terminal_launch(
            &folder,
            current_os(),
            term.as_deref(),
            &program_exists,
            &|name: &str| which::which(name).ok(),
        )
        .ok_or_else(|| {
            "no terminal emulator found — set $TERMINAL to the one you use".to_string()
        })?;
        spawn_detached(&launch)?;
        Ok(launch.program)
    })
    .await
    .map_err(|e| format!("open_terminal_here task failed: {e}"))?
}

/// Open an http(s) URL in the default browser.
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), String> {
    let u = validate_http_url(&url)?;
    tokio::task::spawn_blocking(move || spawn_detached(&default_opener(&u, current_os())))
        .await
        .map_err(|e| format!("open_url task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_family_sees_through_paths_and_windows_shims() {
        assert_eq!(editor_family("code"), EditorFamily::VsCode);
        assert_eq!(
            editor_family(r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\bin\code.cmd"),
            EditorFamily::VsCode
        );
        assert_eq!(editor_family("/usr/bin/codium"), EditorFamily::VsCode);
        assert_eq!(editor_family("Cursor.exe"), EditorFamily::VsCode);
        assert_eq!(editor_family("zed"), EditorFamily::PathColonLine);
        assert_eq!(editor_family("subl"), EditorFamily::PathColonLine);
        assert_eq!(editor_family("idea64.exe"), EditorFamily::JetBrains);
        assert_eq!(editor_family("nvim"), EditorFamily::Terminal);
        assert_eq!(editor_family("vim"), EditorFamily::Terminal);
        assert_eq!(editor_family("gedit"), EditorFamily::Other);
    }

    #[test]
    fn editor_args_per_family() {
        let p = "/home/u/proj/src/main.rs";
        assert_eq!(
            editor_args(EditorFamily::VsCode, p, Some(42)),
            vec!["--goto".to_string(), format!("{p}:42")]
        );
        assert_eq!(
            editor_args(EditorFamily::VsCode, p, None),
            vec![p.to_string()]
        );
        assert_eq!(
            editor_args(EditorFamily::PathColonLine, p, Some(7)),
            vec![format!("{p}:7")]
        );
        assert_eq!(
            editor_args(EditorFamily::JetBrains, p, Some(3)),
            vec!["--line".to_string(), "3".to_string(), p.to_string()]
        );
        assert_eq!(
            editor_args(EditorFamily::Other, p, Some(9)),
            vec![p.to_string()]
        );
        // A Windows path with spaces is one argv entry, never split.
        let w = r"C:\Users\me\my proj\a b.rs";
        assert_eq!(
            editor_args(EditorFamily::VsCode, w, Some(1)),
            vec!["--goto".to_string(), format!("{w}:1")]
        );
    }

    #[test]
    fn editor_candidates_order_and_terminal_skip() {
        let env = [
            ("CORTEX_EDITOR", None),
            ("VISUAL", Some("vim".to_string())),
            ("EDITOR", Some("code --wait".to_string())),
        ];
        let c = editor_candidates(None, &env);
        let names: Vec<&str> = c.iter().map(|(p, _)| p.as_str()).collect();
        // vim (terminal) skipped; `code --wait` keeps its leading arg and is
        // de-duplicated against the well-known list.
        assert_eq!(
            names,
            vec!["code", "code-insiders", "codium", "cursor", "zed"]
        );
        assert_eq!(c[0].1, vec!["--wait".to_string()]);

        // An explicit override goes first even if it is a terminal editor.
        let c = editor_candidates(Some("nvim"), &env);
        assert_eq!(c[0].0, "nvim");
    }

    #[test]
    fn editor_launch_picks_first_installed_and_appends_goto() {
        let cands = editor_candidates(
            None,
            &[("CORTEX_EDITOR", None), ("VISUAL", None), ("EDITOR", None)],
        );
        let l = editor_launch(&cands, "/p/f.rs", Some(5), |p| p == "codium").unwrap();
        assert_eq!(l.program, "codium");
        assert_eq!(l.args, vec!["--goto".to_string(), "/p/f.rs:5".to_string()]);
        assert!(editor_launch(&cands, "/p/f.rs", None, |_| false).is_none());
    }

    #[test]
    fn default_opener_per_os_is_shell_free() {
        let u = "https://example.com/a?b=1&c=2";
        let w = default_opener(u, Os::Windows);
        assert_eq!(w.program, "rundll32");
        assert_eq!(
            w.args,
            vec!["url.dll,FileProtocolHandler".to_string(), u.to_string()]
        );
        assert_eq!(default_opener(u, Os::Linux).program, "xdg-open");
        assert_eq!(default_opener(u, Os::Mac).program, "open");
        for os in [Os::Windows, Os::Linux, Os::Mac] {
            let l = default_opener(u, os);
            assert!(
                l.args.iter().any(|a| a == u),
                "{os:?} passes url as one argv"
            );
            assert!(!l.console);
        }
    }

    #[test]
    fn reveal_launch_per_os() {
        let file = Path::new("/home/u/proj/src/main.rs");
        let w = reveal_launch(Path::new(r"C:\Users\me\proj\a b.rs"), Os::Windows);
        assert_eq!(w.program, "explorer.exe");
        assert_eq!(
            w.args,
            vec![
                "/select,".to_string(),
                r"C:\Users\me\proj\a b.rs".to_string()
            ]
        );
        let m = reveal_launch(file, Os::Mac);
        assert_eq!(m.program, "open");
        assert_eq!(
            m.args,
            vec!["-R".to_string(), file.to_string_lossy().to_string()]
        );
        let l = reveal_launch(file, Os::Linux);
        assert_eq!(l.program, "xdg-open");
        assert_eq!(
            l.args,
            vec!["/home/u/proj/src".to_string()],
            "linux opens the parent"
        );
        // Root has no usable parent → opens itself.
        assert_eq!(
            reveal_launch(Path::new("/"), Os::Linux).args,
            vec!["/".to_string()]
        );
    }

    fn no_resolve(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn terminal_windows_prefers_wt_then_shell_with_console() {
        let dir = Path::new(r"C:\Users\me\proj");
        let l = terminal_launch(
            dir,
            Os::Windows,
            None,
            &|p: &str| p == "wt.exe",
            &no_resolve,
        )
        .unwrap();
        assert_eq!(l.program, "wt.exe");
        assert_eq!(
            l.args,
            vec!["-d".to_string(), dir.to_string_lossy().to_string()]
        );
        assert!(!l.console);

        let l = terminal_launch(
            dir,
            Os::Windows,
            None,
            &|p: &str| p == "powershell.exe" || p == "cmd.exe",
            &no_resolve,
        )
        .unwrap();
        assert_eq!(l.program, "powershell.exe");
        assert_eq!(l.args, vec!["-NoExit".to_string(), "-NoLogo".to_string()]);
        assert_eq!(l.cwd.as_deref(), Some(dir));
        assert!(l.console, "a shell needs its own console window");
        // The `$TERMINAL` env var is a Linux convention; ignored on Windows.
        let l = terminal_launch(
            dir,
            Os::Windows,
            Some("kitty"),
            &|p: &str| p == "cmd.exe",
            &no_resolve,
        )
        .unwrap();
        assert_eq!(l.program, "cmd.exe");
        assert!(l.args.is_empty());
        assert!(terminal_launch(dir, Os::Windows, None, &|_| false, &no_resolve).is_none());
    }

    #[test]
    fn terminal_linux_env_then_probe_order_with_cwd_flags() {
        let dir = Path::new("/home/u/my proj");
        let d = dir.to_string_lossy().to_string();
        // $TERMINAL wins and gets its known flag.
        let l = terminal_launch(dir, Os::Linux, Some("kitty"), &|_| true, &no_resolve).unwrap();
        assert_eq!(l.program, "kitty");
        assert_eq!(l.args, vec!["--directory".to_string(), d.clone()]);
        assert_eq!(l.cwd.as_deref(), Some(dir));
        // Unknown $TERMINAL: no flag, cwd inherited.
        let l = terminal_launch(dir, Os::Linux, Some("st"), &|_| true, &no_resolve).unwrap();
        assert_eq!(l.program, "st");
        assert!(l.args.is_empty());
        assert_eq!(l.cwd.as_deref(), Some(dir));
        // $TERMINAL not installed → probe list; gnome-terminal before konsole.
        let l = terminal_launch(
            dir,
            Os::Linux,
            Some("ghostty"),
            &|p: &str| p == "konsole" || p == "gnome-terminal",
            &no_resolve,
        )
        .unwrap();
        assert_eq!(l.program, "gnome-terminal");
        assert_eq!(l.args, vec!["--working-directory".to_string(), d.clone()]);
        let l =
            terminal_launch(dir, Os::Linux, None, &|p: &str| p == "wezterm", &no_resolve).unwrap();
        assert_eq!(
            l.args,
            vec!["start".to_string(), "--cwd".to_string(), d.clone()]
        );
        // Nothing installed.
        assert!(terminal_launch(dir, Os::Linux, None, &|_| false, &no_resolve).is_none());
        // macOS always has Terminal.app.
        let l = terminal_launch(dir, Os::Mac, None, &|_| false, &no_resolve).unwrap();
        assert_eq!(l.program, "open");
        assert_eq!(l.args, vec!["-a".to_string(), "Terminal".to_string(), d]);
    }

    #[test]
    fn validate_http_url_accepts_only_http_s() {
        assert!(validate_http_url("https://example.com/x?y=1").is_ok());
        assert!(validate_http_url("  http://localhost:3000 ").is_ok());
        assert_eq!(
            validate_http_url("HTTPS://Example.com").unwrap(),
            "HTTPS://Example.com",
            "original spelling is preserved"
        );
        for bad in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://x",
            "https://",
            "https:///path",
            "https://a b",
            "http://x\ny",
            "example.com",
        ] {
            assert!(validate_http_url(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn confinement_accepts_inside_and_rejects_outside() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("src");
        std::fs::create_dir_all(&inside).unwrap();
        let file = inside.join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("secret.txt");
        std::fs::write(&outside, "x").unwrap();

        let roots = vec![root.path().to_path_buf()];
        let canon_file = crate::paths::canonicalize_lossy(&file);
        let canon_out = crate::paths::canonicalize_lossy(&outside);
        assert!(is_within(&canon_file, &roots));
        assert!(is_within(
            &crate::paths::canonicalize_lossy(root.path()),
            &roots
        ));
        assert!(!is_within(&canon_out, &roots));

        // Traversal is resolved before the check.
        let sneaky = inside
            .join("..")
            .join("..")
            .join(other.path().file_name().unwrap().to_string_lossy().as_ref());
        let canon_sneaky = crate::paths::canonicalize_lossy(&sneaky);
        assert!(!is_within(&canon_sneaky, &roots));

        // Full `confine` with the default project root pointing at the tempdir.
        let cfg = ConfigRoots {
            default_project_root: Some(root.path().to_path_buf()),
            ..Default::default()
        };
        assert!(confine(&file.to_string_lossy(), &cfg).is_ok());
        // The worktree sibling of the project root is allowed too.
        let wt = root.path().parent().unwrap().join(".cortex-worktrees");
        let wt_created = !wt.exists() && std::fs::create_dir_all(&wt).is_ok();
        if wt.is_dir() {
            let f = wt.join("probe-open-external.txt");
            std::fs::write(&f, "x").unwrap();
            assert!(confine(&f.to_string_lossy(), &cfg).is_ok());
            let _ = std::fs::remove_file(&f);
            if wt_created {
                let _ = std::fs::remove_dir(&wt);
            }
        }
        assert!(check_shape("").is_err());
        assert!(check_shape("relative/path").is_err());
        assert!(check_shape("/definitely/not/here/xyzzy-42").is_err());
        assert!(check_shape("/tmp/has\0nul").is_err());
    }
}
