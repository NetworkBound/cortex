//! Import pipeline: turn [`ImportedConversation`]s into resumable, searchable
//! Cortex sessions.
//!
//! ## How "resumable" works
//! Each conversation becomes one Cortex session: a deterministic
//! `session-import-<provider>-<hash>` id whose messages are written to the
//! `messages` table via [`TracingStore::record_message`]. The "Recent chats"
//! list (`TracingStore::recent_chat_sessions`) and the desktop/mobile session
//! loaders read straight from that table, so an imported conversation shows up
//! and reopens exactly like a native chat — no schema changes required.
//!
//! ## How "searchable" works
//! Cortex's full-text chat search is `TracingStore::search_messages`, a `LIKE`
//! scan over the same `messages` table (surfaced via the `search_sessions`
//! Tauri command in `commands::observability`). The blended semantic retriever
//! (`retrieval::pipeline`) draws from repo symbols + claude-mem chroma + recent
//! files — it does **not** index the messages table, and there is no public API
//! to push an arbitrary document into the chroma store from here (chroma is
//! populated out-of-band by claude-mem). So the lightest *correct* wiring is:
//! write the imported messages into `messages` the same way a live chat does,
//! which makes them reachable by Cortex's chat search immediately. No separate
//! index call is needed or available. (Documented in the module header and the
//! task report.)
//!
//! ## Idempotency
//! The session id is derived deterministically from `(source, title, first
//! message, message count)`. Before importing, we check whether that session
//! already has rows; if so the conversation is skipped, so re-importing the same
//! export never duplicates history.

use serde::Serialize;

use super::parse::ImportedConversation;
use crate::observability::tracing_store::{StoredMessage, TracingStore};

/// Result of an import run, returned to the Tauri command / HTTP endpoint.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct ImportResult {
    /// Conversations newly written as sessions.
    pub imported: usize,
    /// Conversations skipped (already present, or empty/malformed).
    pub skipped: usize,
    /// The session ids that were created, so the UI can refresh Recent chats.
    pub session_ids: Vec<String>,
}

/// Deterministic, collision-resistant session id for a conversation. Derived
/// from stable content so the *same* conversation always maps to the *same*
/// session id (the basis for idempotency) but two different conversations
/// effectively never collide.
fn derive_session_id(conv: &ImportedConversation) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    conv.source.hash(&mut h);
    conv.title.hash(&mut h);
    conv.messages.len().hash(&mut h);
    if let Some(first) = conv.messages.first() {
        first.content.hash(&mut h);
        first.ts.hash(&mut h);
    }
    if let Some(last) = conv.messages.last() {
        last.content.hash(&mut h);
    }
    // A short provider tag keeps the id human-scannable in logs/DB.
    let tag: String = conv
        .source
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    format!("session-import-{}-{:016x}", tag, h.finish())
}

/// Import a batch of normalized conversations into the tracing store as
/// resumable + searchable sessions. Idempotent per conversation.
///
/// `store` is borrowed; nothing here blocks for long (pure sqlite writes), but
/// the signature is `async` so callers on the tokio runtime compose cleanly and
/// future indexing work can await without an API break.
pub async fn import_conversations(
    convs: Vec<ImportedConversation>,
    store: &TracingStore,
) -> ImportResult {
    let mut result = ImportResult::default();

    for conv in convs {
        if conv.messages.is_empty() {
            result.skipped += 1;
            continue;
        }
        let session_id = derive_session_id(&conv);

        // Idempotency: skip if this session already has messages.
        let (_, existing_count) = store.sum_session_chars(&session_id).unwrap_or((0, 0));
        if existing_count > 0 {
            result.skipped += 1;
            continue;
        }

        // Enforce a monotonic timestamp within the conversation so the Recent
        // chats ordering + transcript replay are stable even when the source
        // omits/duplicates per-message timestamps. Seed from the conversation
        // creation time (or the first real message ts, or 1) and step forward.
        let mut ts = conv
            .created_ts
            .max(conv.messages.first().map(|m| m.ts).unwrap_or(0))
            .max(1);

        let mut written = 0usize;
        for (idx, msg) in conv.messages.iter().enumerate() {
            // Monotonic: never go backwards; advance by at least 1ms per turn.
            ts = msg.ts.max(ts);
            if idx > 0 {
                ts = ts.max(1); // already ensured; keeps the invariant explicit
            }
            let stored = StoredMessage {
                id: format!("{session_id}-{idx}"),
                session_id: session_id.clone(),
                ts,
                role: msg.role.clone(),
                agent_id: Some(format!("import:{}", conv.source)),
                // Prefix the very first message with a provenance banner so the
                // user can tell, in the transcript and in the Recent-chats
                // title/preview, that this is imported history.
                content: if idx == 0 {
                    format!("[Imported from {} — {}]\n\n{}", conv.source, conv.title, msg.content)
                } else {
                    msg.content.clone()
                },
                run_id: None,
                reasoning: None,
                project_root: None,
            };
            if store.record_message(&stored).is_ok() {
                written += 1;
            }
            ts += 1;
        }

        if written == 0 {
            result.skipped += 1;
            continue;
        }
        result.imported += 1;
        result.session_ids.push(session_id);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_import::parse::ImportedMessage;

    fn conv(title: &str, n_msgs: usize) -> ImportedConversation {
        ImportedConversation {
            title: title.to_string(),
            source: "claude.ai".to_string(),
            created_ts: 1_000,
            messages: (0..n_msgs)
                .map(|i| ImportedMessage {
                    role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                    content: format!("message {i} of {title}"),
                    ts: 0, // exercise the monotonic-ts path
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn imports_and_is_searchable_and_recent() {
        let store = TracingStore::in_memory();
        let convs = vec![conv("Alpha", 3), conv("Beta", 2)];
        let res = import_conversations(convs, &store).await;
        assert_eq!(res.imported, 2);
        assert_eq!(res.skipped, 0);
        assert_eq!(res.session_ids.len(), 2);

        // Appears in Recent chats (reads the messages table).
        let recent = store.recent_chat_sessions(50).unwrap();
        assert_eq!(recent.len(), 2);
        // The provenance banner rides the first message → title.
        assert!(recent.iter().any(|r| r.title.contains("Imported from claude.ai")));

        // Searchable via the same messages-table search Cortex uses.
        let hits = store.search_messages("message 1 of Alpha", 10).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|h| h.session_id == res.session_ids[0]));

        // Messages load back in chronological order with monotonic ts.
        let msgs = store.load_session_messages(&res.session_ids[0]).unwrap();
        assert_eq!(msgs.len(), 3);
        for w in msgs.windows(2) {
            assert!(w[1].ts > w[0].ts, "timestamps must be strictly increasing");
        }
    }

    #[tokio::test]
    async fn idempotent_reimport_skips() {
        let store = TracingStore::in_memory();
        let first = import_conversations(vec![conv("Gamma", 4)], &store).await;
        assert_eq!(first.imported, 1);

        // Re-import the identical conversation → skipped, no duplicate session.
        let second = import_conversations(vec![conv("Gamma", 4)], &store).await;
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped, 1);

        let recent = store.recent_chat_sessions(50).unwrap();
        assert_eq!(recent.len(), 1, "re-import must not duplicate the session");
    }

    #[tokio::test]
    async fn empty_conversation_is_skipped() {
        let store = TracingStore::in_memory();
        let res = import_conversations(vec![conv("Empty", 0)], &store).await;
        assert_eq!(res.imported, 0);
        assert_eq!(res.skipped, 1);
    }
}
