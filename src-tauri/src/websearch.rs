//! Reusable keyless web-search engine (DuckDuckGo HTML endpoint).
//!
//! This is the single source of truth for "search the web and get back a list
//! of ranked results". It was originally inlined in `commands::deep_research`
//! (which fans many planner-generated queries out and then *reads* each hit's
//! page); it's now shared so the chat `@websearch:<query>` context provider can
//! reuse the exact same fetch + parse without duplicating the DDG quirks
//! (the `/l/?uddg=` redirect wrapper, protocol-relative `//` links, the
//! `result__a` / `result__snippet` HTML shape).
//!
//! We only ever return result *metadata* (title + url + snippet) as text — we
//! never fetch the result pages here, so there is no SSRF surface: the only
//! outbound request is to the fixed `html.duckduckgo.com` endpoint. A caller
//! who wants a page's contents follows up with the SSRF-guarded
//! `commands::context::fetch_url` (that's what `@web:<url>` does).

use std::time::Duration;

/// One web-search result: the linked title, its resolved URL, and DuckDuckGo's
/// snippet (may be empty when the page exposes none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Decode the URL out of a DuckDuckGo result href (handles the `/l/?uddg=`
/// redirect wrapper and protocol-relative `//` links).
pub fn decode_ddg_href(href: &str) -> String {
    let h = href.trim();
    let decoded = if let Some(idx) = h.find("uddg=") {
        let rest = &h[idx + 5..];
        let val = rest.split('&').next().unwrap_or(rest);
        percent_decode(val)
    } else {
        h.to_string()
    };
    if let Some(stripped) = decoded.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        decoded
    }
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(h) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(h);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Cheap tag-stripper + common-entity decoder for the short bits of inline
/// markup DuckDuckGo wraps around titles/snippets (`<b>` highlight spans).
pub fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract result hits (title + url + snippet) from a DuckDuckGo HTML results
/// page. Splitting on `result__a` keeps one segment per result; the snippet for
/// that result lives in the same segment (before the next `result__a`).
pub fn parse_results(html: &str) -> Vec<WebResult> {
    let mut hits = Vec::new();
    for part in html.split("result__a").skip(1) {
        let Some(hpos) = part.find("href=\"") else { continue };
        let after = &part[hpos + 6..];
        let Some(end) = after.find('"') else { continue };
        let url = decode_ddg_href(&after[..end]);
        if !url.starts_with("http") {
            continue;
        }
        let title = part[hpos..]
            .find('>')
            .and_then(|gp| {
                let t = &part[hpos + gp + 1..];
                t.find("</a>").map(|lt| strip_tags(&t[..lt]))
            })
            .unwrap_or_default();
        let snippet = extract_snippet(part);
        hits.push(WebResult { title, url, snippet });
    }
    hits
}

/// Pull the `result__snippet` text out of a single result segment, if present.
/// DuckDuckGo renders it as `<a class="result__snippet" …>text</a>` (or
/// occasionally a `<div>`), so close on whichever of `</a>`/`</div>` comes first.
fn extract_snippet(part: &str) -> String {
    let Some(sp) = part.find("result__snippet") else { return String::new() };
    let rest = &part[sp..];
    let Some(gp) = rest.find('>') else { return String::new() };
    let body = &rest[gp + 1..];
    let end = match (body.find("</a>"), body.find("</div>")) {
        (Some(a), Some(d)) => a.min(d),
        (Some(a), None) => a,
        (None, Some(d)) => d,
        (None, None) => body.len().min(600),
    };
    strip_tags(&body[..end])
}

/// Run a keyless DuckDuckGo HTML search and return up to `limit` de-duplicated
/// results (by URL). Network errors surface as `Err(String)` so the caller can
/// degrade gracefully.
pub async fn search(query: &str, limit: usize) -> Result<Vec<WebResult>, String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; CortexResearch/1.0)")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .send()
        .await
        .map_err(|e| format!("search request failed: {e}"))?;
    let html = resp.text().await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for h in parse_results(&html) {
        if seen.insert(h.url.clone()) {
            out.push(h);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_redirect_and_relative_hrefs() {
        assert_eq!(
            decode_ddg_href("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fx%3D1&rut=abc"),
            "https://example.com/a?x=1"
        );
        assert_eq!(decode_ddg_href("https://plain.example.com/p"), "https://plain.example.com/p");
        assert_eq!(decode_ddg_href("//cdn.example.com/x"), "https://cdn.example.com/x");
    }

    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(percent_decode("%41%42"), "AB");
    }

    #[test]
    fn strip_tags_removes_markup_and_entities() {
        assert_eq!(strip_tags("The <b>Rust</b> &amp; Cargo"), "The Rust & Cargo");
    }

    #[test]
    fn parses_results_page_with_titles_urls_snippets() {
        let html = r#"<div>
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&rut=z">The <b>Rust</b> Language</a>
            <a class="result__snippet" href="https://rust-lang.org/">A language empowering <b>everyone</b> to build reliable software.</a>
            </div>
            <div>
            <a class="result__a" href="https://docs.rs/">docs.rs</a>
            </div>"#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://rust-lang.org/");
        assert_eq!(hits[0].title, "The Rust Language");
        assert_eq!(
            hits[0].snippet,
            "A language empowering everyone to build reliable software."
        );
        assert_eq!(hits[1].url, "https://docs.rs/");
        // No snippet markup for the second result → empty, not garbage.
        assert_eq!(hits[1].snippet, "");
    }

    #[test]
    fn parse_results_skips_non_http_and_malformed() {
        // A `result__a` with a javascript: href must be dropped, not crash.
        let html = r#"<a class="result__a" href="javascript:void(0)">bad</a>
            <a class="result__a" href="https://ok.example.com/">ok</a>"#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://ok.example.com/");
    }

    #[test]
    fn search_dedups_and_caps_via_parse() {
        // The dedup + limit logic in `search()` operates on `parse_results`
        // output; assert the parser + dedup compose (no network needed): two
        // identical URLs collapse to one in the caller's HashSet path.
        let html = r#"<a class="result__a" href="https://dup.example/">one</a>
            <a class="result__a" href="https://dup.example/">two</a>
            <a class="result__a" href="https://other.example/">three</a>"#;
        let parsed = parse_results(html);
        assert_eq!(parsed.len(), 3, "parser keeps every result__a");
        let mut seen = std::collections::HashSet::new();
        let deduped: Vec<_> = parsed.into_iter().filter(|h| seen.insert(h.url.clone())).collect();
        assert_eq!(deduped.len(), 2, "url dedup collapses the duplicate");
    }

    // Live network test — gated behind `--ignored` so the offline suite never
    // depends on DuckDuckGo. Proves the real fetch + parse round-trip:
    //   cargo test --lib websearch -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_ddg_search_returns_results() {
        let hits = search("rust programming language", 5).await.expect("search ok");
        assert!(!hits.is_empty(), "expected at least one live result");
        assert!(hits.len() <= 5, "respects the limit");
        assert!(hits.iter().all(|h| h.url.starts_with("http")), "all urls are http(s)");
        assert!(hits.iter().any(|h| !h.title.trim().is_empty()), "at least one has a title");
    }
}
