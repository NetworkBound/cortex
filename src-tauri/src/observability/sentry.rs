//! Sentry SDK integration. Phase 4 stub: actual SDK wiring is gated on
//! the user opt-in and a configured DSN. Until then this module exposes
//! the redaction helpers used by `beforeSend` so we can unit-test them.

use once_cell::sync::Lazy;
use regex::Regex;

static SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)(sk-[A-Za-z0-9_-]{20,}|pk-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{20,}|xoxb-[A-Za-z0-9_-]{20,}|Bearer\s+[A-Za-z0-9_.-]+|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]+|AKIA[0-9A-Z]{16}|-----BEGIN[ A-Z]*PRIVATE KEY-----)"#).unwrap()
});

/// Returns true if a key name suggests its value is a secret or PII and
/// should be redacted regardless of the value's length or shape. Matches
/// on substrings so e.g. `api_key`, `auth_token`, `user_email` are caught.
fn is_sensitive_key(key: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "message", "content", "prompt", "body", "args", "result", "password",
        "passwd", "secret", "token", "api_key", "apikey", "authorization",
        "auth", "credential", "private_key", "session", "cookie", "ssn",
        "email", "phone", "address",
    ];
    NEEDLES.iter().any(|needle| key.contains(needle))
}

/// Recursively walks a `serde_json::Value` and replaces values that look
/// like prompt content (long strings under message/content/prompt/body/
/// args/result keys, or anything matching known secret shapes).
pub fn redact(value: &mut serde_json::Value) {
    redact_inner(value, /*sensitive_parent=*/ false);
}

fn redact_inner(value: &mut serde_json::Value, sensitive_parent: bool) {
    match value {
        serde_json::Value::String(s) => {
            if sensitive_parent || s.chars().count() > 256 || SECRET_RE.is_match(s) {
                *s = "[REDACTED]".to_string();
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                redact_inner(v, sensitive_parent);
            }
        }
        serde_json::Value::Object(obj) => {
            for (k, v) in obj.iter_mut() {
                let key = k.to_lowercase();
                let sensitive = sensitive_parent || is_sensitive_key(&key);
                redact_inner(v, sensitive);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_shapes() {
        let mut v = serde_json::json!({"k": "Bearer sk-supersecrettokenstringhere1234567890"});
        redact(&mut v);
        assert_eq!(v["k"], "[REDACTED]");
    }

    #[test]
    fn redacts_long_strings() {
        let mut v = serde_json::json!({ "x": "a".repeat(300) });
        redact(&mut v);
        assert_eq!(v["x"], "[REDACTED]");
    }

    #[test]
    fn redacts_under_sensitive_keys() {
        let mut v = serde_json::json!({ "message": "hi", "ok": true });
        redact(&mut v);
        assert_eq!(v["message"], "[REDACTED]");
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn redacts_short_secrets_under_named_keys() {
        let mut v = serde_json::json!({
            "password": "hunter2",
            "user_email": "a@b.com",
            "api_key": "abc123",
            "ok": true
        });
        redact(&mut v);
        assert_eq!(v["password"], "[REDACTED]");
        assert_eq!(v["user_email"], "[REDACTED]");
        assert_eq!(v["api_key"], "[REDACTED]");
        assert_eq!(v["ok"], true);
    }
}
