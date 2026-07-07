//! Upstream auto-pagination helpers.
//!
//! Pure (no I/O): parsing the next-page target from pagination hints, parsing
//! an RFC 5980 `Link: rel="next"` header, and the page/byte/time cap types.
//! The async fetch loop lives in the CLI, which composes these with the
//! adapter so the pure decisions stay testable.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Hard caps for a pagination follow loop. Reaching any cap stops the loop and
/// surfaces a continuation rather than running away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PageCaps {
    pub max_pages: usize,
    pub max_total_bytes: u64,
    pub max_wall_ms: u64,
}

impl Default for PageCaps {
    fn default() -> Self {
        Self {
            max_pages: 5,
            max_total_bytes: 8 * 1024 * 1024,
            max_wall_ms: 60_000,
        }
    }
}

/// Where to fetch the next page from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageTarget {
    /// Fetch with updated operation args (a cursor token, page increment, ...).
    Args(Value),
    /// Fetch a literal next URL (Link `rel="next"` or a `next` URL field).
    Url(String),
}

/// Why a pagination loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// No next page was reported.
    NoMore,
    /// The page-count cap was reached.
    PageCap,
    /// The total-byte cap was reached.
    ByteCap,
    /// The wall-clock cap was reached.
    TimeCap,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoMore => "no_more",
            Self::PageCap => "page_cap",
            Self::ByteCap => "byte_cap",
            Self::TimeCap => "time_cap",
        }
    }
}

/// Parse a `rel="next"` target URL from an RFC 5980 Link header value
/// (case-insensitive `rel`). Returns the first `next` URL, if any.
pub fn parse_link_rel_next(link_header: &str) -> Option<String> {
    for section in link_header.split(',') {
        let lower = section.to_ascii_lowercase();
        if !(lower.contains("rel=\"next\"") || lower.contains("rel=next")) {
            continue;
        }
        let Some(start) = section.find('<') else {
            continue;
        };
        let after = &section[start + 1..];
        let Some(end) = after.find('>') else {
            continue;
        };
        return Some(after[..end].trim().to_string());
    }
    None
}

/// Derive the next-page target from a page's pagination `hints` (the shape
/// produced by the HTTP adapter: `link_rel_next`, `next`, `next_page` /
/// `nextPageToken`, `has_more`) and the `current_args` used for this page.
///
/// Resolution order: a cursor token (written to the caller's existing
/// cursor-ish param, or `page_token`), then a `page` increment, then a Link
/// `rel="next"` URL, then a full-URL `next` field. Cursor/page are tried
/// FIRST because the follow loop can chase them directly; URL continuation
/// requires an adapter `execute_url` path that is not yet wired, so it is only
/// chosen when no followable cursor/page is present (otherwise a Link header
/// would silently preempt a usable cursor). Returns `None` when there is no
/// next page.
pub fn next_args_from_hints(hints: &Value, current_args: &Value) -> Option<PageTarget> {
    let base = match current_args {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };

    // A cursor token. A URL-valued `next` is NOT a cursor (it is handled by
    // the URL branches below), so filter it out here.
    let token = ["next_page", "nextPageToken", "next"]
        .iter()
        .find_map(|key| hints.get(*key).and_then(Value::as_str))
        .filter(|token| !token.starts_with("http://") && !token.starts_with("https://"));
    if let Some(token) = token {
        let param = cursor_param(&base).unwrap_or_else(|| "page_token".to_string());
        let mut next_args = base;
        next_args.insert(param, Value::String(token.to_string()));
        return Some(PageTarget::Args(Value::Object(next_args)));
    }

    let has_more = hints
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if (has_more || base.contains_key("page"))
        && let Some(page) = base.get("page").and_then(Value::as_u64)
    {
        let mut next_args = base;
        next_args.insert("page".to_string(), Value::from(page.saturating_add(1)));
        return Some(PageTarget::Args(Value::Object(next_args)));
    }

    if let Some(link) = hints.get("link_rel_next").and_then(Value::as_str)
        && let Some(url) = parse_link_rel_next(link)
    {
        return Some(PageTarget::Url(url));
    }

    if let Some(next) = hints.get("next").and_then(Value::as_str)
        && (next.starts_with("http://") || next.starts_with("https://"))
    {
        return Some(PageTarget::Url(next.to_string()));
    }

    None
}

/// True when an operation's effects permit auto-pagination: read-only and not
/// mutating, shell-backed, or sensitive (I6/I7). The follow loop never chases
/// pages on an operation that could mutate state or touch secrets.
pub fn pagination_allowed(effects: &crate::EffectSet) -> bool {
    effects.read_only && !effects.mutating && !effects.shell && !effects.sensitive
}

fn cursor_param(args: &Map<String, Value>) -> Option<String> {
    for candidate in [
        "cursor",
        "page_token",
        "next_token",
        "starting_after",
        "continuation_token",
        "after",
    ] {
        if args.contains_key(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc5980_link_rel_next_case_insensitively() {
        let link = "<https://api.example.com/page2>; rel=\"next\", <https://api.example.com/last>; rel=\"last\"";
        assert_eq!(
            parse_link_rel_next(link).as_deref(),
            Some("https://api.example.com/page2")
        );
        // Case-insensitive rel, single section.
        assert_eq!(
            parse_link_rel_next("<https://x/p2>; REL=\"next\"").as_deref(),
            Some("https://x/p2")
        );
        // No next rel.
        assert!(parse_link_rel_next("<https://x/p1>; rel=\"prev\"").is_none());
    }

    #[test]
    fn next_page_url_from_link_header_hint() {
        let hints = json!({"link_rel_next": "<https://api/p2>; rel=\"next\""});
        let target = next_args_from_hints(&hints, &json!({})).unwrap();
        assert_eq!(target, PageTarget::Url("https://api/p2".to_string()));
    }

    #[test]
    fn next_page_url_from_next_field() {
        let hints = json!({"next": "https://api/items?cursor=xyz"});
        let target = next_args_from_hints(&hints, &json!({"limit": 10})).unwrap();
        assert_eq!(
            target,
            PageTarget::Url("https://api/items?cursor=xyz".to_string())
        );
    }

    #[test]
    fn cursor_token_written_to_existing_param() {
        let hints = json!({"nextPageToken": "TOKEN-2"});
        // Use a non-default cursor param (starting_after) so the test fails if
        // the cursor_param candidate scan regresses.
        let target =
            next_args_from_hints(&hints, &json!({"starting_after": "TOKEN-1", "limit": 5}));
        match target {
            Some(PageTarget::Args(args)) => {
                assert_eq!(args["starting_after"], json!("TOKEN-2"));
                assert_eq!(args["limit"], json!(5));
            }
            other => panic!("expected Args target, got {other:?}"),
        }
    }

    #[test]
    fn cursor_token_defaults_to_page_token_param() {
        let hints = json!({"next_page": "abc"});
        let target = next_args_from_hints(&hints, &json!({"limit": 5})).unwrap();
        match target {
            PageTarget::Args(args) => assert_eq!(args["page_token"], json!("abc")),
            other => panic!("expected Args target, got {other:?}"),
        }
    }

    #[test]
    fn cursor_token_wins_over_unsupported_link_url() {
        // Both a followable cursor and a Link rel="next" URL are present: the
        // cursor must win so the follow loop can chase it instead of aborting
        // on the (not-yet-wired) URL continuation.
        let hints = json!({
            "link_rel_next": "<https://api/p2>; rel=\"next\"",
            "nextPageToken": "CURSOR-2"
        });
        let target = next_args_from_hints(&hints, &json!({"page_token": "CURSOR-1"})).unwrap();
        match target {
            PageTarget::Args(args) => assert_eq!(args["page_token"], json!("CURSOR-2")),
            other => panic!("expected Args (cursor) target, got {other:?}"),
        }
    }

    #[test]
    fn page_number_increments_when_caller_uses_page() {
        let hints = json!({"has_more": true});
        let target = next_args_from_hints(&hints, &json!({"page": 2, "per_page": 20})).unwrap();
        match target {
            PageTarget::Args(args) => {
                assert_eq!(args["page"], json!(3));
                assert_eq!(args["per_page"], json!(20));
            }
            other => panic!("expected Args target, got {other:?}"),
        }
    }

    #[test]
    fn no_next_page_returns_none() {
        assert!(next_args_from_hints(&json!({"has_more": false}), &json!({})).is_none());
        assert!(next_args_from_hints(&json!({}), &json!({})).is_none());
    }

    use serde_json::json;
}
