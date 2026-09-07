//! Read-only access to claude-mem's chroma sqlite DB at
//! `~/.claude-mem/chroma/chroma.sqlite3`. Phase 3 exposes a simple "find by
//! substring" against the embedded docs table — semantic search via vector
//! similarity is a Phase 3.5 stretch (needs an embedding call we'd rather
//! delegate to claude-mem itself).

use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct ChromaHit {
    pub id: String,
    pub document: String,
    pub metadata: Option<String>,
}

pub fn chroma_db_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let p = home.join(".claude-mem").join("chroma").join("chroma.sqlite3");
    p.exists().then_some(p)
}

pub fn substring_search(needle: &str, limit: usize) -> anyhow::Result<Vec<ChromaHit>> {
    let Some(path) = chroma_db_path() else {
        return Ok(Vec::new());
    };
    let conn = Connection::open(path)?;
    let mut hits = Vec::new();

    // Chroma's sqlite schema has changed across versions. Try the modern
    // `embeddings_queue` / `embedding_fulltext_search` shape first; fall
    // back to scanning `embedding_metadata` if not present.
    let try_modern = conn.prepare(
        "SELECT id, document FROM embeddings_queue WHERE document LIKE ?1 ESCAPE '\\' LIMIT ?2",
    );
    if let Ok(mut stmt) = try_modern {
        // Escape LIKE wildcards (`%`, `_`) and the escape char itself so the
        // user-supplied needle is matched literally rather than as a pattern.
        let escaped = needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{}%", escaped);
        let rows = stmt.query_map(rusqlite::params![pattern, limit], |r| {
            Ok(ChromaHit {
                id: r.get::<_, String>(0).unwrap_or_default(),
                document: r.get::<_, String>(1).unwrap_or_default(),
                metadata: None,
            })
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() { hits.push(row); }
            return Ok(hits);
        }
    }

    // Fallback: list tables, give a debug hit so the UI shows something useful
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    hits.push(ChromaHit {
        id: "schema-info".into(),
        document: format!("chroma tables: {}", names.join(", ")),
        metadata: None,
    });

    Ok(hits)
}
