//! PRP loader — reads/writes `<project_root>/.cortex/prps/<name>.md` files with
//! YAML frontmatter (`name`, `status`, `created_unix_ms`, `stages`, `gates`).
//!
//! Design notes
//! ------------
//! * Slug validation (`[A-Za-z0-9_.-]+`, ≤64 chars) keeps `create_prp` /
//!   `update_prp_stage` from escaping the prps directory.
//! * Bad files are silently skipped from `load_prps` so the panel stays usable
//!   if a user hand-edits a malformed file.
//! * `update_prp_stage` is implemented as a parse → mutate → re-serialize cycle
//!   rather than a string search, so we don't corrupt other frontmatter keys.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use yaml_front_matter::YamlFrontMatter;

/// One of the four lifecycle stages a PRP advances through.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PrpStage {
    #[serde(rename = "stage-1")]
    Stage1,
    #[serde(rename = "stage-2")]
    Stage2,
    #[serde(rename = "stage-3")]
    Stage3,
    #[serde(rename = "stage-4")]
    Stage4,
}

impl PrpStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrpStage::Stage1 => "stage-1",
            PrpStage::Stage2 => "stage-2",
            PrpStage::Stage3 => "stage-3",
            PrpStage::Stage4 => "stage-4",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "stage-1" => Some(Self::Stage1),
            "stage-2" => Some(Self::Stage2),
            "stage-3" => Some(Self::Stage3),
            "stage-4" => Some(Self::Stage4),
            _ => None,
        }
    }

    /// Returns the next stage in the lifecycle, or `None` if already at stage-4.
    pub fn next(self) -> Option<Self> {
        match self {
            PrpStage::Stage1 => Some(PrpStage::Stage2),
            PrpStage::Stage2 => Some(PrpStage::Stage3),
            PrpStage::Stage3 => Some(PrpStage::Stage4),
            PrpStage::Stage4 => None,
        }
    }
}

/// Gate status map — keys are gate names (`syntax`, `tests`, `coverage`,
/// `build`, `security`), values are `pending | pass | fail | skipped`.
pub type GateStatuses = BTreeMap<String, String>;

/// Outbound shape for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prp {
    pub name: String,
    pub status: PrpStage,
    #[serde(default)]
    pub created_unix_ms: i64,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub gates: GateStatuses,
    pub body: String,
    /// Absolute path on disk — handy for the UI to display.
    pub path: String,
}

/// Internal: raw frontmatter shape we parse out of the file.
#[derive(Debug, Deserialize, Serialize)]
struct Frontmatter {
    name: String,
    status: PrpStage,
    #[serde(default)]
    created_unix_ms: i64,
    #[serde(default)]
    stages: Vec<String>,
    #[serde(default)]
    gates: GateStatuses,
}

/// Slug validation — same shape as skills.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        // Reject dot-only names like "." or ".." — they don't traverse but
        // would create odd/hidden files.
        && !name.chars().all(|c| c == '.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Resolve the `.cortex/prps/` directory inside a project root.
pub fn prps_dir_for(project_root: &Path) -> PathBuf {
    project_root.join(".cortex").join("prps")
}

fn file_for(project_root: &Path, name: &str) -> Result<PathBuf, String> {
    if !is_valid_name(name) {
        return Err(format!("invalid PRP name '{name}'"));
    }
    Ok(prps_dir_for(project_root).join(format!("{name}.md")))
}

fn default_gates() -> GateStatuses {
    let mut g = GateStatuses::new();
    for key in ["syntax", "tests", "coverage", "build", "security"] {
        g.insert(key.to_string(), "pending".to_string());
    }
    g
}

fn default_stages() -> Vec<String> {
    vec![
        "stage-1: Spec drafted".into(),
        "stage-2: Plan validated".into(),
        "stage-3: Implementation".into(),
        "stage-4: Test + verify".into(),
    ]
}

fn parse_file(path: &Path) -> Option<Prp> {
    let text = fs::read_to_string(path).ok()?;
    let doc = YamlFrontMatter::parse::<Frontmatter>(&text).ok()?;
    let fm = doc.metadata;
    if !is_valid_name(&fm.name) {
        return None;
    }
    Some(Prp {
        name: fm.name,
        status: fm.status,
        created_unix_ms: fm.created_unix_ms,
        stages: fm.stages,
        gates: fm.gates,
        body: doc.content,
        path: path.to_string_lossy().into_owned(),
    })
}

/// Serialize a PRP back to disk as `<frontmatter>---\n<body>`.
fn write_prp(path: &Path, fm: &Frontmatter, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let yaml = serde_yaml::to_string(fm).map_err(|e| format!("yaml serialize failed: {e}"))?;
    let mut out = String::with_capacity(yaml.len() + body.len() + 16);
    out.push_str("---\n");
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    // Ensure exactly one blank line between frontmatter and body.
    let trimmed = body.trim_start_matches('\n');
    out.push('\n');
    out.push_str(trimmed);
    fs::write(path, out).map_err(|e| format!("write failed: {e}"))
}

fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn template_body(goal_hint: &str) -> String {
    let goal = if goal_hint.is_empty() {
        "_Describe what this PRP delivers._".to_string()
    } else {
        goal_hint.to_string()
    };
    format!(
        "# Goal\n{goal}\n\n# Gotchas\n- _List anything subtle here._\n\n# Curated docs\n- _e.g. @docs/architecture.md_\n\n# Acceptance\n- _List measurable acceptance criteria._\n"
    )
}

/// Walk the project's prps dir and return every parseable PRP.
pub fn load_prps(project_root: &Path) -> Vec<Prp> {
    let root = prps_dir_for(project_root);
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<Prp> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if let Some(prp) = parse_file(&path) {
            out.push(prp);
        }
    }
    // Stable sort by created time (newest first), then name as tiebreaker.
    out.sort_by(|a, b| {
        b.created_unix_ms
            .cmp(&a.created_unix_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Convenience wrapper used by commands — accepts `Vec<Prp>` directly.
pub fn list_prps(project_root: &Path) -> Vec<Prp> {
    load_prps(project_root)
}

/// Load a single PRP by name.
pub fn get_prp(project_root: &Path, name: &str) -> Option<Prp> {
    let path = file_for(project_root, name).ok()?;
    if !path.is_file() {
        return None;
    }
    parse_file(&path)
}

/// Create a new PRP at stage-1. Errors if the file already exists so we don't
/// silently clobber an existing spec.
pub fn create_prp(project_root: &Path, name: &str, body_hint: &str) -> Result<Prp, String> {
    let path = file_for(project_root, name)?;
    if path.exists() {
        return Err(format!("PRP '{name}' already exists"));
    }
    let fm = Frontmatter {
        name: name.to_string(),
        status: PrpStage::Stage1,
        created_unix_ms: now_unix_ms(),
        stages: default_stages(),
        gates: default_gates(),
    };
    let body = template_body(body_hint);
    write_prp(&path, &fm, &body)?;
    parse_file(&path).ok_or_else(|| "failed to read back PRP after write".to_string())
}

/// Advance the stage of an existing PRP. Errors if the file is missing.
pub fn update_prp_stage(project_root: &Path, name: &str, stage: PrpStage) -> Result<(), String> {
    let path = file_for(project_root, name)?;
    if !path.is_file() {
        return Err(format!("PRP '{name}' not found"));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
    let doc = YamlFrontMatter::parse::<Frontmatter>(&text)
        .map_err(|e| format!("parse failed: {e}"))?;
    let mut fm = doc.metadata;
    fm.status = stage;
    write_prp(&path, &fm, &doc.content)
}

/// Persist a new gate-statuses map onto an existing PRP. Used by `run_gates`.
pub fn update_prp_gates(
    project_root: &Path,
    name: &str,
    gates: GateStatuses,
) -> Result<(), String> {
    let path = file_for(project_root, name)?;
    if !path.is_file() {
        return Err(format!("PRP '{name}' not found"));
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
    let doc = YamlFrontMatter::parse::<Frontmatter>(&text)
        .map_err(|e| format!("parse failed: {e}"))?;
    let mut fm = doc.metadata;
    fm.gates = gates;
    write_prp(&path, &fm, &doc.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn name_validation() {
        assert!(is_valid_name("add-redis-cache"));
        assert!(is_valid_name("a.b_c-1"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("../escape"));
        assert!(!is_valid_name("has space"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }

    #[test]
    fn create_then_load_roundtrip() {
        let dir = tempdir().unwrap();
        let prp = create_prp(dir.path(), "add-cache", "Add a Redis cache.").unwrap();
        assert_eq!(prp.name, "add-cache");
        assert_eq!(prp.status, PrpStage::Stage1);
        assert_eq!(prp.gates.len(), 5);
        assert!(prp.body.contains("Add a Redis cache"));

        let all = load_prps(dir.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "add-cache");
    }

    #[test]
    fn duplicate_create_errors() {
        let dir = tempdir().unwrap();
        create_prp(dir.path(), "foo", "x").unwrap();
        assert!(create_prp(dir.path(), "foo", "x").is_err());
    }

    #[test]
    fn advance_stage() {
        let dir = tempdir().unwrap();
        create_prp(dir.path(), "foo", "x").unwrap();
        update_prp_stage(dir.path(), "foo", PrpStage::Stage2).unwrap();
        let p = get_prp(dir.path(), "foo").unwrap();
        assert_eq!(p.status, PrpStage::Stage2);
    }

    #[test]
    fn stage_next_chain() {
        assert_eq!(PrpStage::Stage1.next(), Some(PrpStage::Stage2));
        assert_eq!(PrpStage::Stage4.next(), None);
    }

    #[test]
    fn bad_file_is_skipped() {
        let dir = tempdir().unwrap();
        let prps_dir = prps_dir_for(dir.path());
        fs::create_dir_all(&prps_dir).unwrap();
        fs::write(prps_dir.join("garbage.md"), "no frontmatter").unwrap();
        assert!(load_prps(dir.path()).is_empty());
    }

    #[test]
    fn update_gates_preserves_body() {
        let dir = tempdir().unwrap();
        create_prp(dir.path(), "foo", "BODY HINT").unwrap();
        let mut gates = GateStatuses::new();
        gates.insert("syntax".into(), "pass".into());
        gates.insert("tests".into(), "fail".into());
        gates.insert("coverage".into(), "skipped".into());
        gates.insert("build".into(), "pass".into());
        gates.insert("security".into(), "pending".into());
        update_prp_gates(dir.path(), "foo", gates).unwrap();
        let p = get_prp(dir.path(), "foo").unwrap();
        assert_eq!(p.gates.get("tests").map(|s| s.as_str()), Some("fail"));
        assert!(p.body.contains("BODY HINT"));
    }
}
