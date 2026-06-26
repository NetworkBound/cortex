use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::app_state::AppState;
use crate::memory::markdown;
use serde::Serialize;
use tauri::State;
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
pub struct VaultFolder {
    pub path: String,
    pub note_count: usize,
    pub total_count: usize,
}

#[derive(Debug, Serialize)]
pub struct VaultTag {
    pub tag: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct VaultNote {
    pub path: String,
    pub title: String,
    pub folder: String,
    pub tags: Vec<String>,
    pub link_count: usize,
    pub backlink_count: usize,
    pub size: usize,
    pub is_orphan: bool,
}

#[derive(Debug, Serialize)]
pub struct VaultAnalysis {
    pub total_notes: usize,
    pub total_folders: usize,
    pub total_tags: usize,
    pub orphan_count: usize,
    pub broken_link_count: usize,
    pub folders: Vec<VaultFolder>,
    pub tags: Vec<VaultTag>,
    pub notes: Vec<VaultNote>,
    pub broken_links: Vec<(String, String)>,
}

fn resolve_vault(arg: Option<String>, state: &State<'_, AppState>) -> Option<PathBuf> {
    if let Some(s) = arg {
        if !s.trim().is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    state.config.read().obsidian_vault.clone()
}

fn relative_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn parent_folder(rel: &str) -> String {
    match rel.rfind('/') {
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}

fn normalise_stem(raw: &str) -> String {
    raw.trim().to_lowercase()
}

#[tauri::command]
pub async fn analyze_vault(
    vault_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<VaultAnalysis, String> {
    let root = resolve_vault(vault_path, &state)
        .ok_or_else(|| "No Obsidian vault configured. Set one in Settings.".to_string())?;

    if !root.is_dir() {
        return Err(format!("Vault path does not exist: {}", root.display()));
    }

    let md_files: Vec<PathBuf> = WalkDir::new(&root)
        .max_depth(10)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == "md" || s == "markdown")
        })
        .filter(|e| {
            let p = e.path().to_string_lossy();
            !p.contains(".obsidian") && !p.contains(".trash") && !p.contains("node_modules")
        })
        .map(|e| e.into_path())
        .collect();

    // Parse every note
    struct ParsedNote {
        rel_path: String,
        title: String,
        folder: String,
        tags: Vec<String>,
        wikilinks: Vec<String>,
        size: usize,
        stem: String,
    }

    let mut parsed: Vec<ParsedNote> = Vec::new();
    let mut stem_set: HashSet<String> = HashSet::new();

    for path in &md_files {
        let rel = relative_path(path, &root);
        let folder = parent_folder(&rel);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let norm_stem = normalise_stem(&stem);

        let entry = match markdown::read_entry(path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let title = entry.title.unwrap_or_else(|| stem.clone());
        let size = entry.body.chars().count();

        let tags: Vec<String> = entry
            .frontmatter
            .get("tags")
            .and_then(|v| {
                if let Some(arr) = v.as_array() {
                    Some(
                        arr.iter()
                            .filter_map(|t| t.as_str().map(|s| s.to_string()))
                            .collect(),
                    )
                } else if let Some(s) = v.as_str() {
                    Some(s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        stem_set.insert(norm_stem.clone());
        parsed.push(ParsedNote {
            rel_path: rel,
            title,
            folder,
            tags,
            wikilinks: entry.wikilinks,
            size,
            stem: norm_stem,
        });
    }

    // Build backlink counts and identify broken links
    let mut backlink_counts: HashMap<String, usize> = HashMap::new();
    let mut broken_links: Vec<(String, String)> = Vec::new();

    for note in &parsed {
        for wl in &note.wikilinks {
            let target = wl.split('|').next().unwrap_or(wl);
            let target = target.rsplit('/').next().unwrap_or(target);
            let target = target.trim_end_matches(".md");
            let key = normalise_stem(target);
            let key2 = key.replace(' ', "_");
            let key3 = key.replace(' ', "-");

            if stem_set.contains(&key) || stem_set.contains(&key2) || stem_set.contains(&key3) {
                let resolved = if stem_set.contains(&key) {
                    &key
                } else if stem_set.contains(&key2) {
                    &key2
                } else {
                    &key3
                };
                *backlink_counts.entry(resolved.clone()).or_insert(0) += 1;
            } else {
                broken_links.push((note.rel_path.clone(), target.to_string()));
            }
        }
    }

    // Build folder stats
    let mut folder_note_counts: HashMap<String, usize> = HashMap::new();
    let mut all_folders: HashSet<String> = HashSet::new();
    for note in &parsed {
        *folder_note_counts.entry(note.folder.clone()).or_insert(0) += 1;
        let mut path = note.folder.clone();
        all_folders.insert(path.clone());
        while let Some(i) = path.rfind('/') {
            path = path[..i].to_string();
            all_folders.insert(path.clone());
        }
        if !note.folder.is_empty() {
            all_folders.insert(String::new());
        }
    }

    let mut folders: Vec<VaultFolder> = all_folders
        .iter()
        .map(|f| {
            let note_count = *folder_note_counts.get(f).unwrap_or(&0);
            let total_count = parsed
                .iter()
                .filter(|n| {
                    if f.is_empty() {
                        true
                    } else {
                        n.folder == *f || n.folder.starts_with(&format!("{f}/"))
                    }
                })
                .count();
            VaultFolder {
                path: if f.is_empty() { "/".to_string() } else { f.clone() },
                note_count,
                total_count,
            }
        })
        .collect();
    folders.sort_by(|a, b| b.total_count.cmp(&a.total_count));

    // Build tag stats
    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    for note in &parsed {
        for tag in &note.tags {
            *tag_counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut tags: Vec<VaultTag> = tag_counts
        .into_iter()
        .map(|(tag, count)| VaultTag { tag, count })
        .collect();
    tags.sort_by(|a, b| b.count.cmp(&a.count));

    // Build notes list
    let notes: Vec<VaultNote> = parsed
        .iter()
        .map(|n| {
            let bl = *backlink_counts.get(&n.stem).unwrap_or(&0);
            let is_orphan = n.wikilinks.is_empty() && bl == 0;
            VaultNote {
                path: n.rel_path.clone(),
                title: n.title.clone(),
                folder: n.folder.clone(),
                tags: n.tags.clone(),
                link_count: n.wikilinks.len(),
                backlink_count: bl,
                size: n.size,
                is_orphan,
            }
        })
        .collect();

    let orphan_count = notes.iter().filter(|n| n.is_orphan).count();
    let total_folders = folders.len().saturating_sub(1); // exclude root

    Ok(VaultAnalysis {
        total_notes: notes.len(),
        total_folders,
        total_tags: tags.len(),
        orphan_count,
        broken_link_count: broken_links.len(),
        folders,
        tags,
        notes,
        broken_links,
    })
}
