//! Skills subsystem — Anthropic-style declarative "skills" loaded from
//! `~/.cortex/skills/<name>/SKILL.md`. Each skill is a Markdown file with a
//! YAML frontmatter header that declares its name, description and a list of
//! input variables; the body is a Handlebars-style template (`{{var}}`).
//!
//! The flow is:
//!   1. `loader::load_skills` walks the skills dir and returns every parseable
//!      skill (malformed files are skipped, never panic — the panel stays usable
//!      even when one bad file is sitting in the directory).
//!   2. The frontend renders a form per skill from `inputs`.
//!   3. `runner::expand_skill` looks up a single skill by name, substitutes
//!      `{{var}}` markers from a caller-provided map and returns the expanded
//!      body. Unknown vars are an error so the user notices typos instead of
//!      seeing a half-expanded prompt slip into chat.
//!
//! Storage lives at `~/.cortex/skills/<name>/SKILL.md` (same convention as the
//! Claude-Flow / Anthropic skills directory). The wrapping folder lets a skill
//! ship with sidecar assets later (icons, examples, etc.) without forcing a
//! schema bump here.

pub mod loader;
pub mod runner;

pub use loader::{load_skills, load_skill_by_name, skills_root, Skill, SkillInput};
pub use runner::expand_skill;
