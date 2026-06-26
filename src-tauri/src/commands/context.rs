//! Aider-style /tokens breakdown.
//!
//! Counts characters in each component that contributes to the model's
//! context window so the user can see *where* their tokens go before
//! complaining about pricing or compaction.
//!
//! Char counts are converted to a rough token estimate at chars / 4
//! (a decent approximation for both OpenAI and Anthropic tokenisers —
//! exact tokenisation is intentionally out of scope: no new deps).

use crate::observability::tracing_store::TracingStore;
use crate::projects::diagnostics::{self, Diagnostic};
use crate::projects::rules;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

/// Per-component context spend for a single session. Each `*_chars` field
/// counts characters (UTF-8 chars, not bytes) and `total_estimated_tokens`
/// is the sum divided by 4.
#[derive(Debug, Clone, Serialize)]
pub struct ContextBreakdown {
    pub system_chars: usize,
    pub claude_md_chars: usize,
    pub rules_chars: usize,
    pub repo_map_chars: usize,
    pub history_chars: usize,
    pub history_message_count: usize,
    pub attached_files_chars: usize,
    pub total_estimated_tokens: usize,
}

impl ContextBreakdown {
    fn recompute_total(&mut self) {
        let total_chars = self.system_chars
            + self.claude_md_chars
            + self.rules_chars
            + self.repo_map_chars
            + self.history_chars
            + self.attached_files_chars;
        self.total_estimated_tokens = total_chars / 4;
    }
}

/// Estimate the context breakdown for `session_id`, optionally scoped to a
/// project root so we can tally CLAUDE.md / .cortex/rules. History and
/// attached-file char counts come from the local SQLite messages table
/// when it exists; otherwise they are 0 and the frontend supplements them
/// from its in-memory store.
#[tauri::command]
pub async fn estimate_context_breakdown(
    session_id: String,
    project_root: Option<String>,
    store: State<'_, TracingStore>,
) -> Result<ContextBreakdown, String> {
    let project = project_root.as_deref().map(PathBuf::from);
    let (system_chars, claude_md_chars, rules_chars) = match project.as_deref() {
        Some(p) => project_context_chars(p),
        None => (0, 0, 0),
    };

    // History + attached-files come from SQLite when available. Both are
    // best-effort: a missing `messages` table is the expected case in
    // builds where session persistence isn't wired yet.
    let (history_chars, history_message_count) =
        store.sum_session_chars(&session_id).unwrap_or((0, 0));
    let attached_files_chars = store
        .latest_assistant_content(&session_id)
        .ok()
        .flatten()
        .map(|content| sum_attached_file_sizes(&content, project.as_deref()))
        .unwrap_or(0);

    let mut out = ContextBreakdown {
        system_chars,
        claude_md_chars,
        rules_chars,
        repo_map_chars: 0, // detection is out of scope per spec.
        history_chars,
        history_message_count,
        attached_files_chars,
        total_estimated_tokens: 0,
    };
    out.recompute_total();
    Ok(out)
}

/// Tally chars across the auto-loaded project context: the small system
/// preamble, each top-level instruction file (CLAUDE.md and friends), and
/// any `.cortex/rules/*.md` entries. Mirrors `sessions::gather_project_context`
/// truncation rules so the numbers match what actually ends up in the prompt.
fn project_context_chars(project: &Path) -> (usize, usize, usize) {
    // System: the same header + closing note that gather_project_context emits.
    // We count chars of the static skeleton so the user sees a non-zero baseline.
    let project_name = project
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let header = format!(
        "# Cortex project session — {}\n\nWorking directory: `{}`\n",
        project_name,
        project.display(),
    );
    let footer = "---\nThis context was auto-loaded by Cortex. Ask anything about this project — your CLAUDE.md instructions, runbooks, and memory are all available to me. Type @ to insert any project file.";
    let system_chars = header.chars().count() + footer.chars().count();

    // CLAUDE.md and siblings — each capped at 8000 chars to match gather_project_context.
    let mut claude_md_chars = 0usize;
    for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md", "README.md"] {
        if let Ok(body) = std::fs::read_to_string(project.join(name)) {
            claude_md_chars += body.chars().take(8000).count();
        }
    }

    // .cortex/rules/*.md — count only the rules that would actually be
    // injected at session bootstrap (i.e. `alwaysApply`). Glob / description /
    // manual rules don't contribute to the baseline since they depend on the
    // user message. Bodies are already capped at 4000 chars by `load_rules`.
    let all_rules = rules::load_rules(project);
    let rules_chars: usize = rules::select_active(&all_rules, "")
        .iter()
        .map(|r| r.body.chars().count())
        .sum();

    (system_chars, claude_md_chars, rules_chars)
}

/// Parse `@file:path` references from a message and sum the on-disk byte
/// sizes of each, capped at 50_000 bytes (≈50KB) per file to match the
/// effective truncation cap an attachment pipeline would apply.
///
/// Paths are confined to `project_root`: a reference is only stat'd if it
/// canonicalises to a location *inside* the canonicalised project root. This
/// stops a crafted `@file:` ref (absolute path, `../` traversal, or a symlink
/// escape) from probing arbitrary files on disk via `fs::metadata`. When no
/// project root is supplied we can't confine, so nothing is counted.
fn sum_attached_file_sizes(content: &str, project_root: Option<&Path>) -> usize {
    const PER_FILE_CAP: u64 = 50_000;
    let Some(root) = project_root else {
        return 0;
    };
    // Canonicalise the root once so symlinks/`..` in candidate paths are
    // resolved against a real, absolute base before the containment check.
    let Ok(root) = std::fs::canonicalize(root) else {
        return 0;
    };
    let mut total: usize = 0;
    for token in content.split_whitespace() {
        let Some(raw) = token.strip_prefix("@file:") else {
            continue;
        };
        // Trim trailing punctuation so `@file:foo.rs.` resolves to foo.rs.
        let path_str = raw.trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ')' | ']'));
        if path_str.is_empty() {
            continue;
        }
        // Resolve relative refs against the project root, then canonicalise so
        // symlinks and `..` segments are fully collapsed before we check
        // containment. A path that doesn't exist (or can't be resolved) is
        // skipped — same effect as the old `metadata` failing.
        let candidate = root.join(path_str);
        let Ok(resolved) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if !resolved.starts_with(&root) {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&resolved) {
            let size = meta.len().min(PER_FILE_CAP) as usize;
            total += size;
        }
    }
    total
}

// ---------- /web URL → markdown (Aider-style) ----------
//
// User-driven web fetch: `/web https://…` in the composer pulls the page,
// converts the body to a stripped-down markdown blob, and returns it to the
// frontend so the user can paste it into chat or reference it as `@web:url`.
// Intentionally simple — no JS execution, no follow-redirects-forever. Caps
// the response at 256 KiB so a single command can't blow the context window.

#[derive(Debug, Clone, Serialize)]
pub struct FetchedPage {
    pub url: String,
    pub title: Option<String>,
    pub markdown: String,
    pub fetched_unix_ms: i64,
    pub truncated: bool,
}

const FETCH_LIMIT_BYTES: usize = 256 * 1024;

/// Reject hosts that resolve to non-public IPs (SSRF guard).
///
/// Extracts the host[:port] authority from an http(s) URL string, resolves it
/// via the system resolver, and fails if *any* resolved address is private,
/// loopback, link-local, unspecified, multicast, or the cloud metadata range
/// (`169.254.169.254`). Resolving here — rather than trusting the literal —
/// also blocks raw-IP and DNS-to-internal tricks. Self-contained: std only, no
/// `url` crate dependency.
fn guard_public_host(raw_url: &str) -> Result<(), String> {
    use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

    // Strip scheme, then take the authority up to the first '/', '?' or '#'.
    let after_scheme = raw_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(raw_url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Drop any userinfo ("user:pass@host").
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host_port.is_empty() {
        return Err("could not parse host from URL".into());
    }

    // Split host / port, handling bracketed IPv6 literals (`[::1]:443`).
    let (host, port): (String, u16) = if let Some(rest) = host_port.strip_prefix('[') {
        let (h, after) = rest
            .split_once(']')
            .ok_or_else(|| "malformed IPv6 host in URL".to_string())?;
        let p = after
            .strip_prefix(':')
            .and_then(|s| s.parse().ok())
            .unwrap_or(80);
        (h.to_string(), p)
    } else if let Some((h, p)) = host_port.rsplit_once(':') {
        // Only treat as host:port when the suffix is numeric; otherwise it's
        // an IPv6 literal without brackets / a malformed host.
        match p.parse::<u16>() {
            Ok(p) => (h.to_string(), p),
            Err(_) => (host_port.to_string(), 80),
        }
    } else {
        (host_port.to_string(), 80)
    };

    // A nested fn (not a closure) so the IPv4-mapped branch can recurse into it.
    fn is_blocked(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_multicast()
                    || v4.is_unspecified()
                    || v4.is_documentation()
                    || v4.octets()[0] == 0
                    // CGNAT 100.64.0.0/10
                    || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
                    // cloud metadata
                    || v4 == Ipv4Addr::new(169, 254, 169, 254)
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_multicast()
                    || v6.is_unspecified()
                    // unique-local fc00::/7
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    // link-local fe80::/10
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // IPv4-mapped — re-check the embedded v4
                    || v6.to_ipv4_mapped().map(IpAddr::V4).is_some_and(is_blocked)
            }
        }
    }

    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve host: {e}"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if is_blocked(addr.ip()) {
            return Err("refusing to fetch a private, loopback, or internal address".into());
        }
    }
    if !any {
        return Err("host did not resolve to any address".into());
    }
    Ok(())
}

/// Fetch `url` and convert the body to plain markdown-ish text. http/https
/// only; other schemes are rejected so users can't accidentally fetch
/// `file:///etc/passwd`. Hosts resolving to private/internal IPs are blocked
/// (SSRF guard) and redirects are *not* followed so a public URL can't bounce
/// the request to an internal one. Strips `<script>` / `<style>` content and
/// collapses HTML tags to text — no full HTML parser dependency.
#[tauri::command]
pub async fn fetch_url(url: String) -> Result<FetchedPage, String> {
    let url = url.trim();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("only http/https URLs are supported".into());
    }
    guard_public_host(url)?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("Cortex/", env!("CARGO_PKG_VERSION"), " (+local)"))
        .timeout(std::time::Duration::from_secs(20))
        // Don't auto-follow redirects: a 30x to an internal host would bypass
        // the SSRF guard above. Surface redirects as an error instead.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("http client init failed: {e}"))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    let status = resp.status();
    if status.is_redirection() {
        return Err(format!(
            "refusing to follow redirect (HTTP {status}); re-issue /web against the final URL"
        ));
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| format!("read body failed: {e}"))?;
    let truncated = body.len() > FETCH_LIMIT_BYTES;
    // Clamp the cut to a UTF-8 char boundary so slicing never panics
    // mid-codepoint when FETCH_LIMIT_BYTES lands inside a multibyte char.
    let body = if truncated {
        let mut end = FETCH_LIMIT_BYTES;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        &body[..end]
    } else {
        &body[..]
    };

    let title = extract_title(body);
    let markdown = html_to_markdown(body);
    let fetched_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    Ok(FetchedPage {
        url: url.to_string(),
        title,
        markdown,
        fetched_unix_ms,
        truncated,
    })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after_open = lower[start..].find('>')? + start + 1;
    let end = lower[after_open..].find("</title>")?;
    let title = html[after_open..after_open + end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Coarse HTML→text/markdown: drops `<script>`/`<style>`/`<noscript>` blocks
/// entirely, strips all other tags, decodes a handful of common entities,
/// and collapses whitespace. Good enough for docs/RFCs/blog posts; not a
/// full DOM converter.
fn html_to_markdown(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    // Remove <script>, <style>, <noscript> content.
    let mut work = html.to_string();
    for tag in ["script", "style", "noscript"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        loop {
            let lw = work.to_ascii_lowercase();
            let Some(o) = lw.find(&open) else { break };
            let Some(c) = lw[o..].find(&close) else { break };
            let end = o + c + close.len();
            work.replace_range(o..end, "");
        }
    }
    let _ = lower; // explicit unused (kept for potential future use)
    // Replace block-level tags with newlines to preserve paragraph breaks.
    let block_breaks = [
        "<br", "<p", "</p>", "<div", "</div>", "<li", "</li>", "</tr>", "<tr",
        "<h1", "</h1>", "<h2", "</h2>", "<h3", "</h3>", "<h4", "</h4>",
    ];
    for b in block_breaks {
        let lw = work.to_ascii_lowercase();
        let mut replaced = String::with_capacity(work.len());
        let mut i = 0usize;
        while let Some(pos) = lw[i..].find(b) {
            let abs = i + pos;
            replaced.push_str(&work[i..abs]);
            replaced.push('\n');
            // skip past the matched fragment so we replace each occurrence
            i = abs + b.len();
        }
        replaced.push_str(&work[i..]);
        work = replaced;
    }
    // Drop remaining tags <…> entirely.
    let mut out = String::with_capacity(work.len());
    let mut inside_tag = false;
    for ch in work.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            c if !inside_tag => out.push(c),
            _ => {}
        }
    }
    // Decode a handful of common entities.
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // Collapse runs of blank lines.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_blank = false;
    for line in out.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !prev_blank {
                collapsed.push('\n');
            }
            prev_blank = true;
        } else {
            collapsed.push_str(t);
            collapsed.push('\n');
            prev_blank = false;
        }
    }
    collapsed.trim().to_string()
}

// ---------- @diff / @problems / @terminal (Continue-style providers) ----------
//
// Three @-vocab backends lifted from Continue. Each is a small, synchronous
// shell-out that returns a snapshot — no daemons, no file watchers.

/// Hard cap on the diff blob returned to the frontend. 32 KiB is more than
/// enough to surface "what's changed" without blowing the context window.
const DIFF_LIMIT_BYTES: usize = 32 * 1024;

/// Run `git diff --no-color HEAD` in `project_root` and return the unified
/// diff as one big string, truncated at [`DIFF_LIMIT_BYTES`]. An empty
/// string is returned if there are no changes or git isn't available — the
/// frontend should treat both cases the same.
#[tauri::command]
pub async fn git_working_diff(project_root: String) -> Result<String, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let output = crate::sys::no_window("git")
        .args(["diff", "--no-color", "HEAD"])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git: spawn failed: {e}"))?;
    if !output.status.success() {
        // No diff / not a repo / git missing — surface an empty string so
        // the UI just shows "no changes" rather than a hard error.
        return Ok(String::new());
    }
    let mut diff = String::from_utf8_lossy(&output.stdout).into_owned();
    if diff.len() > DIFF_LIMIT_BYTES {
        // Truncate on a char boundary so we never split a multibyte codepoint.
        let mut cut = DIFF_LIMIT_BYTES;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        diff.truncate(cut);
        diff.push_str("\n[truncated — diff exceeded 32 KiB]");
    }
    Ok(diff)
}

/// Run the project's compilers in check-only mode and return up to 100
/// diagnostics. Cached for 30s per project root inside the
/// [`crate::projects::diagnostics`] module.
#[tauri::command]
pub async fn project_diagnostics(project_root: String) -> Result<Vec<Diagnostic>, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    Ok(diagnostics::collect(&root))
}

/// Number of trailing lines from the terminal log to surface.
const TERMINAL_TAIL_LINES: usize = 200;

/// Core of [`recent_terminal_output`] and the chat `@terminal` provider:
/// return the last `max_lines` of `<home>/.cortex/last-shell-output.log`, or
/// `None` when the file is absent, unreadable, or empty. Factored out so the
/// Tauri command and the `@terminal` resolver in `commands::chat` share one
/// implementation (and one test) rather than each reaching for `$HOME`.
pub fn read_terminal_tail(home: &Path, max_lines: usize) -> Option<String> {
    let path = home.join(".cortex").join("last-shell-output.log");
    if !path.exists() {
        return None;
    }
    let body = std::fs::read_to_string(&path).ok()?;
    let mut lines: Vec<&str> = body.lines().collect();
    if lines.len() > max_lines {
        let skip = lines.len() - max_lines;
        lines = lines.split_off(skip);
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Return the last 200 lines of `~/.cortex/last-shell-output.log`, or `None`
/// if the file doesn't exist. The monitors subsystem (see `monitors.rs`)
/// emits live events but doesn't keep a buffer; users who want a tail can
/// redirect their shell or `tee` output into this file.
#[tauri::command]
pub async fn recent_terminal_output() -> Result<Option<String>, String> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    Ok(read_terminal_tail(&home, TERMINAL_TAIL_LINES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_is_sum_over_four() {
        let mut b = ContextBreakdown {
            system_chars: 100,
            claude_md_chars: 200,
            rules_chars: 300,
            repo_map_chars: 0,
            history_chars: 400,
            history_message_count: 0,
            attached_files_chars: 0,
            total_estimated_tokens: 0,
        };
        b.recompute_total();
        // 1000 chars / 4 = 250 tokens
        assert_eq!(b.total_estimated_tokens, 250);
    }

    #[test]
    fn terminal_tail_reads_caps_and_handles_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Absent log → None.
        assert!(read_terminal_tail(home, 200).is_none());

        let cortex = home.join(".cortex");
        std::fs::create_dir_all(&cortex).unwrap();
        let log = cortex.join("last-shell-output.log");

        // Empty file → None (no lines).
        std::fs::write(&log, "").unwrap();
        assert!(read_terminal_tail(home, 200).is_none());

        // 10 lines, ask for the last 3 → only the tail is returned, in order.
        let body: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        std::fs::write(&log, &body).unwrap();
        let tail = read_terminal_tail(home, 3).expect("tail present");
        assert_eq!(tail, "line 8\nline 9\nline 10");

        // max_lines beyond the file length returns everything.
        let all = read_terminal_tail(home, 999).expect("tail present");
        assert_eq!(all.lines().count(), 10);
        assert!(all.starts_with("line 1\n"));
    }

    #[test]
    fn project_context_handles_missing_dir() {
        let bogus = Path::new("/definitely/not/a/real/path/cortex-test");
        let (sys, claude, rules) = project_context_chars(bogus);
        // header+footer skeleton is still emitted (the project name is empty),
        // but no files contribute.
        assert!(sys > 0);
        assert_eq!(claude, 0);
        assert_eq!(rules, 0);
    }

    #[test]
    fn attached_files_ignores_unknown_refs() {
        let content = "see @file:/no/such/file.rs and @file:/also/missing.ts";
        // A real, canonicalisable root so confinement runs; the refs still
        // don't exist inside it, so nothing is counted.
        assert_eq!(sum_attached_file_sizes(content, Some(Path::new("/tmp"))), 0);
    }

    #[test]
    fn guard_blocks_loopback_and_private_and_metadata() {
        assert!(guard_public_host("http://127.0.0.1/").is_err());
        assert!(guard_public_host("http://localhost:8080/x").is_err());
        assert!(guard_public_host("http://10.0.0.194:3000/foo").is_err());
        assert!(guard_public_host("http://10.0.0.5/").is_err());
        assert!(guard_public_host("http://169.254.169.254/latest/meta-data").is_err());
        assert!(guard_public_host("http://[::1]:443/").is_err());
        // userinfo must not let an internal host slip past parsing.
        assert!(guard_public_host("http://user:pass@127.0.0.1/").is_err());
    }

    #[test]
    fn guard_allows_public_host() {
        // Uses a stable documentation domain; resolution may fail offline, in
        // which case we accept the resolve error but never a "blocked" verdict.
        if let Err(e) = guard_public_host("https://example.com/") {
            assert!(
                e.contains("resolve"),
                "public host should not be blocked, got: {e}"
            );
        }
    }

    #[test]
    fn attached_files_strips_trailing_punct() {
        // Hard to test real file sizes hermetically — just confirm the parser
        // doesn't blow up on punctuation-terminated refs.
        let content = "see @file:/tmp/none.rs. and @file:/tmp/none2.ts,";
        assert_eq!(sum_attached_file_sizes(content, Some(Path::new("/tmp"))), 0);
    }
}
