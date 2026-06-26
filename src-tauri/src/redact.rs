//! Text secret-scrubbing for anything Cortex writes out of the app
//! (chat exports, shared markdown, future log/transcript dumps).
//!
//! The vault auto-publishes via Quartz and the user has a documented history of
//! credential leaks, so any export path must strip secrets *before* the bytes
//! land on disk. This complements `observability::sentry::redact`, which scrubs
//! structured JSON telemetry; this one scrubs free-form text.

use once_cell::sync::Lazy;
use regex::Regex;

const MASK: &str = "[REDACTED]";

/// Single-line secret shapes. Case-insensitive where it helps.
static SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?i)(",
        // OpenAI / Anthropic-style keys.
        r"sk-[A-Za-z0-9_-]{20,}",
        r"|pk-[A-Za-z0-9_-]{20,}",
        // GitHub tokens (classic + fine-grained) and other forge PATs.
        r"|gh[posru]_[A-Za-z0-9]{20,}",
        r"|github_pat_[A-Za-z0-9_]{20,}",
        // Slack.
        r"|xox[baprs]-[A-Za-z0-9-]{10,}",
        // AWS access key id.
        r"|AKIA[0-9A-Z]{16}",
        // Bearer headers.
        r"|Bearer\s+[A-Za-z0-9._\-]+",
        // JWTs (three base64url segments).
        r"|eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
        // Cortex's own baked gateway backend key shape: a 64-char hex string
        // (SHA-256-width). Word-bounded so we don't clip longer hex blobs.
        r"|\b[0-9a-f]{64}\b",
        r")",
    ))
    .unwrap()
});

/// `key = value` / `key: value` assignments for sensitive identifiers. Captures
/// the key so it stays readable while the value is masked.
static ASSIGN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)\b(password|passwd|secret|api[_-]?key|access[_-]?token|auth[_-]?token|token)\b(\s*[:=]\s*)("?)([^\s"']{1,})("?)"#)
        .unwrap()
});

/// Whole PEM private-key blocks (SSH / TLS), which span multiple lines.
static PRIVATE_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
        .unwrap()
});

/// Redact secrets from free-form text. Order matters: kill multi-line key
/// blocks first, then `key=value` assignments, then bare token shapes.
pub fn redact_text(input: &str) -> String {
    let s = PRIVATE_KEY_RE.replace_all(input, MASK);
    let s = ASSIGN_RE.replace_all(&s, |c: &regex::Captures| {
        format!("{}{}{}{}{}", &c[1], &c[2], &c[3], MASK, &c[5])
    });
    SECRET_RE.replace_all(&s, MASK).into_owned()
}

#[cfg(test)]
mod tests {
    use super::redact_text;

    #[test]
    fn masks_token_shapes() {
        assert!(redact_text("key sk-abcdefghijklmnopqrstuvwxyz0123").contains("[REDACTED]"));
        assert!(redact_text("ghp_0123456789abcdefghijABCDEFG").contains("[REDACTED]"));
        assert!(redact_text("Authorization: Bearer abc.def-123_xyz").contains("[REDACTED]"));
        assert!(redact_text("id AKIAIOSFODNN7EXAMPLE here").contains("[REDACTED]"));
        let jwt = "eyJhbGc.eyJzdWIiOiIx.SflKxwRJSMeKKF2QT4";
        assert!(redact_text(jwt).contains("[REDACTED]"));
    }

    #[test]
    fn masks_assignments_but_keeps_key() {
        let out = redact_text("password = hunter2secret");
        assert!(out.contains("password"));
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("hunter2secret"));
        let out2 = redact_text(r#"api_key: "abcd1234efgh""#);
        assert!(out2.contains("api_key"));
        assert!(!out2.contains("abcd1234efgh"));
    }

    #[test]
    fn masks_private_key_block() {
        let pem = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3Nz...\nlots\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let out = redact_text(pem);
        assert!(out.contains("before") && out.contains("after"));
        assert!(!out.contains("AAAAB3Nz"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn leaves_ordinary_prose_untouched() {
        let prose = "The quick brown fox edits src/main.rs:42 and runs the tests.";
        assert_eq!(redact_text(prose), prose);
    }
}
