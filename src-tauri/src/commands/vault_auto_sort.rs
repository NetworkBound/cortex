use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

use super::vault_analysis::analyze_vault;

const TIMEOUT: Duration = Duration::from_secs(120);

const SYSTEM_PROMPT: &str = "\
You are an Obsidian vault organizer. Analyze the vault structure provided and output \
a clear, actionable report with specific suggestions in these categories:

1. **Folder Moves** — notes that belong in a different folder based on their content/tags.
2. **Tag Cleanup** — redundant, misspelled, or inconsistent tags to merge or rename.
3. **Orphan Resolution** — orphan notes (no links in or out) that should be linked to existing notes.
4. **Link Suggestions** — pairs of notes that should link to each other based on topic overlap.
5. **Folder Structure** — new folders to create or empty folders to remove.

Be specific: use exact note paths and names. Prioritize the highest-impact changes first. \
Keep each suggestion to one line. Group by category with markdown headers. \
If the vault is already well-organized, say so briefly.";

#[derive(Debug, Serialize)]
pub struct AutoSortResult {
    pub suggestions: String,
}

#[tauri::command]
pub async fn vault_auto_sort(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let analysis = analyze_vault(None, state.clone()).await?;

    let mut prompt = String::with_capacity(8192);
    prompt.push_str(&format!(
        "## Vault Overview\n- {} notes, {} folders, {} tags\n- {} orphans, {} broken links\n\n",
        analysis.total_notes,
        analysis.total_folders,
        analysis.total_tags,
        analysis.orphan_count,
        analysis.broken_link_count,
    ));

    prompt.push_str("## Folders\n");
    for f in &analysis.folders {
        prompt.push_str(&format!("- `{}` — {} direct, {} total\n", f.path, f.note_count, f.total_count));
    }

    if !analysis.tags.is_empty() {
        prompt.push_str("\n## Tags\n");
        for t in analysis.tags.iter().take(50) {
            prompt.push_str(&format!("- `#{}` ({})\n", t.tag, t.count));
        }
    }

    prompt.push_str("\n## Notes\n");
    for n in &analysis.notes {
        let tags_str = if n.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", n.tags.join(", "))
        };
        prompt.push_str(&format!(
            "- `{}` — {}{} | {}out/{}in{}\n",
            n.path,
            n.title,
            tags_str,
            n.link_count,
            n.backlink_count,
            if n.is_orphan { " [ORPHAN]" } else { "" },
        ));
    }

    if !analysis.broken_links.is_empty() {
        prompt.push_str("\n## Broken Links\n");
        for (source, target) in analysis.broken_links.iter().take(50) {
            prompt.push_str(&format!("- `{}` -> [[{}]]\n", source, target));
        }
    }

    prompt.push_str("\nAnalyze and provide specific reorganization suggestions.");

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: SYSTEM_PROMPT.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: prompt,
            },
        ],
        stream: true,
        temperature: Some(0.3),
    };

    let (tx, mut rx) = mpsc::channel::<StreamItem>(128);
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
        Ok(body) => {
            if body.trim().is_empty() {
                Err("AI returned an empty response".into())
            } else {
                Ok(body)
            }
        }
        Err(_) => Err("Auto-sort analysis timed out after 120s".into()),
    }
}
