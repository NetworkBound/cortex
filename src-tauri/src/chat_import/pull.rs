//! EXPERIMENTAL live pullers for Claude.ai and ChatGPT.
//!
//! ⚠️ **READ THIS.** These talk to **unofficial, undocumented** web endpoints
//! using a user's *session credential*. They are:
//!   - **Fragile** — the endpoints/shapes are internal and change without
//!     notice; expect breakage.
//!   - **ToS-gray** — automated access to these private APIs may violate the
//!     providers' terms of service. The user supplies their own token and bears
//!     that responsibility; Cortex makes no warranty.
//!   - **Best-effort** — every failure path returns a human-readable `Err`
//!     string rather than panicking, and partial results are kept.
//!
//! **The token is never logged.** Error messages deliberately exclude the
//! credential and any header values derived from it.
//!
//! The pulled JSON is fed through the *same* parsers in [`super::parse`] as the
//! file-import path, so the live and offline shapes converge on one model. The
//! unit test here only exercises that shape-mapping (offline); there is **no**
//! live network test.

use super::parse::{parse_claude, parse_chatgpt, ImportedConversation};

/// A browser-like UA. The internal endpoints reject obviously-automated
/// clients; this is cosmetic, not auth.
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Cap how many conversations a single pull will fetch in detail, so a huge
/// account doesn't hammer the provider or stall the import for minutes.
const MAX_CONVERSATIONS: usize = 200;

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "failed to build HTTP client".to_string())
}

// ───────────────────────────────────────────────────────────────────────────
// Claude.ai
// ───────────────────────────────────────────────────────────────────────────

/// EXPERIMENTAL: pull conversations from claude.ai using a `sessionKey` cookie.
///
/// Flow: `GET /api/organizations` → org uuid → `GET
/// /api/organizations/{org}/chat_conversations` (list) → per conversation `GET
/// .../chat_conversations/{uuid}?tree=True&rendering_mode=raw` → reuse
/// [`parse_claude`] on the per-conversation shape (it already understands
/// `chat_messages` with `text` / `content` blocks).
///
/// `session_key` is the value of the `sessionKey` cookie (a logged-in
/// claude.ai browser session). Never logged.
pub async fn pull_claude(session_key: &str) -> Result<Vec<ImportedConversation>, String> {
    if session_key.trim().is_empty() {
        return Err("empty session key".to_string());
    }
    let client = client()?;
    let cookie = format!("sessionKey={session_key}");

    // 1) Discover the org uuid.
    let orgs: serde_json::Value = client
        .get("https://claude.ai/api/organizations")
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .map_err(|_| "claude.ai: request to /api/organizations failed (network or blocked)".to_string())?
        .error_for_status()
        .map_err(|e| format!("claude.ai: /api/organizations returned {}", status_of(&e)))?
        .json()
        .await
        .map_err(|_| "claude.ai: could not parse organizations response (shape changed?)".to_string())?;

    let org_uuid = orgs
        .as_array()
        .and_then(|a| a.first())
        .and_then(|o| o.get("uuid"))
        .and_then(|u| u.as_str())
        .ok_or_else(|| "claude.ai: no organization uuid in response (shape changed?)".to_string())?
        .to_string();

    // 2) List conversations (summaries).
    let list: serde_json::Value = client
        .get(format!(
            "https://claude.ai/api/organizations/{org_uuid}/chat_conversations"
        ))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .map_err(|_| "claude.ai: chat_conversations list request failed".to_string())?
        .error_for_status()
        .map_err(|e| format!("claude.ai: chat_conversations returned {}", status_of(&e)))?
        .json()
        .await
        .map_err(|_| "claude.ai: could not parse conversation list".to_string())?;

    let uuids: Vec<String> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("uuid").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .take(MAX_CONVERSATIONS)
                .collect()
        })
        .unwrap_or_default();

    // 3) Fetch each conversation's full tree and parse it. A single failed
    //    conversation is skipped, not fatal.
    let mut out = Vec::new();
    for uuid in uuids {
        let url = format!(
            "https://claude.ai/api/organizations/{org_uuid}/chat_conversations/{uuid}?tree=True&rendering_mode=raw"
        );
        let resp = client
            .get(&url)
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        let Ok(resp) = resp.error_for_status() else { continue };
        let Ok(detail) = resp.json::<serde_json::Value>().await else { continue };
        // parse_claude expects a top-level array OR single object; a single
        // conversation object is accepted directly.
        out.extend(parse_claude(&detail));
    }

    if out.is_empty() {
        return Err("claude.ai: authenticated but no conversations could be parsed".to_string());
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// ChatGPT
// ───────────────────────────────────────────────────────────────────────────

/// EXPERIMENTAL: pull conversations from chatgpt.com using a bearer
/// `access_token` (the JWT a logged-in session uses against `backend-api`).
///
/// Flow: `GET /backend-api/conversations?offset=0&limit=100` → ids → per id
/// `GET /backend-api/conversation/{id}` (the `mapping` tree) → reuse
/// [`parse_chatgpt`] for linearization.
///
/// `access_token` is never logged.
pub async fn pull_chatgpt(access_token: &str) -> Result<Vec<ImportedConversation>, String> {
    if access_token.trim().is_empty() {
        return Err("empty access token".to_string());
    }
    let client = client()?;
    let bearer = format!("Bearer {access_token}");

    // 1) List conversation ids.
    let list: serde_json::Value = client
        .get("https://chatgpt.com/backend-api/conversations?offset=0&limit=100")
        .header(reqwest::header::AUTHORIZATION, &bearer)
        .send()
        .await
        .map_err(|_| "chatgpt: conversations list request failed (network or blocked)".to_string())?
        .error_for_status()
        .map_err(|e| format!("chatgpt: conversations list returned {}", status_of(&e)))?
        .json()
        .await
        .map_err(|_| "chatgpt: could not parse conversation list (shape changed?)".to_string())?;

    let ids: Vec<String> = list
        .get("items")
        .and_then(|i| i.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("id").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .take(MAX_CONVERSATIONS)
                .collect()
        })
        .unwrap_or_default();

    // 2) Fetch each conversation's mapping and linearize. The detail endpoint
    //    returns a single conversation object with a top-level `mapping` but no
    //    `title`/`create_time` wrapper guarantee, so synthesize the wrapper the
    //    parser expects.
    let mut out = Vec::new();
    for id in ids {
        let resp = client
            .get(format!("https://chatgpt.com/backend-api/conversation/{id}"))
            .header(reqwest::header::AUTHORIZATION, &bearer)
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        let Ok(resp) = resp.error_for_status() else { continue };
        let Ok(detail) = resp.json::<serde_json::Value>().await else { continue };
        // Wrap in a one-element array so parse_chatgpt's top-level handling
        // applies uniformly (it already reads title/create_time/mapping/
        // current_node from each item).
        let wrapped = serde_json::Value::Array(vec![detail]);
        out.extend(parse_chatgpt(&wrapped));
    }

    if out.is_empty() {
        return Err("chatgpt: authenticated but no conversations could be parsed".to_string());
    }
    Ok(out)
}

/// Best-effort status-code extraction from a reqwest error, with no body /
/// header leakage (so a token embedded in a redirected URL never surfaces).
fn status_of(e: &reqwest::Error) -> String {
    e.status().map(|s| s.as_u16().to_string()).unwrap_or_else(|| "error".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offline shape-mapping check: the wrapper we synthesize for the ChatGPT
    /// detail endpoint must linearize via the shared parser. No network.
    #[test]
    fn chatgpt_detail_shape_maps_through_parser() {
        let detail = serde_json::json!({
            "title": "Live pull chat",
            "mapping": {
                "root": { "parent": null, "children": ["m1"], "message": null },
                "m1": { "parent": "root", "children": [],
                        "message": { "author": { "role": "user" },
                                     "content": { "content_type": "text", "parts": ["pulled question"] } } }
            }
        });
        let wrapped = serde_json::Value::Array(vec![detail]);
        let convs = parse_chatgpt(&wrapped);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages[0].content, "pulled question");
        assert_eq!(convs[0].source, "chatgpt");
    }

    /// The Claude per-conversation detail shape maps through `parse_claude`.
    #[test]
    fn claude_detail_shape_maps_through_parser() {
        let detail = serde_json::json!({
            "uuid": "x",
            "name": "Live claude chat",
            "chat_messages": [
                { "sender": "human", "text": "pulled hi" },
                { "sender": "assistant", "content": [{ "type": "text", "text": "pulled hello" }] }
            ]
        });
        let convs = parse_claude(&detail);
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].source, "claude.ai");
    }

    #[tokio::test]
    async fn empty_credentials_error_without_network() {
        assert!(pull_claude("   ").await.is_err());
        assert!(pull_chatgpt("").await.is_err());
    }
}
