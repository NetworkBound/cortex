//! Codex-style three-tier sandbox.
//!
//! A *second* gate (after guardrails) that classifies the whole session's
//! permission to act:
//!
//!   * `ReadOnly`          — only read-shaped tools (read/search/ls/…).
//!   * `WorkspaceWrite`    — read tools + write/edit/patch/run_*, but writes
//!                           must target a path inside `project_root`.
//!   * `DangerFullAccess`  — anything goes.
//!
//! Persisted at `<project_root>/.cortex/sandbox.toml`:
//! ```toml
//! tier = "workspace-write"
//! ```
//!
//! Tier evaluation is **deny-bias**: a tier rejection in `chat.rs` skips the
//! tool call before guardrails and approvals get a chance to allow it. High-
//! risk guardrails still override an allow from this layer — they live in the
//! caller. See `commands/chat.rs` for wiring.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Three Codex-style tiers, ordered by how much they let through.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxTier {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl Default for SandboxTier {
    fn default() -> Self {
        Self::WorkspaceWrite
    }
}

impl SandboxTier {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxTier::ReadOnly => "read-only",
            SandboxTier::WorkspaceWrite => "workspace-write",
            SandboxTier::DangerFullAccess => "danger-full-access",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" | "read_only" => Some(SandboxTier::ReadOnly),
            "workspace-write" | "workspacewrite" | "workspace_write" => {
                Some(SandboxTier::WorkspaceWrite)
            }
            // NB: keep this vocabulary in sync with
            // `profiles::is_valid_sandbox_tier`. We deliberately do NOT accept a
            // bare "full-access" alias here: that string is rejected by profile
            // validation, so accepting it as DangerFullAccess elsewhere would
            // let the most-permissive tier through one path while another
            // silently refuses the same value.
            "danger-full-access" | "dangerfullaccess" | "danger_full_access" => {
                Some(SandboxTier::DangerFullAccess)
            }
            _ => None,
        }
    }
}

/// On-disk schema for `.cortex/sandbox.toml`.
#[derive(Debug, Deserialize, Serialize, Default)]
struct SandboxFile {
    tier: Option<String>,
}

fn sandbox_path(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("sandbox.toml")
}

/// Load the configured tier for a project. Missing / malformed files fall
/// back to `SandboxTier::default()` (WorkspaceWrite) so behavior matches the
/// historical default of "we just run things in the workspace".
pub fn load_tier(project_root: &Path) -> SandboxTier {
    let path = sandbox_path(project_root);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("sandbox: no tier file ({}): {e}", path.display());
            return SandboxTier::default();
        }
    };
    let parsed: SandboxFile = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("sandbox: bad toml at {}: {e}", path.display());
            return SandboxTier::default();
        }
    };
    parsed
        .tier
        .as_deref()
        .and_then(SandboxTier::parse)
        .unwrap_or_default()
}

/// The tier a project ACTUALLY runs under, trust gate included: an untrusted
/// project (or no project at all) is pinned to `ReadOnly` regardless of any
/// on-disk `.cortex/sandbox.toml` — a malicious repo must not be able to
/// widen its own sandbox. Single source of truth shared by `chat.rs`'s event
/// gate and the CLI adapters that pass a sandbox flag to their subprocess
/// (`codex_spec.rs`), so the two can never drift.
pub fn effective_tier(project_root: Option<&Path>) -> SandboxTier {
    match project_root {
        Some(root) if super::trust::is_trusted(root) => load_tier(root),
        _ => SandboxTier::ReadOnly,
    }
}

/// Write the configured tier for a project, creating `.cortex/` if needed.
pub fn write_tier(project_root: &Path, tier: SandboxTier) -> anyhow::Result<()> {
    let path = sandbox_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = format!("tier = \"{}\"\n", tier.as_str());
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    file.write_all(body.as_bytes())?;
    Ok(())
}

/// Lowercase tool-name fragments that classify as read-shaped. Matched as
/// substrings so adapter-specific prefixes (`fs.read_file`, `bash_ls`, …)
/// all map cleanly to ReadOnly.
pub(crate) const READ_TOKENS: &[&str] = &[
    "read", "search", "ls", "list", "grep", "glob", "fetch", "status", "view",
];

/// Lowercase tool-name fragments that classify as write/exec-shaped. These
/// require `WorkspaceWrite` or higher and (for writes) a path inside the
/// project root.
pub(crate) const WRITE_TOKENS: &[&str] = &[
    "write", "edit", "patch", "create", "delete", "remove", "apply", "run_",
    "exec", "shell", "bash", "save", "modify", "append", "update", "insert",
    "mkdir", "rmdir", "move", "rename", "copy", "chmod", "chown", "touch",
    "mv", "cp", "rm", "set", "put", "upload", "format", "truncate", "replace",
    // State-mutating verbs that can pair with a read token (e.g.
    // `git_status_reset`) and would otherwise slip through ReadOnly.
    "reset", "revert", "drop", "destroy", "kill", "prune", "clean", "wipe",
    "purge", "push", "commit", "merge", "rebase", "stash", "checkout",
];

pub(crate) fn name_matches_any(name: &str, tokens: &[&str]) -> bool {
    let n = name.to_ascii_lowercase();
    tokens.iter().any(|t| n.contains(t))
}

/// Lowercase key-name fragments whose string value is treated as a filesystem
/// path. Matched as substrings so adapter-specific spellings (`out_path`,
/// `dst_file`, `output_filename`, …) are all covered.
const PATH_KEY_TOKENS: &[&str] = &[
    "path", "file", "dir", "dest", "destination", "src", "source", "target",
    "output", "out", "input", "filename", "folder", "location",
];

/// Heuristic: does this string value look like a filesystem path we must
/// confine? We flag anything that is absolute (`/…`, `C:\…`, `\\…`) or that
/// contains a `..` traversal component, regardless of the key it sits under.
/// This catches out-of-root targets smuggled through unrecognized keys.
fn looks_like_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let p = Path::new(s);
    if p.is_absolute() {
        return true;
    }
    // Windows-style absolute / UNC that `is_absolute` may miss on unix builds.
    if s.starts_with('/') || s.starts_with('\\') {
        return true;
    }
    if s.len() >= 2 {
        let b = s.as_bytes();
        if b[1] == b':' && b[0].is_ascii_alphabetic() {
            return true;
        }
    }
    p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Walk the JSON tree and collect EVERY string value that should be treated as
/// a filesystem path: values under a path-shaped key, plus any string anywhere
/// that lexically looks like an absolute path or `..` traversal. Validating all
/// of them (rather than the first match) prevents a benign in-root path from
/// masking an out-of-root sibling field.
pub(crate) fn collect_paths(payload_json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return out;
    };
    fn walk(v: &serde_json::Value, key_is_pathish: bool, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let kl = k.to_ascii_lowercase();
                    let child_pathish = PATH_KEY_TOKENS.iter().any(|t| kl.contains(t));
                    walk(child, child_pathish, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for child in arr {
                    // Inherit the parent key's path-ness for array elements
                    // (e.g. `"paths": ["a", "b"]`).
                    walk(child, key_is_pathish, out);
                }
            }
            serde_json::Value::String(s) => {
                if !s.is_empty() && (key_is_pathish || looks_like_path(s)) {
                    out.push(s.clone());
                }
            }
            _ => {}
        }
    }
    walk(&v, false, &mut out);
    out
}

/// Lexically normalize a path (no fs hits): collapse `.` and resolve `..`
/// against the preceding component. A `..` that would escape above the first
/// component is preserved as a leading `..` so the containment check below can
/// detect the escape. Mirrors the absolute-path leading components verbatim.
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut stack: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                match stack.last() {
                    // Pop a real directory name we previously pushed.
                    Some(Component::Normal(_)) => {
                        stack.pop();
                    }
                    // Can't pop a root/prefix; can't cancel an existing `..`.
                    // Keep the `..` so an escape stays visible.
                    Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                    _ => stack.push(comp),
                }
            }
            other => stack.push(other),
        }
    }
    stack.iter().collect()
}

/// True if `candidate` is inside `root` after lexical normalization (no fs
/// hits). Relative candidates are resolved *against `root`* (the caller runs
/// tools with `root` as cwd), then confined; absolute candidates are confined
/// directly. Either way a `..` that climbs above `root` is rejected.
pub fn path_inside(root: &Path, candidate: &str) -> bool {
    let cand = PathBuf::from(candidate);
    // Anchor relative paths to the project root before normalizing, so a
    // relative path is only "inside" when it actually resolves under root.
    let resolved = if cand.is_relative() {
        lexical_normalize(&root.join(&cand))
    } else {
        lexical_normalize(&cand)
    };
    let root_norm = lexical_normalize(root);
    let root_str = root_norm.to_string_lossy().replace('\\', "/");
    let cand_str = resolved.to_string_lossy().replace('\\', "/");
    let root_trim = root_str.trim_end_matches('/');
    if root_trim.is_empty() {
        // Degenerate empty root: only an exactly-empty resolution is "inside".
        return cand_str.is_empty();
    }
    cand_str == root_trim || cand_str.starts_with(&format!("{root_trim}/"))
}

/// Well-known credential/secret-store path fragments that must never be
/// exposed to a tool call under `ReadOnly`/`WorkspaceWrite`, regardless of
/// this module's broader "reads aren't root-confined" design (see the
/// module doc — that's an intentional tradeoff so agents can read
/// cross-project context like a global CLAUDE.md; loosening it into full
/// root confinement is a product decision, not a bug fix). This is a narrow,
/// additional floor under that design, not a change to it. `DangerFullAccess`
/// is unaffected — the user explicitly opted into "anything goes" there.
///
/// Matched as a case-insensitive substring so `~/.ssh/id_rsa`,
/// `/home/x/.ssh/id_rsa`, and `C:\Users\x\.ssh\id_rsa` (either separator) all
/// match. Deliberately narrow (well-known credential-store paths only) to
/// avoid false-positive-blocking legitimate broad reads.
const SENSITIVE_PATH_FRAGMENTS: &[&str] = &[
    "/.ssh/",
    "\\.ssh\\",
    "/.aws/credentials",
    "\\.aws\\credentials",
    "/.aws/config",
    "\\.aws\\config",
    "/.gnupg/",
    "\\.gnupg\\",
    "/.netrc",
    "\\.netrc",
    "/.git-credentials",
    "\\.git-credentials",
    "/.docker/config.json",
    "\\.docker\\config.json",
    "/.kube/config",
    "\\.kube\\config",
    "/.npmrc",
    "\\.npmrc",
    "/.pypirc",
    "\\.pypirc",
    // Cortex's own trust store — reading it back through a tool call would
    // let a prompt-injected project probe what other paths are trusted.
    "/.cortex/trusted-paths.json",
    "\\.cortex\\trusted-paths.json",
];

/// True if `s` contains a sensitive-credential path fragment (case-insensitive
/// substring match). Shared by the JSON-path walker below and by the raw
/// shell-command check, since a command line embeds its target path as a bare
/// word (`cat ~/.ssh/id_rsa`), not as a JSON path-shaped value `collect_paths`
/// would extract.
fn contains_sensitive_fragment(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    SENSITIVE_PATH_FRAGMENTS.iter().any(|f| lower.contains(f))
}

/// True if any collected path in this payload matches a sensitive-credential
/// fragment. Used to add a floor under otherwise-permitted read paths.
fn touches_sensitive_path(payload_json: &str) -> Option<String> {
    for p in collect_paths(payload_json) {
        if contains_sensitive_fragment(&p) {
            return Some(p);
        }
    }
    None
}

/// Gate a single tool call. Returns `Err(reason)` to deny, `Ok(())` to allow.
/// Caller is responsible for path constraints; this function takes the
/// project root via closure-free args so it stays cheap to call per event.
pub fn tier_allows(
    tier: SandboxTier,
    tool_name: &str,
    payload_json: &str,
    project_root: Option<&Path>,
) -> Result<(), String> {
    match tier {
        SandboxTier::DangerFullAccess => Ok(()),
        SandboxTier::ReadOnly => {
            if name_matches_any(tool_name, READ_TOKENS)
                && !name_matches_any(tool_name, WRITE_TOKENS)
            {
                if let Some(bad) = touches_sensitive_path(payload_json) {
                    return Err(format!(
                        "tier 'read-only' refused: '{bad}' is a credential/secret-store path"
                    ));
                }
                return Ok(());
            }
            // Codex parity: a shell/exec tool may still run under read-only IF
            // the command it carries is provably read-only (`git status`, `ls`,
            // `grep`, `cat`, …). This is the only path that mattered for an
            // *untrusted* project — `chat.rs` forces those to ReadOnly, which
            // otherwise blocked even `git status`. We fail closed: if we can't
            // find a command string or it doesn't classify as read-only, deny.
            let is_exec = name_matches_any(tool_name, &["run_", "exec", "shell", "bash"]);
            if is_exec {
                if let Some(cmd) = super::safe_commands::extract_command(payload_json) {
                    if super::safe_commands::is_read_only_command(&cmd) {
                        // Check both the structured payload (in case a path
                        // also rides along in a sibling field) AND the raw
                        // command string itself, since a shell command embeds
                        // its target as a bare word (`cat ~/.ssh/id_rsa`) that
                        // `collect_paths` (JSON-shaped values only) won't see.
                        if let Some(bad) = touches_sensitive_path(payload_json) {
                            return Err(format!(
                                "tier 'read-only' refused: '{bad}' is a credential/secret-store path"
                            ));
                        }
                        if contains_sensitive_fragment(&cmd) {
                            return Err(format!(
                                "tier 'read-only' refused: command touches a credential/secret-store path: {cmd}"
                            ));
                        }
                        return Ok(());
                    }
                }
            }
            Err(format!(
                "tier 'read-only' forbids tool '{tool_name}' (only read/search/ls/grep/fetch/status/view tools, or a provably read-only shell command, allowed)"
            ))
        }
        SandboxTier::WorkspaceWrite => {
            let is_read = name_matches_any(tool_name, READ_TOKENS)
                && !name_matches_any(tool_name, WRITE_TOKENS);
            let is_write = name_matches_any(tool_name, WRITE_TOKENS);
            if !is_read && !is_write {
                return Err(format!(
                    "tier 'workspace-write' does not classify tool '{tool_name}'"
                ));
            }
            if is_read {
                if let Some(bad) = touches_sensitive_path(payload_json) {
                    return Err(format!(
                        "tier 'workspace-write' refused: '{bad}' is a credential/secret-store path"
                    ));
                }
                return Ok(());
            }
            // Write/exec tool — EVERY path-like value in the payload must be
            // inside root. We validate all of them (not the first match) so a
            // crafted sibling field can't smuggle an out-of-root write past a
            // benign in-root path.
            let Some(root) = project_root else {
                // No project root configured → allow (best-effort; we have
                // nothing to anchor the check against).
                return Ok(());
            };
            let paths = collect_paths(payload_json);
            // Fail closed: a write/exec tool that exposes no recognizable
            // in-root path could be writing somewhere we can't see. We still
            // permit exec-shaped tools (shell/bash/run_*) to carry no path,
            // since their command string isn't a single confined target; pure
            // write/edit tools must name an in-root path.
            if paths.is_empty() {
                let is_exec = name_matches_any(
                    tool_name,
                    &["run_", "exec", "shell", "bash"],
                );
                if is_exec {
                    return Ok(());
                }
                return Err(format!(
                    "tier 'workspace-write' refused: tool '{tool_name}' provides no in-root path to confine"
                ));
            }
            if let Some(bad) = paths.iter().find(|p| !path_inside(root, p)) {
                return Err(format!(
                    "tier 'workspace-write' refused: path '{bad}' escapes project root"
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_stringify_round_trip() {
        for t in [SandboxTier::ReadOnly, SandboxTier::WorkspaceWrite, SandboxTier::DangerFullAccess] {
            assert_eq!(SandboxTier::parse(t.as_str()), Some(t));
        }
        assert_eq!(SandboxTier::parse("READ-ONLY"), Some(SandboxTier::ReadOnly));
        assert_eq!(SandboxTier::parse("nonsense"), None);
    }

    #[test]
    fn load_and_write_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load_tier(tmp.path()), SandboxTier::WorkspaceWrite);
        write_tier(tmp.path(), SandboxTier::ReadOnly).unwrap();
        assert_eq!(load_tier(tmp.path()), SandboxTier::ReadOnly);
        write_tier(tmp.path(), SandboxTier::DangerFullAccess).unwrap();
        assert_eq!(load_tier(tmp.path()), SandboxTier::DangerFullAccess);
    }

    #[test]
    fn read_only_allows_reads_denies_writes() {
        let p: Option<&Path> = None;
        let t = SandboxTier::ReadOnly;
        for name in ["read_file", "fs.search", "grep_files", "view_file"] {
            assert!(tier_allows(t, name, "{}", p).is_ok(), "expected allow: {name}");
        }
        for name in ["write_file", "run_bash", "patch_apply"] {
            assert!(tier_allows(t, name, "{}", p).is_err(), "expected deny: {name}");
        }
    }

    /// A read-shaped tool call targeting a well-known credential path must be
    /// denied even under ReadOnly, even though reads are otherwise not
    /// root-confined by this module's design (see `SENSITIVE_PATH_FRAGMENTS`
    /// doc). Guards against prompt-injection-driven credential exfiltration
    /// via a legitimate-looking `read_file`/`fs.search` call.
    #[test]
    fn read_only_denies_reads_of_sensitive_credential_paths() {
        let p: Option<&Path> = None;
        let t = SandboxTier::ReadOnly;
        let sensitive = [
            "~/.ssh/id_rsa",
            "/home/user/.ssh/id_ed25519",
            "/home/user/.aws/credentials",
            "/home/user/.gnupg/private-keys-v1.d/foo",
            "/home/user/.netrc",
            "/home/user/.git-credentials",
            "/home/user/.docker/config.json",
            "/home/user/.kube/config",
            "/home/user/.cortex/trusted-paths.json",
        ];
        for path in sensitive {
            let payload = serde_json::json!({ "path": path }).to_string();
            assert!(
                tier_allows(t, "read_file", &payload, p).is_err(),
                "expected deny for sensitive path: {path}"
            );
        }
        // An ordinary in-project read is unaffected.
        let payload = serde_json::json!({ "path": "src/main.rs" }).to_string();
        assert!(tier_allows(t, "read_file", &payload, p).is_ok());
    }

    /// Same guard, WorkspaceWrite tier's read branch.
    #[test]
    fn workspace_write_denies_reads_of_sensitive_credential_paths() {
        let p: Option<&Path> = None;
        let t = SandboxTier::WorkspaceWrite;
        let payload = serde_json::json!({ "path": "~/.ssh/authorized_keys" }).to_string();
        assert!(tier_allows(t, "read_file", &payload, p).is_err());
        let payload = serde_json::json!({ "path": "~/.aws/credentials" }).to_string();
        assert!(tier_allows(t, "fs.search", &payload, p).is_err());
    }

    /// A "safe shell command" that's otherwise provably read-only (e.g. `cat`)
    /// must still be denied if its target is a sensitive credential path — the
    /// safe-command allowlist classifies by verb, not by path, so this guard
    /// has to run independently of it.
    #[test]
    fn read_only_denies_safe_shell_command_reading_sensitive_path() {
        let p: Option<&Path> = None;
        let t = SandboxTier::ReadOnly;
        let payload = serde_json::json!({ "cmd": "cat ~/.ssh/id_rsa" }).to_string();
        assert!(tier_allows(t, "shell_exec", &payload, p).is_err());
    }

    /// DangerFullAccess is untouched by this guard — the user explicitly
    /// opted into unrestricted access for that tier.
    #[test]
    fn danger_full_access_ignores_sensitive_path_guard() {
        let p: Option<&Path> = None;
        let t = SandboxTier::DangerFullAccess;
        let payload = serde_json::json!({ "path": "~/.ssh/id_rsa" }).to_string();
        assert!(tier_allows(t, "read_file", &payload, p).is_ok());
    }

    #[test]
    fn read_only_allows_safe_shell_commands_denies_writes() {
        let t = SandboxTier::ReadOnly;
        let p: Option<&Path> = None;
        // A shell/exec tool carrying a provably read-only command is allowed.
        for cmd in ["git status", "ls -la", "grep -rn TODO src", "cat README.md"] {
            let payload = serde_json::json!({ "cmd": cmd }).to_string();
            assert!(
                tier_allows(t, "shell_exec", &payload, p).is_ok(),
                "expected allow under read-only: {cmd}"
            );
        }
        // A mutating / executing command stays denied.
        for cmd in ["rm -rf build", "git push --force", "python evil.py", "cargo build"] {
            let payload = serde_json::json!({ "cmd": cmd }).to_string();
            assert!(
                tier_allows(t, "run_shell", &payload, p).is_err(),
                "expected deny under read-only: {cmd}"
            );
        }
        // An exec tool with no recognizable command fails closed (denied).
        assert!(tier_allows(t, "run_bash", "{}", p).is_err());
    }

    #[test]
    fn workspace_write_requires_path_inside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let t = SandboxTier::WorkspaceWrite;
        // Read tools always allowed; write with no path is allowed.
        assert!(tier_allows(t, "read_file", "{}", Some(root)).is_ok());
        assert!(tier_allows(t, "run_bash", "{}", Some(root)).is_ok());
        // Write path inside root → allow; outside or with .. → deny.
        let inside = serde_json::json!({ "path": root.join("foo.txt") }).to_string();
        assert!(tier_allows(t, "write_file", &inside, Some(root)).is_ok());
        assert!(tier_allows(t, "write_file", r#"{"path":"/etc/passwd"}"#, Some(root)).is_err());
        assert!(tier_allows(t, "write_file", r#"{"path":"../etc/p"}"#, Some(root)).is_err());
    }

    #[test]
    fn danger_full_access_allows_anything() {
        assert!(
            tier_allows(SandboxTier::DangerFullAccess, "rm_rf", r#"{"path":"/"}"#, None)
                .is_ok()
        );
    }

    #[test]
    fn collect_paths_walks_nested_payload() {
        let nested = r#"{"args":{"target":{"path":"/tmp/x"}}}"#;
        assert_eq!(collect_paths(nested), vec!["/tmp/x".to_string()]);
        // A plain non-path command string is not collected.
        let none = r#"{"args":{"cmd":"echo hi"}}"#;
        assert!(collect_paths(none).is_empty());
    }

    #[test]
    fn collect_paths_catches_unrecognized_keys_and_siblings() {
        // Unrecognized key name, but the value is clearly an absolute path.
        let smuggled = r#"{"sneaky_arg":"/etc/passwd"}"#;
        assert_eq!(collect_paths(smuggled), vec!["/etc/passwd".to_string()]);
        // A benign in-root path must NOT mask an out-of-root sibling: both
        // are collected so the gate can reject on the bad one.
        let both = r#"{"path":"/repo/ok.txt","backup":"/etc/shadow"}"#;
        let got = collect_paths(both);
        assert!(got.contains(&"/repo/ok.txt".to_string()));
        assert!(got.contains(&"/etc/shadow".to_string()));
    }

    #[test]
    fn workspace_write_rejects_smuggled_sibling_and_unknown_key() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let t = SandboxTier::WorkspaceWrite;
        // Out-of-root path under an unrecognized key is still caught.
        let unknown = r#"{"whatever":"/etc/passwd"}"#;
        assert!(tier_allows(t, "write_file", unknown, Some(root)).is_err());
        // Benign in-root path alongside an out-of-root sibling → deny.
        let smuggle = format!(
            r#"{{"path":"{}/ok.txt","also":"/etc/shadow"}}"#,
            root.display()
        );
        assert!(tier_allows(t, "write_file", &smuggle, Some(root)).is_err());
        // Pure write tool with no recognizable path → fail closed (deny).
        assert!(tier_allows(t, "write_file", "{}", Some(root)).is_err());
        // Exec-shaped tool with no path is still allowed (command isn't a
        // single confined target).
        assert!(tier_allows(t, "run_bash", "{}", Some(root)).is_ok());
    }
}
