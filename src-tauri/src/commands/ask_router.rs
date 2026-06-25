//! Natural-language slash command router.
//!
//! Backs the `/ask <query>` slash command. Given the user's free-form text and
//! the list of currently-available slash commands (shipped from the frontend
//! so this code doesn't have to mirror the JS registry), asks the gateway to pick
//! the best matching slash and any args it should be invoked with. Returns
//! `null` + a reason when no slash matches confidently.
//!
//! Reuses the streaming-collect + timeout pattern from
//! [`super::commit_suggest`] / [`super::changelog`].

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_state::AppState;
use crate::gateway::client::{ChatCompletionRequest, ChatMessage, GatewayClient, StreamItem};

/// Wall-clock cap on the gateway call. The model only emits a tiny JSON blob,
/// so 20s is comfortable headroom.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Hard cap on the slash menu blob sent to the model. Each spec is ~80 bytes,
/// so 16 KiB fits roughly 200 commands — comfortably above what the registry
/// holds today and below most context limits.
const MENU_LIMIT_BYTES: usize = 16 * 1024;

const SYSTEM_PROMPT: &str = "You are a slash-command router for Cortex, a desktop \
chat app. Given the user's natural-language query and the list of available \
slash commands below, pick the single best match and the args it should be \
invoked with. Output ONLY a JSON object with these exact keys: \
{\"slash\": \"<name or null>\", \"args\": \"<args or empty string>\", \
\"confidence\": <0.0-1.0>, \"reason\": \"<one short sentence>\"}. \
Use the canonical slash name (not an alias). If no command matches well, \
set \"slash\" to null, confidence < 0.5, and explain briefly in \"reason\". \
Do not wrap the JSON in code fences or add any other text.";

/// Slash spec shipped from the frontend. Mirrors the relevant fields of
/// `SlashCommand` in `src/lib/slash-commands.ts` — we deliberately accept just
/// what the router needs so the JS side can evolve without breaking us.
#[derive(Debug, Deserialize, Clone)]
pub struct SlashSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub usage: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct AskResult {
    /// Canonical slash name (no leading `/`), or `None` when nothing matched.
    pub matched_slash: Option<String>,
    /// Args string to pass to the matched slash. Empty when the slash takes none.
    pub suggested_args: String,
    /// Model-reported confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// One-line rationale shown to the user (e.g. in a confirm toast).
    pub reason: String,
}

#[tauri::command]
pub async fn ask_router(
    query: String,
    available_slashes: Vec<SlashSpec>,
    state: State<'_, AppState>,
) -> Result<AskResult, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("ask_router: empty query".into());
    }
    if available_slashes.is_empty() {
        return Err("ask_router: no slashes provided".into());
    }

    let menu = render_menu(&available_slashes);

    let cfg = state.config.read().clone();
    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    let client = GatewayClient::new(cfg.gateway_base_url, api_key);

    let user_prompt = format!(
        "USER QUERY:\n{q}\n\n--- AVAILABLE SLASH COMMANDS ---\n{menu}\n--- END ---\n\n\
         Respond with the JSON object only.",
    );

    let req = ChatCompletionRequest {
        model: cfg.gateway_model.clone(),
        messages: vec![
            ChatMessage { role: "system".into(), content: SYSTEM_PROMPT.into() },
            ChatMessage { role: "user".into(), content: user_prompt },
        ],
        stream: true,
        temperature: Some(0.1),
    };

    let collected = run_with_timeout(client, req).await?;
    let raw = sanitize(&collected);
    if raw.trim().is_empty() {
        return Err("The gateway returned an empty router response".into());
    }

    parse_router_json(&raw, &available_slashes)
}

/// Render the slash menu as a compact text block. Each row is
/// `/<name> [aliases] — <description>` — short enough that the prompt stays
/// well under typical context limits even with ~150 commands.
fn render_menu(slashes: &[SlashSpec]) -> String {
    let mut out = String::new();
    for s in slashes {
        let name = s.name.trim();
        if name.is_empty() {
            continue;
        }
        out.push('/');
        out.push_str(name);
        if let Some(u) = s.usage.as_deref() {
            let u = u.trim();
            if !u.is_empty() {
                out.push(' ');
                out.push_str(u);
            }
        }
        if !s.aliases.is_empty() {
            let aliases: Vec<&str> =
                s.aliases.iter().map(|a| a.as_str()).filter(|a| !a.is_empty()).collect();
            if !aliases.is_empty() {
                out.push_str(" (aka ");
                out.push_str(&aliases.join(", "));
                out.push(')');
            }
        }
        let desc = s.description.trim();
        if !desc.is_empty() {
            out.push_str(" — ");
            out.push_str(desc);
        }
        out.push('\n');
        if out.len() >= MENU_LIMIT_BYTES {
            out.push_str("…[truncated — menu exceeded 16 KiB]\n");
            break;
        }
    }
    out
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
        Err(_) => Err("ask_router: The gateway timed out".into()),
    }
}

/// Best-effort strip of code fences / preamble around the JSON object.
fn sanitize(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_prefix("```") {
        let after_lang = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            s = after_lang[..end].trim_end_matches('\n').to_string();
        }
    }
    s.trim().to_string()
}

#[derive(Debug, Deserialize)]
struct RawRouterJson {
    #[serde(default)]
    slash: Option<serde_json::Value>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    reason: Option<String>,
}

/// Parse the model output. We accept some sloppiness — the model occasionally
/// emits `"slash": ""` instead of `null`, or wraps the JSON in extra text we
/// missed. Resolve aliases against the supplied menu so the frontend can run
/// the result through `findCommand` directly.
fn parse_router_json(raw: &str, slashes: &[SlashSpec]) -> Result<AskResult, String> {
    let json_blob = extract_json_object(raw).unwrap_or(raw);
    let parsed: RawRouterJson = serde_json::from_str(json_blob)
        .map_err(|e| format!("ask_router: invalid JSON from model: {e} (raw={raw})"))?;

    let raw_slash = match parsed.slash {
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim().trim_start_matches('/').to_string();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
                None
            } else {
                Some(trimmed)
            }
        }
        Some(serde_json::Value::Null) | None => None,
        // Defensive — any other type is treated as "no match".
        Some(_) => None,
    };

    let canonical = raw_slash.and_then(|name| resolve_canonical(&name, slashes));

    let confidence = parsed.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
    let args = parsed.args.unwrap_or_default().trim().to_string();
    let reason = parsed
        .reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "(no reason provided)".to_string());

    Ok(AskResult {
        matched_slash: canonical,
        suggested_args: args,
        confidence,
        reason,
    })
}

/// Find the first `{...}` substring. The model sometimes prefixes the JSON
/// with stray whitespace or trailing chatter even after `sanitize`.
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Map a slash name (canonical OR alias) onto the canonical name from the
/// menu. Returns `None` when the name isn't in the supplied list — we
/// deliberately don't trust the model to invent commands.
fn resolve_canonical(name: &str, slashes: &[SlashSpec]) -> Option<String> {
    let needle = name.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    for s in slashes {
        if s.name.eq_ignore_ascii_case(&needle) {
            return Some(s.name.clone());
        }
        for a in &s.aliases {
            if a.eq_ignore_ascii_case(&needle) {
                return Some(s.name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu_sample() -> Vec<SlashSpec> {
        vec![
            SlashSpec {
                name: "changelog".into(),
                description: "Generate a changelog".into(),
                aliases: vec!["changes".into()],
                usage: Some("[since]".into()),
            },
            SlashSpec {
                name: "fix".into(),
                description: "AI-debug recent error".into(),
                aliases: vec!["debug".into()],
                usage: None,
            },
            SlashSpec {
                name: "cost".into(),
                description: "Show usage cost".into(),
                aliases: vec![],
                usage: None,
            },
        ]
    }

    #[test]
    fn render_menu_includes_aliases_and_usage() {
        let txt = render_menu(&menu_sample());
        assert!(txt.contains("/changelog [since] (aka changes)"));
        assert!(txt.contains("/fix (aka debug)"));
        assert!(txt.contains("/cost"));
    }

    #[test]
    fn parse_router_json_resolves_alias_to_canonical() {
        let raw = r#"{"slash":"debug","args":"","confidence":0.92,"reason":"user wants to debug"}"#;
        let out = parse_router_json(raw, &menu_sample()).unwrap();
        assert_eq!(out.matched_slash.as_deref(), Some("fix"));
        assert_eq!(out.suggested_args, "");
        assert!((out.confidence - 0.92).abs() < 1e-4);
    }

    #[test]
    fn parse_router_json_strips_leading_slash() {
        let raw = r#"{"slash":"/changelog","args":"1d","confidence":0.81,"reason":"recent changes"}"#;
        let out = parse_router_json(raw, &menu_sample()).unwrap();
        assert_eq!(out.matched_slash.as_deref(), Some("changelog"));
        assert_eq!(out.suggested_args, "1d");
    }

    #[test]
    fn parse_router_json_treats_empty_as_no_match() {
        let raw = r#"{"slash":"","args":"","confidence":0.2,"reason":"unclear"}"#;
        let out = parse_router_json(raw, &menu_sample()).unwrap();
        assert!(out.matched_slash.is_none());
        assert_eq!(out.reason, "unclear");
    }

    #[test]
    fn parse_router_json_drops_invented_command() {
        let raw = r#"{"slash":"teleport","args":"","confidence":0.99,"reason":"made up"}"#;
        let out = parse_router_json(raw, &menu_sample()).unwrap();
        assert!(out.matched_slash.is_none(), "model-invented slash must be rejected");
    }

    #[test]
    fn parse_router_json_handles_chatter_around_blob() {
        let raw = "sure thing — here you go:\n{\"slash\":\"cost\",\"args\":\"\",\"confidence\":0.7,\"reason\":\"asked about cost\"}\nhope that helps";
        let out = parse_router_json(raw, &menu_sample()).unwrap();
        assert_eq!(out.matched_slash.as_deref(), Some("cost"));
    }

    #[test]
    fn sanitize_strips_code_fences() {
        let raw = "```json\n{\"slash\":\"cost\"}\n```";
        assert_eq!(sanitize(raw), "{\"slash\":\"cost\"}");
    }
}
