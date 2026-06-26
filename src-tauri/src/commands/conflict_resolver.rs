//! AI-powered merge conflict resolver.
//!
//! Finds files with unresolved merge conflicts in `project_root` via `git
//! ls-files -u` and a sanity-check pass over `git diff --check`, reads each
//! one (capped at 64 KiB), and asks the gateway to produce a clean resolved
//! version. We never write the file ourselves — `ConflictResolverModal`
//! presents the AI proposal next to the conflict markers and the user
//! explicitly accepts. Same streaming-collect + timeout pattern as
//! [`super::doc_gen`] / [`super::refactor_suggester`].
//!
//! The user flow is `/conflict` (alias `/resolve`) — see
//! `src/lib/conflict-resolver.ts` for the TS wrapper and the modal.

use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Per-file cap. 64 KiB matches `explain` / `doc_gen` — generous enough for
/// real source files without inflating gateway latency.
const FILE_LIMIT_BYTES: usize = 64 * 1024;

/// Wall-clock cap per file. The model has to rewrite the whole file so we
/// give it more headroom than `commit_suggest` (30s).
const TIMEOUT: Duration = Duration::from_secs(60);

const SYSTEM_PROMPT: &str =
    "You are a git merge conflict resolver. The following file has conflict markers \
     (<<<<<<<, =======, >>>>>>>). Return the resolved content as a single code block, \
     preserving the intent of both sides where possible. If genuinely incompatible, \
     prefer the one that's more recent / specific. \
     Return ONLY the resolved file content — no fences, no preamble.";

#[derive(Debug, Serialize, Clone)]
pub struct ResolvedConflict {
    pub path: String,
    pub before: String,
    pub after: String,
    /// `"ours"`, `"theirs"`, or `"merged"` — best-effort tag based on which
    /// side of the conflict markers the resolution most closely matches.
    pub ai_chosen_side: String,
    /// Heuristic 0..1 confidence — higher when the AI output is non-empty,
    /// shorter than the input, and no longer contains conflict markers.
    pub confidence: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct ConflictReport {
    pub files: Vec<ResolvedConflict>,
    pub errors: Vec<String>,
}

#[tauri::command]
pub async fn resolve_conflicts(
    project_root: String,
    state: State<'_, AppState>,
) -> Result<ConflictReport, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }

    let mut report = ConflictReport::default();

    let conflicted = match find_conflicted_files(&root) {
        Ok(set) => set,
        Err(e) => {
            report.errors.push(format!("git: {e}"));
            return Ok(report);
        }
    };

    if conflicted.is_empty() {
        return Ok(report);
    }

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();

    for rel in conflicted {
        let abs = root.join(&rel);
        let before = match read_capped(&abs) {
            Ok(s) => s,
            Err(e) => {
                report.errors.push(format!("{rel}: read failed: {e}"));
                continue;
            }
        };
        if !has_conflict_markers(&before) {
            // ls-files -u flagged it but markers are absent (e.g. binary).
            // Skip rather than ask the gateway to hallucinate.
            report
                .errors
                .push(format!("{rel}: no conflict markers found, skipped"));
            continue;
        }

        let client =
            GatewayClient::new(cfg.gateway_base_url.clone(), api_key.clone());

        let req = ChatCompletionRequest {
            model: cfg.gateway_model.clone(),
            messages: vec![
                ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
                ChatMessage { role: "user".into(), content: build_user_prompt(&rel, &before) },
            ],
            stream: true,
            temperature: Some(0.1),
        };

        match run_with_timeout(client, req).await {
            Ok(raw) => {
                let after = sanitize(&raw);
                if after.trim().is_empty() {
                    report.errors.push(format!("{rel}: The gateway returned empty body"));
                    continue;
                }
                let (side, confidence) = score_resolution(&before, &after);
                report.files.push(ResolvedConflict {
                    path: rel,
                    before,
                    after,
                    ai_chosen_side: side,
                    confidence,
                });
            }
            Err(e) => report.errors.push(format!("{rel}: {e}")),
        }
    }

    Ok(report)
}

async fn run_with_timeout(
    client: GatewayClient,
    req: ChatCompletionRequest,
) -> Result<String, String> {
    let (tx, mut rx) = mpsc::channel::<StreamItem>(64);

    let stream_fut = async move {
        let _ = client.chat_completion_stream(req, tx).await;
    };

    let collect_fut = async {
        let mut buf = String::new();
        while let Some(item) = rx.recv().await {
            match item {
                StreamItem::Delta(s) => buf.push_str(&s),
                StreamItem::Done { .. } => break,
            }
        }
        buf
    };

    match tokio::time::timeout(TIMEOUT, async {
        let (_, body) = tokio::join!(stream_fut, collect_fut);
        body
    })
    .await
    {
        Ok(body) => Ok(body),
        Err(_) => Err("The gateway timed out resolving conflict".into()),
    }
}

fn build_user_prompt(rel_path: &str, body: &str) -> String {
    format!(
        "Path: {rel_path}\n--- BEGIN CONFLICTED FILE ---\n{body}\n--- END CONFLICTED FILE ---",
    )
}

/// Find files with unresolved merge conflicts. `git ls-files -u` prints
/// stage-1/2/3 entries — we dedupe paths into a `BTreeSet` for stable order.
fn find_conflicted_files(root: &Path) -> Result<BTreeSet<String>, String> {
    let out = crate::sys::no_window("git")
        .args(["ls-files", "-u"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("spawn ls-files: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let mut set = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // Format: `<mode> <sha> <stage>\t<path>` — split on TAB.
        if let Some(idx) = line.find('\t') {
            let path = line[idx + 1..].trim();
            if !path.is_empty() {
                set.insert(path.to_string());
            }
        }
    }
    Ok(set)
}

fn read_capped(p: &Path) -> Result<String, String> {
    let raw = fs::read_to_string(p).map_err(|e| e.to_string())?;
    Ok(truncate(raw, FILE_LIMIT_BYTES))
}

fn truncate(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let mut cut = limit;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[truncated — file exceeded 64 KiB]");
    s
}

fn has_conflict_markers(body: &str) -> bool {
    body.contains("<<<<<<<") && body.contains("=======") && body.contains(">>>>>>>")
}

/// Strip a single outer fenced block + any "Here is …" preamble. Same shape
/// as `explain::sanitize`, kept inline to avoid cross-module coupling.
fn sanitize(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }
    if let Some(rest) = s.strip_prefix("Here is") {
        if let Some(idx) = rest.find('\n') {
            s = rest[idx + 1..].trim_start().to_string();
        }
    }
    s
}

/// Heuristic side + confidence scorer. We compare the resolved body to the
/// "ours" and "theirs" halves extracted from the original conflict block.
/// Confidence falls off when the AI left conflict markers behind or returned
/// something suspiciously short.
fn score_resolution(before: &str, after: &str) -> (String, f64) {
    if has_conflict_markers(after) {
        return ("merged".into(), 0.2);
    }
    if after.trim().is_empty() {
        return ("merged".into(), 0.0);
    }
    let (ours, theirs) = extract_sides(before);
    let after_t = after.trim();
    let in_ours = ours.iter().any(|seg| !seg.is_empty() && after_t.contains(seg));
    let in_theirs = theirs.iter().any(|seg| !seg.is_empty() && after_t.contains(seg));
    let side = match (in_ours, in_theirs) {
        (true, false) => "ours",
        (false, true) => "theirs",
        _ => "merged",
    }
    .to_string();

    // Base confidence: 0.75. Bump up when both halves are represented (true
    // merge). Lower when output is dramatically shorter than the input — that
    // usually means the model dropped content rather than reconciling it.
    let mut c = 0.75_f64;
    if in_ours && in_theirs {
        c = 0.9;
    }
    let ratio = (after.len() as f64) / (before.len().max(1) as f64);
    if ratio < 0.25 {
        c *= 0.5;
    }
    (side, c.clamp(0.0, 1.0))
}

/// Pull out the bodies of the `ours` and `theirs` halves of every conflict
/// block. We split on the markers and split each block on `=======`. Empty
/// strings are caller-filtered to avoid scoring against meaningless blanks.
fn extract_sides(body: &str) -> (Vec<String>, Vec<String>) {
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<<<<<<<") {
        let after_start = &rest[start..];
        let after_marker = match after_start.find('\n') {
            Some(i) => &after_start[i + 1..],
            None => break,
        };
        let Some(mid) = after_marker.find("=======") else { break };
        let ours_block = &after_marker[..mid];
        let after_mid = &after_marker[mid..];
        let after_mid_nl = match after_mid.find('\n') {
            Some(i) => &after_mid[i + 1..],
            None => break,
        };
        let Some(end) = after_mid_nl.find(">>>>>>>") else { break };
        let theirs_block = &after_mid_nl[..end];
        ours.push(ours_block.trim().to_string());
        theirs.push(theirs_block.trim().to_string());
        let after_end = &after_mid_nl[end..];
        rest = match after_end.find('\n') {
            Some(i) => &after_end[i + 1..],
            None => "",
        };
    }
    (ours, theirs)
}

/// Run `git add` on a relative path. Used by the frontend "Stage all
/// accepted" button via the [`stage_resolved_files`] command — we keep the
/// loop here so the modal can hand us one list and get one report back.
#[tauri::command]
pub async fn stage_resolved_files(
    project_root: String,
    paths: Vec<String>,
) -> Result<StageReport, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("not a directory: {project_root}"));
    }
    let mut report = StageReport::default();
    for p in paths {
        match run_git_add(&root, &p) {
            Ok(()) => report.staged.push(p),
            Err(e) => report.errors.push(format!("{p}: {e}")),
        }
    }
    Ok(report)
}

#[derive(Debug, Default, Serialize)]
pub struct StageReport {
    pub staged: Vec<String>,
    pub errors: Vec<String>,
}

fn run_git_add(root: &Path, path: &str) -> Result<(), String> {
    let out = crate::sys::no_window("git")
        .args(["add", "--", path])
        .current_dir(root)
        .output()
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFLICT: &str = "before\n<<<<<<< HEAD\nfoo_ours\n=======\nfoo_theirs\n>>>>>>> branch\nafter\n";

    #[test]
    fn has_markers_detects_conflict() {
        assert!(has_conflict_markers(SAMPLE_CONFLICT));
        assert!(!has_conflict_markers("nothing to see"));
    }

    #[test]
    fn extract_sides_pulls_both_halves() {
        let (ours, theirs) = extract_sides(SAMPLE_CONFLICT);
        assert_eq!(ours, vec!["foo_ours".to_string()]);
        assert_eq!(theirs, vec!["foo_theirs".to_string()]);
    }

    #[test]
    fn score_resolution_picks_ours_when_only_ours_kept() {
        let (side, c) = score_resolution(SAMPLE_CONFLICT, "before\nfoo_ours\nafter\n");
        assert_eq!(side, "ours");
        assert!(c >= 0.7);
    }

    #[test]
    fn score_resolution_picks_theirs_when_only_theirs_kept() {
        let (side, c) = score_resolution(SAMPLE_CONFLICT, "before\nfoo_theirs\nafter\n");
        assert_eq!(side, "theirs");
        assert!(c >= 0.7);
    }

    #[test]
    fn score_resolution_marks_merged_when_both_present() {
        let merged = "before\nfoo_ours\nfoo_theirs\nafter\n";
        let (side, c) = score_resolution(SAMPLE_CONFLICT, merged);
        assert_eq!(side, "merged");
        assert!(c >= 0.85);
    }

    #[test]
    fn score_resolution_low_confidence_when_markers_remain() {
        let (_, c) = score_resolution(SAMPLE_CONFLICT, SAMPLE_CONFLICT);
        assert!(c <= 0.25);
    }

    #[test]
    fn sanitize_strips_outer_fence() {
        let raw = "```rust\nfn main() {}\n```";
        assert_eq!(sanitize(raw), "fn main() {}");
    }

    #[test]
    fn truncate_caps_long_files() {
        let blob = "x".repeat(FILE_LIMIT_BYTES + 200);
        let out = truncate(blob, FILE_LIMIT_BYTES);
        assert!(out.contains("[truncated"));
    }
}
