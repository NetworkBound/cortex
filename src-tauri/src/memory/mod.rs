//! Memory layer — reads user's existing memory sources (claude project
//! memory dirs, runbooks, optional Obsidian vault), parses frontmatter +
//! `[[wikilinks]]`, and exposes a uniform `MemoryEntry` to the rest of the
//! app. Writes go through `writer::MemoryWriter` which versions and backs
//! up per-file edits.

pub mod chat_history;
pub mod chroma;
pub mod embed;
pub mod markdown;
pub mod obsidian_rest;
pub mod snapshots;
pub mod sources;
pub mod sync;
pub mod writer;

pub use markdown::MarkdownEntry;
pub use sources::{MemorySource, SourceKind};
