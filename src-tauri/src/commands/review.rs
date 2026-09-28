//! AI code review of the working tree (`/review`).
//!
//! Collects the project's uncommitted diff (or the diff against a base
//! branch), redacts secrets, caps it per-file and in total, and asks a model
//! for structured findings. The reviewer can be a *different* model than the
//! one that wrote the code: when the caller passes no explicit `agent`, we
//! pick a capable catalog model from another provider family than
//! `author_model` (the session's current model) so the "Claude writes, GPT
//! reviews" pattern works out of the box.
//!
//! Model output is parsed leniently — prose around the JSON, code fences and
//! a bare array are all tolerated — and when nothing parses the raw text is
//! surfaced as a single `info` finding so the user never loses the review.
//!
//! Mirrors the [`super::explain`] shape (small command + pure helpers +
//! unit tests) and reuses [`crate::agents::oneshot::complete_resilient`] so a
//! transient provider blip retries/falls back like every other helper feature.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::State;

use crate::agents::adapter::AgentCapability;
use crate::agents::oneshot;
use crate::app_state::AppState;
use crate::orchestrator::cost_router;

/// Hard cap on the whole diff we send to the model.
const DIFF_LIMIT_BYTES: usize = 64 * 1024;
/// Per-file cap so one generated lockfile can't crowd out real changes.
const PER_FILE_LIMIT_BYTES: usize = 16 * 1024;
/// Wall clock on the model call. Reviews are heavier than explanations.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Severity keys, strongest first. Used for both normalisation and ordering.
const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "info"];

/// Reviewer preference when picking a cross-model reviewer: strongest
/// generalist first. Only entries the registry can actually reach are used.
const REVIEWER_PREFERENCE: [&str; 6] = [
    "gpt-5.5",
    "claude-opus-4-8",
    "gemini-3.1-pro-preview",
    "gpt-5.4",
    "claude-sonnet-4-6",
    "gemini-3-pro-preview",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewFinding {
    /// One of `critical` | `high` | `medium` | `low` | `info`.
    pub severity: String,
    /// Repo-relative path (forward slashes, as git prints them), or `""`
    /// for findings that don't belong to one file.
    pub file: String,
    pub line: Option<u32>,
    pub title: String,
    pub detail: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewReport {
    /// `None` = uncommitted changes vs HEAD; `Some(ref)` = vs that base.
    pub base: Option<String>,
    /// Model slug that produced the review (or the adapter id when the
    /// default route answered).
    pub model: String,
    pub agent_id: String,
    /// `true` when the reviewer was chosen to differ from `author_model`.
    pub cross_model: bool,
    pub fell_back: bool,
    pub summary: Option<String>,
    /// Sorted strongest severity first, then by file/line.
    pub findings: Vec<ReviewFinding>,
    /// Files present in the (capped) diff.
    pub files: Vec<String>,
    pub diff_bytes: usize,
    /// `true` when any per-file or total cap kicked in.
    pub truncated: bool,
    /// `true` when the model text could not be parsed as findings — the
    /// single `info` finding then carries the raw output.
    pub unparsed: bool,
    pub latency_ms: i64,
}

#[tauri::command]
pub async fn review_diff(
    project_root: String,
    base: Option<String>,
    agent: Option<String>,
    author_model: Option<String>,
    state: State<'_, AppState>,
) -> Result<ReviewReport, String> {
    let started = Instant::now();
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let base = base.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    if let Some(b) = &base {
        validate_base_ref(b)?;
    }

    let raw_diff = collect_diff(&root, base.as_deref())?;
    if raw_diff.trim().is_empty() {
        return Err(match &base {
            Some(b) => format!("no changes against `{b}` — nothing to review"),
            None => "no uncommitted changes — nothing to review".to_string(),
        });
    }
    let redacted = crate::redact::redact_text(&raw_diff);
    let capped = cap_diff(&redacted, PER_FILE_LIMIT_BYTES, DIFF_LIMIT_BYTES);
    let prompt = build_prompt(&capped.text, base.as_deref());

    // Reviewer selection: explicit `agent` wins; otherwise pick a capable
    // model from another provider family than the author's.
    let (reviewer, cross_model) = match agent
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
    {
        Some(a) => (Some(a), false),
        None => {
            let picked = {
                let reg = state.registry.read();
                let cands: Vec<String> =
                    cost_router::candidates(&[AgentCapability::Chat], &reg, &[])
                        .into_iter()
                        .map(|c| c.model)
                        .collect();
                pick_reviewer(author_model.as_deref(), &cands)
            };
            let cross = picked.is_some();
            (picked, cross)
        }
    };

    let outcome = tokio::time::timeout(
        TIMEOUT,
        oneshot::complete_resilient(&state.registry, reviewer.clone(), prompt),
    )
    .await
    .map_err(|_| format!("review timed out after {}s", TIMEOUT.as_secs()))??;

    let parsed = parse_review(&outcome.text);
    Ok(ReviewReport {
        base,
        model: reviewer
            .or(outcome.model)
            .unwrap_or_else(|| outcome.agent_id.clone()),
        agent_id: outcome.agent_id,
        cross_model,
        fell_back: outcome.fell_back,
        summary: parsed.summary,
        findings: parsed.findings,
        files: capped.files,
        diff_bytes: capped.text.len(),
        truncated: capped.truncated,
        unparsed: parsed.unparsed,
        latency_ms: started.elapsed().as_millis() as i64,
    })
}

// ----- git ---------------------------------------------------------------

/// Reject anything that could be read by git as an option (`--output=…`) or
/// that isn't a plausible ref name. Refs are passed after `--no-color` but we
/// still refuse leading dashes and whitespace/control characters outright.
fn validate_base_ref(base: &str) -> Result<(), String> {
    if base.starts_with('-') {
        return Err(format!("invalid base ref `{base}`"));
    }
    if base.chars().any(|c| {
        c.is_whitespace() || c.is_control() || matches!(c, '~' | '^' | ':' | '\\' | '*' | '?' | '[')
    }) {
        return Err(format!("invalid base ref `{base}`"));
    }
    if base.contains("..") {
        return Err(format!(
            "invalid base ref `{base}` — pass a single branch or commit"
        ));
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    crate::sys::no_window("git")
        .args(args)
        .current_dir(root)
        .envs(crate::commands::git::NON_INTERACTIVE_ENV.iter().copied())
        .output()
        .map_err(|e| format!("git: spawn failed: {e}"))
}

/// `git diff HEAD` for the working tree, or the working tree against the
/// merge-base of `base` and HEAD (so only this branch's changes show up,
/// including uncommitted ones). Falls back to diffing against `base`
/// directly when there is no merge-base (unrelated histories, shallow clone).
fn collect_diff(root: &Path, base: Option<&str>) -> Result<String, String> {
    let target: String = match base {
        None => "HEAD".to_string(),
        Some(b) => {
            let mb = git(root, &["merge-base", b, "HEAD"])?;
            if mb.status.success() {
                let s = String::from_utf8_lossy(&mb.stdout).trim().to_string();
                if s.is_empty() {
                    b.to_string()
                } else {
                    s
                }
            } else {
                b.to_string()
            }
        }
    };
    let out = git(
        root,
        &["diff", "--no-color", "--no-ext-diff", target.as_str(), "--"],
    )?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "git diff failed".to_string()
        } else {
            format!("git diff failed: {err}")
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ----- diff capping ------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct CappedDiff {
    text: String,
    files: Vec<String>,
    truncated: bool,
}

/// Split a unified diff into `(path, chunk)` pairs on `diff --git` headers.
/// The path is taken from the `b/` side so renames/new files report their
/// current name. Text before the first header (rare) is dropped.
fn split_diff_by_file(diff: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = header_path(rest.trim_end());
            out.push((path, line.to_string()));
        } else if let Some((_, chunk)) = out.last_mut() {
            chunk.push_str(line);
        }
    }
    out
}

/// `a/foo b/foo` → `foo`. Git quotes paths with special characters; we keep
/// the quoted form as-is rather than guessing at unescaping.
fn header_path(rest: &str) -> String {
    if let Some(idx) = rest.find(" b/") {
        return rest[idx + 3..].to_string();
    }
    rest.split_whitespace()
        .last()
        .map(|s| s.trim_start_matches("b/").to_string())
        .unwrap_or_default()
}

fn truncate_at_boundary(s: &str, limit: usize) -> &str {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// Apply the per-file and total caps. Each truncated file ends with an
/// explicit note so the model (and the user) knows the tail is missing;
/// files past the total cap are listed by name only.
fn cap_diff(diff: &str, per_file: usize, total: usize) -> CappedDiff {
    let parts = split_diff_by_file(diff);
    if parts.is_empty() {
        // Not a `diff --git` stream (unexpected) — cap it as one blob.
        let cut = truncate_at_boundary(diff, total);
        let truncated = cut.len() < diff.len();
        let mut text = cut.to_string();
        if truncated {
            text.push_str("\n[truncated — diff exceeded the size cap]\n");
        }
        return CappedDiff {
            text,
            files: Vec::new(),
            truncated,
        };
    }
    let mut text = String::new();
    let mut files = Vec::with_capacity(parts.len());
    let mut truncated = false;
    let mut omitted: Vec<String> = Vec::new();
    for (path, chunk) in parts {
        files.push(path.clone());
        let mut piece = truncate_at_boundary(&chunk, per_file).to_string();
        if piece.len() < chunk.len() {
            truncated = true;
            if !piece.ends_with('\n') {
                piece.push('\n');
            }
            piece.push_str(&format!(
                "[truncated — {path} exceeded {} KiB; remainder not shown]\n",
                per_file / 1024
            ));
        }
        if text.len() + piece.len() > total {
            truncated = true;
            omitted.push(path);
            continue;
        }
        text.push_str(&piece);
    }
    if !omitted.is_empty() {
        text.push_str(&format!(
            "\n[truncated — {} more changed file(s) not shown: {}]\n",
            omitted.len(),
            omitted.join(", ")
        ));
    }
    CappedDiff {
        text,
        files,
        truncated,
    }
}

// ----- prompt ------------------------------------------------------------

fn build_prompt(diff: &str, base: Option<&str>) -> String {
    let scope = match base {
        Some(b) => {
            format!("the changes on this branch relative to `{b}` (including uncommitted edits)")
        }
        None => "the uncommitted changes in the working tree".to_string(),
    };
    format!(
        "You are a meticulous senior code reviewer. Review {scope} below.\n\
         Focus on real problems: bugs, logic errors, unhandled errors, security issues, \
         race conditions, resource leaks, broken cross-platform (Windows/Linux) behaviour, \
         and misleading names or comments. Do not comment on formatting. Do not praise. \
         Only report findings you are confident about; an empty list is a valid answer.\n\n\
         Respond with ONLY a JSON object of this exact shape and nothing else:\n\
         {{\"summary\": \"one or two sentences\", \"findings\": [{{\"severity\": \"critical|high|medium|low|info\", \
         \"file\": \"path/as/in/diff\", \"line\": 123, \"title\": \"short title\", \
         \"detail\": \"what is wrong and why\", \"suggestion\": \"concrete fix, or null\"}}]}}\n\
         `line` is the line number in the NEW version of the file (from the @@ hunk headers), or null.\n\n\
         --- DIFF ---\n{diff}\n--- END DIFF ---"
    )
}

// ----- parsing -----------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct RawReview {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    findings: Vec<RawFinding>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFinding {
    #[serde(default)]
    severity: Option<String>,
    #[serde(default, alias = "path")]
    file: Option<String>,
    #[serde(default)]
    line: Option<serde_json::Value>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, alias = "description", alias = "message")]
    detail: Option<String>,
    #[serde(default, alias = "fix")]
    suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct ParsedReview {
    summary: Option<String>,
    findings: Vec<ReviewFinding>,
    unparsed: bool,
}

/// Rank of a severity key (0 = strongest). Unknown → `info`.
fn severity_rank(sev: &str) -> usize {
    SEVERITIES
        .iter()
        .position(|s| *s == sev)
        .unwrap_or(SEVERITIES.len() - 1)
}

/// Fold model-invented severities (`error`, `warning`, `blocker`, `nit`…)
/// onto the five canonical keys.
fn normalize_severity(raw: Option<&str>) -> String {
    let key = raw
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match key.as_str() {
        "critical" | "blocker" | "severe" | "p0" => "critical",
        "high" | "error" | "major" | "p1" => "high",
        "medium" | "moderate" | "warning" | "warn" | "p2" => "medium",
        "low" | "minor" | "nit" | "nitpick" | "p3" => "low",
        _ => "info",
    }
    .to_string()
}

fn line_from_value(v: Option<&serde_json::Value>) -> Option<u32> {
    let n = match v? {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        serde_json::Value::String(s) => {
            // "12", "12-15", "L12", "12:3" → 12
            let digits: String = s
                .trim()
                .trim_start_matches(['L', 'l'])
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<u32>().ok()
        }
        _ => None,
    };
    n.filter(|n| *n > 0)
}

/// Strip a single outer ``` fence (with optional language tag).
fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let body = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
    body.rfind("```")
        .map(|end| &body[..end])
        .unwrap_or(body)
        .trim()
}

/// Find the outermost JSON object or array in chatty model output.
fn extract_json(s: &str) -> Option<&str> {
    let obj = s.find('{');
    let arr = s.find('[');
    let (open, close) = match (obj, arr) {
        (Some(o), Some(a)) if a < o => (a, ']'),
        (Some(o), _) => (o, '}'),
        (None, Some(a)) => (a, ']'),
        (None, None) => return None,
    };
    let end = s.rfind(close)?;
    (end > open).then(|| &s[open..=end])
}

fn parse_review(raw: &str) -> ParsedReview {
    let body = strip_fence(raw);
    let parsed: Option<RawReview> = extract_json(body).and_then(|json| {
        serde_json::from_str::<RawReview>(json).ok().or_else(|| {
            serde_json::from_str::<Vec<RawFinding>>(json)
                .ok()
                .map(|findings| RawReview {
                    summary: None,
                    findings,
                })
        })
    });
    let Some(review) = parsed else {
        let text = raw.trim();
        if text.is_empty() {
            return ParsedReview {
                summary: None,
                findings: Vec::new(),
                unparsed: false,
            };
        }
        return ParsedReview {
            summary: None,
            findings: vec![ReviewFinding {
                severity: "info".into(),
                file: String::new(),
                line: None,
                title: "Unparsed review output".into(),
                detail: text.to_string(),
                suggestion: None,
            }],
            unparsed: true,
        };
    };
    let mut findings: Vec<ReviewFinding> = review
        .findings
        .into_iter()
        .filter_map(|f| {
            let title = f.title.map(|t| t.trim().to_string()).unwrap_or_default();
            let detail = f.detail.map(|d| d.trim().to_string()).unwrap_or_default();
            if title.is_empty() && detail.is_empty() {
                return None;
            }
            Some(ReviewFinding {
                severity: normalize_severity(f.severity.as_deref()),
                file: f
                    .file
                    .map(|p| p.trim().trim_start_matches("b/").replace('\\', "/"))
                    .unwrap_or_default(),
                line: line_from_value(f.line.as_ref()),
                title: if title.is_empty() {
                    detail.chars().take(80).collect()
                } else {
                    title
                },
                detail,
                suggestion: f
                    .suggestion
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null")),
            })
        })
        .collect();
    findings.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.unwrap_or(0).cmp(&b.line.unwrap_or(0)))
    });
    ParsedReview {
        summary: review
            .summary
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        findings,
        unparsed: false,
    }
}

// ----- reviewer selection ------------------------------------------------

/// Provider family of a model slug: `claude-opus-4-8` → `claude`,
/// `ollama:llama3` → `ollama`, `gpt-5.5` → `gpt`.
fn model_family(slug: &str) -> String {
    let s = slug.trim().to_ascii_lowercase();
    s.split(|c| c == '-' || c == ':' || c == '/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Pick a reviewer from `candidates` (reachable model slugs) that belongs to
/// a different provider family than `author`. Walks [`REVIEWER_PREFERENCE`]
/// first, then any other candidate, so the choice is deterministic for a
/// given registry state. Returns `None` (→ default route) when the author is
/// unknown or nothing from another family is reachable.
fn pick_reviewer(author: Option<&str>, candidates: &[String]) -> Option<String> {
    let author = author.map(str::trim).filter(|a| !a.is_empty())?;
    let family = model_family(author);
    let differs = |c: &str| model_family(c) != family && c != author;
    REVIEWER_PREFERENCE
        .iter()
        .find(|p| candidates.iter().any(|c| c.as_str() == **p) && differs(*p))
        .map(|p| (*p).to_string())
        .or_else(|| candidates.iter().find(|c| differs(c.as_str())).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n hi\n+there\n";

    #[test]
    fn split_diff_by_file_finds_paths_and_chunks() {
        let parts = split_diff_by_file(SAMPLE);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "src/a.rs");
        assert!(parts[0].1.contains("+new"));
        assert_eq!(parts[1].0, "README.md");
        assert!(parts[1].1.ends_with("+there\n"));
    }

    #[test]
    fn header_path_handles_spaces_and_nested_dirs() {
        assert_eq!(header_path("a/x y.rs b/x y.rs"), "x y.rs");
        assert_eq!(
            header_path("a/src/lib/b/c.rs b/src/lib/b/c.rs"),
            "src/lib/b/c.rs"
        );
    }

    #[test]
    fn cap_diff_keeps_small_diffs_intact() {
        let c = cap_diff(SAMPLE, PER_FILE_LIMIT_BYTES, DIFF_LIMIT_BYTES);
        assert_eq!(c.text, SAMPLE);
        assert!(!c.truncated);
        assert_eq!(c.files, vec!["src/a.rs", "README.md"]);
    }

    #[test]
    fn cap_diff_truncates_per_file_with_note() {
        let big = format!(
            "diff --git a/big.lock b/big.lock\n{}\ndiff --git a/small.rs b/small.rs\n+ok\n",
            "+x".repeat(3000)
        );
        let c = cap_diff(&big, 1024, DIFF_LIMIT_BYTES);
        assert!(c.truncated);
        assert!(c.text.contains("[truncated — big.lock exceeded 1 KiB"));
        assert!(c.text.contains("diff --git a/small.rs b/small.rs\n+ok"));
        assert_eq!(c.files, vec!["big.lock", "small.rs"]);
    }

    #[test]
    fn cap_diff_lists_files_past_total_cap() {
        let mut diff = String::new();
        for i in 0..5 {
            diff.push_str(&format!(
                "diff --git a/f{i}.rs b/f{i}.rs\n{}\n",
                "+y".repeat(300)
            ));
        }
        let c = cap_diff(&diff, 4096, 1500);
        assert!(c.truncated);
        assert!(c.text.contains("more changed file(s) not shown"));
        assert!(c.text.contains("f4.rs"));
        assert_eq!(c.files.len(), 5);
        assert!(c.text.len() < 1500 + 400);
    }

    #[test]
    fn cap_diff_without_headers_caps_as_blob() {
        let blob = "é".repeat(2000);
        let c = cap_diff(&blob, 512, 1001);
        assert!(c.truncated);
        assert!(c.files.is_empty());
        assert!(c.text.starts_with("é"));
    }

    #[test]
    fn build_prompt_mentions_scope_and_diff() {
        let p = build_prompt("+x", None);
        assert!(p.contains("uncommitted changes"));
        assert!(p.contains("--- DIFF ---\n+x\n--- END DIFF ---"));
        assert!(p.contains("\"findings\""));
        let p2 = build_prompt("+x", Some("main"));
        assert!(p2.contains("relative to `main`"));
    }

    #[test]
    fn parse_review_accepts_clean_json() {
        let raw = r#"{"summary":"Looks ok","findings":[{"severity":"high","file":"src/a.rs","line":12,"title":"Unwrap on user input","detail":"panics","suggestion":"use ?"}]}"#;
        let r = parse_review(raw);
        assert!(!r.unparsed);
        assert_eq!(r.summary.as_deref(), Some("Looks ok"));
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "high");
        assert_eq!(r.findings[0].line, Some(12));
        assert_eq!(r.findings[0].suggestion.as_deref(), Some("use ?"));
    }

    #[test]
    fn parse_review_tolerates_prose_and_fences() {
        let raw = "Sure! Here is the review:\n```json\n{\"findings\":[{\"severity\":\"warning\",\"path\":\"b/x.ts\",\"line\":\"L7\",\"title\":\"t\",\"description\":\"d\"}]}\n```\nHope this helps.";
        let r = parse_review(raw);
        assert!(!r.unparsed);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "medium");
        assert_eq!(r.findings[0].file, "x.ts");
        assert_eq!(r.findings[0].line, Some(7));
        assert_eq!(r.findings[0].detail, "d");
    }

    #[test]
    fn parse_review_accepts_bare_array() {
        let raw = "[{\"severity\":\"low\",\"file\":\"a\",\"title\":\"x\",\"detail\":\"y\"}]";
        let r = parse_review(raw);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "low");
    }

    #[test]
    fn parse_review_falls_back_to_unparsed_finding() {
        let r = parse_review("I could not review this diff.");
        assert!(r.unparsed);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, "info");
        assert_eq!(r.findings[0].detail, "I could not review this diff.");
        let empty = parse_review("   ");
        assert!(!empty.unparsed);
        assert!(empty.findings.is_empty());
    }

    #[test]
    fn parse_review_sorts_by_severity_then_location() {
        let raw = r#"{"findings":[
            {"severity":"low","file":"b.rs","line":3,"title":"c","detail":"d"},
            {"severity":"critical","file":"z.rs","line":9,"title":"a","detail":"d"},
            {"severity":"low","file":"a.rs","line":30,"title":"b","detail":"d"},
            {"severity":"low","file":"a.rs","line":2,"title":"b2","detail":"d"},
            {"severity":"","title":"","detail":""}
        ]}"#;
        let r = parse_review(raw);
        let order: Vec<&str> = r.findings.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(order, vec!["a", "b2", "b", "c"]);
    }

    #[test]
    fn normalize_severity_maps_aliases() {
        assert_eq!(normalize_severity(Some("Blocker")), "critical");
        assert_eq!(normalize_severity(Some("error")), "high");
        assert_eq!(normalize_severity(Some("warning")), "medium");
        assert_eq!(normalize_severity(Some("nit")), "low");
        assert_eq!(normalize_severity(Some("whatever")), "info");
        assert_eq!(normalize_severity(None), "info");
    }

    #[test]
    fn line_from_value_handles_shapes() {
        use serde_json::json;
        assert_eq!(line_from_value(Some(&json!(5))), Some(5));
        assert_eq!(line_from_value(Some(&json!("12-15"))), Some(12));
        assert_eq!(line_from_value(Some(&json!(0))), None);
        assert_eq!(line_from_value(Some(&json!(null))), None);
        assert_eq!(line_from_value(None), None);
    }

    #[test]
    fn extract_json_prefers_outermost_container() {
        assert_eq!(extract_json("x {\"a\":1} y"), Some("{\"a\":1}"));
        assert_eq!(extract_json("[1,{\"a\":2}] tail"), Some("[1,{\"a\":2}]"));
        assert_eq!(extract_json("nothing"), None);
    }

    #[test]
    fn pick_reviewer_prefers_other_family() {
        let cands: Vec<String> = [
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "gpt-5.5",
            "gemini-3.1-pro-preview",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            pick_reviewer(Some("claude-sonnet-4-6"), &cands).as_deref(),
            Some("gpt-5.5")
        );
        assert_eq!(
            pick_reviewer(Some("gpt-5.4"), &cands).as_deref(),
            Some("claude-opus-4-8")
        );
        // Only same-family candidates reachable → None (default route).
        let only_claude = vec!["claude-opus-4-8".to_string()];
        assert_eq!(pick_reviewer(Some("claude-sonnet-4-6"), &only_claude), None);
        // Unknown author → None.
        assert_eq!(pick_reviewer(None, &cands), None);
        // Non-preferred candidate from another family still wins over nothing.
        let odd = vec!["ollama:qwen3".to_string()];
        assert_eq!(
            pick_reviewer(Some("claude-opus-4-8"), &odd).as_deref(),
            Some("ollama:qwen3")
        );
    }

    #[test]
    fn model_family_splits_on_separators() {
        assert_eq!(model_family("claude-opus-4-8"), "claude");
        assert_eq!(model_family("ollama:llama3.2:1b"), "ollama");
        assert_eq!(model_family("GPT-5.5"), "gpt");
    }

    #[test]
    fn validate_base_ref_rejects_options_and_ranges() {
        assert!(validate_base_ref("main").is_ok());
        assert!(validate_base_ref("origin/main").is_ok());
        assert!(validate_base_ref("feature/x_y-1").is_ok());
        assert!(validate_base_ref("--output=/tmp/x").is_err());
        assert!(validate_base_ref("main..dev").is_err());
        assert!(validate_base_ref("ma in").is_err());
        assert!(validate_base_ref("HEAD~1").is_err());
        assert!(validate_base_ref("a\\b").is_err());
    }
}
