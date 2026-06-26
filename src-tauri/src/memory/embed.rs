//! Embedding helpers for semantic memory search — builds on the existing
//! Ollama config (no new dependency). Embeds text via Ollama's
//! `/api/embeddings` and scores cosine similarity. Used by the
//! `semantic_memory_search` command to re-rank lexical candidates from the
//! vault. Everything degrades gracefully: if Ollama is unreachable or the
//! embedding model is missing, callers fall back to lexical order.

use once_cell::sync::Lazy;
use serde::Deserialize;

/// Process-wide HTTP client so a batch of embeds shares connections
/// (keep-alive) instead of doing a fresh TCP+TLS handshake per request.
static EMBED_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_default()
});

/// Default embedding model — present on the homelab Ollama. Override via the
/// `OLLAMA_EMBED_MODEL` env var.
pub const DEFAULT_EMBED_MODEL: &str = "mxbai-embed-large";

pub fn embed_model() -> String {
    std::env::var("OLLAMA_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
}

#[derive(Deserialize)]
struct EmbedResponse {
    #[serde(default)]
    embedding: Vec<f32>,
}

/// Embed a single string via Ollama `/api/embeddings`. Returns the vector, or
/// an error the caller can swallow to fall back to lexical search.
pub async fn embed_text(base_url: &str, model: &str, text: &str) -> anyhow::Result<Vec<f32>> {
    let resp = EMBED_CLIENT
        .post(format!("{}/api/embeddings", base_url.trim_end_matches('/')))
        .json(&serde_json::json!({ "model": model, "prompt": text }))
        .send()
        .await?
        .error_for_status()?
        .json::<EmbedResponse>()
        .await?;
    if resp.embedding.is_empty() {
        anyhow::bail!("ollama returned an empty embedding (model '{model}' may not embed)");
    }
    Ok(resp.embedding)
}

/// Cosine similarity in [-1, 1]. Returns 0.0 for mismatched/zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::cosine;

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 1.0], &[2.0, 2.0]) - 1.0).abs() < 1e-6); // scale-invariant
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0); // length mismatch
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0); // zero vector
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6); // opposite
    }
}
