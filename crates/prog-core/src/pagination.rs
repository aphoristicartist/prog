//! Upstream auto-pagination helpers.
//!
//! Pure (no I/O): canonical pagination-shape detection from a page body and a
//! `Link` header, deriving the next-page target, parsing an RFC 5980
//! `Link: rel="next"` header, merging page shapes monotonically (I5), and the
//! page/byte/time cap types. The async fetch loop lives in the CLI, which
//! composes these with the adapter so the pure decisions stay testable.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::shape::{self, Shape};

/// Cursor-family response fields, in the priority order used to pick the
/// normalized `next_cursor` token. `next` is handled separately because it may
/// be a full URL rather than a token.
const CURSOR_FAMILY: &[&str] = &[
    "cursor",
    "next_cursor",
    "next_token",
    "continuation",
    "continuation_token",
    "nextPageToken",
    "next_page",
    "starting_after",
    "after",
];

/// Scan order for a cursor token inside an already-built hints object. The
/// normalized `next_cursor` key (produced by [`extract_pagination_hints`]) is
/// tried first; the raw family fields follow so direct/legacy hint shapes
/// still resolve. `next` is included last so a non-URL `next` string is still
/// usable as a token while a URL-valued `next` falls through to the URL arms.
const CURSOR_TOKEN_PRIORITY: &[&str] = &[
    "next_cursor",
    "cursor",
    "next_token",
    "continuation",
    "continuation_token",
    "nextPageToken",
    "next_page",
    "starting_after",
    "after",
    "next",
    "page_token",
];

/// Body fields copied through verbatim for observability and legacy callers
/// (the adapter used to surface `next`/`next_page`/`nextPageToken`/`has_more`
/// unchanged). Keeping the raw values means existing consumers and golden
/// fixtures do not break when the normalized keys are added alongside.
const PASSTHROUGH_FIELDS: &[&str] = &[
    "next",
    "next_page",
    "nextPageToken",
    "has_more",
    "hasMore",
    "cursor",
    "next_cursor",
    "next_token",
    "continuation",
    "continuation_token",
    "starting_after",
    "after",
    "page",
    "per_page",
    "page_size",
    "offset",
    "limit",
];

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

    /// True when there is no remaining page to surface a resume action for.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::NoMore)
    }
}

/// Detect canonical pagination hints from a page `body` and (optionally) an
/// RFC 5980 `Link` header value. Pure: performs no I/O. The HTTP adapter is a
/// one-line caller; this is the single testable source of truth for "how does
/// this page advertise its next page".
///
/// The returned object is the hint shape consumed by [`next_args_from_hints`].
/// All keys are optional:
/// - `link_rel_next`: raw `Link` header value when it carries `rel="next"`.
/// - `next_cursor`: a string-valued, non-URL continuation token, normalized
///   from the first present member of the cursor family.
/// - `cursor_param`: the response field name that supplied `next_cursor`.
/// - `next_url`: a full-URL `next` field.
/// - `has_more`: boolean "more pages exist" flag (`has_more` or `hasMore`).
/// - `page_strategy`: `"page_number"` (body echoes `page` + `per_page` /
///   `page_size`) or `"offset_limit"` (body echoes `offset` + `limit`).
///
/// Any recognized raw field present in the body is also copied through so
/// callers and fixtures that consumed the legacy hint shape keep working.
pub fn extract_pagination_hints(body: &Value, link_header: Option<&str>) -> Option<Value> {
    let mut hints = Map::new();

    if let Some(link) = link_header
        && link.to_ascii_lowercase().contains("rel=\"next\"")
    {
        hints.insert("link_rel_next".to_string(), Value::String(link.to_string()));
    }

    if let Value::Object(map) = body {
        // Cursor family: first string-valued, non-URL member wins (priority order).
        for &field in CURSOR_FAMILY {
            if let Some(value) = map.get(field)
                && let Some(token) = value.as_str()
                && !is_url(token)
            {
                hints.insert("next_cursor".to_string(), Value::String(token.to_string()));
                hints.insert("cursor_param".to_string(), Value::String(field.to_string()));
                break;
            }
        }

        // `next` field: URL-valued -> next_url; non-URL string -> cursor token.
        if let Some(next) = map.get("next").and_then(Value::as_str)
            && !hints.contains_key("next_cursor")
        {
            if is_url(next) {
                hints.insert("next_url".to_string(), Value::String(next.to_string()));
            } else {
                hints.insert("next_cursor".to_string(), Value::String(next.to_string()));
                hints.insert(
                    "cursor_param".to_string(),
                    Value::String("next".to_string()),
                );
            }
        } else if let Some(next) = map.get("next").and_then(Value::as_str)
            && is_url(next)
        {
            hints.insert("next_url".to_string(), Value::String(next.to_string()));
        }

        // has_more / hasMore.
        for key in ["has_more", "hasMore"] {
            if let Some(value) = map.get(key).and_then(Value::as_bool) {
                hints.insert("has_more".to_string(), Value::Bool(value));
                break;
            }
        }

        // Strategy detection from echoed paging params.
        if map.contains_key("page")
            && (map.contains_key("per_page") || map.contains_key("page_size"))
        {
            hints.insert(
                "page_strategy".to_string(),
                Value::String("page_number".to_string()),
            );
        }
        if map.contains_key("offset") && map.contains_key("limit") {
            hints.insert(
                "page_strategy".to_string(),
                Value::String("offset_limit".to_string()),
            );
        }

        // Raw passthrough of recognized fields (legacy compat + observability).
        for &key in PASSTHROUGH_FIELDS {
            if let Some(value) = map.get(key) {
                hints.insert(key.to_string(), value.clone());
            }
        }
    }

    if hints.is_empty() {
        None
    } else {
        Some(Value::Object(hints))
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
/// produced by [`extract_pagination_hints`]) and the `current_args` used for
/// this page.
///
/// Resolution order: a cursor token (written to the caller's existing
/// cursor-ish param, or `page_token`), then a page-number increment, then an
/// offset/limit increment, then a Link `rel="next"` URL, then a full-URL
/// `next` field. Cursor/page are tried FIRST because the follow loop can chase
/// them directly; URL continuation requires the adapter `execute_url` path, so
/// it is only chosen when no followable cursor/page is present (otherwise a
/// Link header would silently preempt a usable cursor). Returns `None` when
/// there is no next page.
pub fn next_args_from_hints(hints: &Value, current_args: &Value) -> Option<PageTarget> {
    let base = match current_args {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };

    // 1. A cursor token (URL-valued `next` is excluded here).
    if let Some(token) = cursor_token_from_hints(hints) {
        let param = cursor_param(&base).unwrap_or_else(|| "page_token".to_string());
        let mut next_args = base;
        next_args.insert(param, Value::String(token));
        return Some(PageTarget::Args(Value::Object(next_args)));
    }

    let has_more = hints
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let page_strategy = hints.get("page_strategy").and_then(Value::as_str);

    // 2. Page-number increment.
    if (has_more || page_strategy == Some("page_number") || base.contains_key("page"))
        && let Some(page) = base.get("page").and_then(Value::as_u64)
    {
        let mut next_args = base;
        next_args.insert("page".to_string(), Value::from(page.saturating_add(1)));
        return Some(PageTarget::Args(Value::Object(next_args)));
    }

    // 3. Offset/limit increment: offset advances by `limit`.
    let wants_offset = page_strategy == Some("offset_limit")
        || (base.contains_key("offset") && base.contains_key("limit"));
    if wants_offset
        && let (Some(offset), Some(limit)) = (
            base.get("offset").and_then(Value::as_u64),
            base.get("limit").and_then(Value::as_u64),
        )
    {
        let mut next_args = base;
        next_args.insert(
            "offset".to_string(),
            Value::from(offset.saturating_add(limit)),
        );
        return Some(PageTarget::Args(Value::Object(next_args)));
    }

    // 4. Link rel="next" URL.
    if let Some(link) = hints.get("link_rel_next").and_then(Value::as_str)
        && let Some(url) = parse_link_rel_next(link)
    {
        return Some(PageTarget::Url(url));
    }

    // 5. Normalized next_url.
    if let Some(url) = hints.get("next_url").and_then(Value::as_str) {
        return Some(PageTarget::Url(url.to_string()));
    }

    // 6. Legacy raw `next` URL.
    if let Some(next) = hints.get("next").and_then(Value::as_str)
        && is_url(next)
    {
        return Some(PageTarget::Url(next.to_string()));
    }

    None
}

/// Merge a page's shape into an accumulated multi-page shape. Monotonic by
/// construction because it delegates to [`shape::join`] (I5); `None` is the
/// identity (the first page seeds the accumulation). Named so the invariant
/// harness has a stable target and so callers do not reach past the
/// pagination module into the shape lattice directly.
pub fn merge_page_shapes(accumulated: Option<&Shape>, page: &Shape) -> Shape {
    match accumulated {
        Some(prior) => shape::join(prior, page),
        None => page.clone(),
    }
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

fn cursor_token_from_hints(hints: &Value) -> Option<String> {
    let map = hints.as_object()?;
    for &field in CURSOR_TOKEN_PRIORITY {
        if let Some(token) = map.get(field).and_then(Value::as_str)
            && !is_url(token)
        {
            return Some(token.to_string());
        }
    }
    None
}

fn is_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::infer;

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
        // on the URL continuation when a token is available.
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

    // --- New coverage for the gap-closure work ---

    #[test]
    fn extract_pagination_hints_detects_cursor_family() {
        for field in [
            "cursor",
            "next_cursor",
            "next_token",
            "continuation",
            "continuation_token",
            "nextPageToken",
            "next_page",
            "starting_after",
            "after",
        ] {
            let body = json!({ field: "TOKEN-XYZ" });
            let hints =
                extract_pagination_hints(&body, None).unwrap_or_else(|| panic!("for {field}"));
            assert_eq!(hints["next_cursor"], json!("TOKEN-XYZ"), "field {field}");
            assert_eq!(hints["cursor_param"], json!(field), "field {field}");
        }
    }

    #[test]
    fn extract_pagination_hints_classifies_url_vs_token_next() {
        // URL-valued `next` -> next_url, NOT next_cursor.
        let url_body = json!({"next": "https://api.example.com/page2"});
        let url_hints = extract_pagination_hints(&url_body, None).unwrap();
        assert_eq!(
            url_hints["next_url"],
            json!("https://api.example.com/page2")
        );
        assert!(url_hints.get("next_cursor").is_none());

        // String next_page (non-URL) -> next_cursor (a token), NOT next_url.
        let token_body = json!({"next_page": "opaque-cursor-1"});
        let token_hints = extract_pagination_hints(&token_body, None).unwrap();
        assert_eq!(token_hints["next_cursor"], json!("opaque-cursor-1"));
        assert_eq!(token_hints["cursor_param"], json!("next_page"));
        assert!(token_hints.get("next_url").is_none());
    }

    #[test]
    fn extract_pagination_hints_detects_page_and_offset_strategies() {
        let page_body = json!({"items": [1], "page": 1, "per_page": 25});
        let page_hints = extract_pagination_hints(&page_body, None).unwrap();
        assert_eq!(page_hints["page_strategy"], json!("page_number"));

        let offset_body = json!({"items": [1], "offset": 0, "limit": 25});
        let offset_hints = extract_pagination_hints(&offset_body, None).unwrap();
        assert_eq!(offset_hints["page_strategy"], json!("offset_limit"));
    }

    #[test]
    fn extract_pagination_hints_copies_raw_fields_and_link_through() {
        // Legacy 4-key shape is preserved (adapter contract) plus the Link header.
        let body = json!({"items": [1], "next_page": 2});
        let hints = extract_pagination_hints(
            &body,
            Some("<https://api.example.com/items?page=2>; rel=\"next\""),
        )
        .unwrap();
        assert_eq!(hints["next_page"], json!(2));
        assert!(
            hints["link_rel_next"]
                .as_str()
                .unwrap()
                .contains("rel=\"next\"")
        );
        // next_page is a number here, so it is NOT promoted to next_cursor.
        assert!(hints.get("next_cursor").is_none());
    }

    #[test]
    fn extract_pagination_hints_none_when_empty() {
        assert!(extract_pagination_hints(&json!({"items": [1, 2, 3]}), None).is_none());
        assert!(extract_pagination_hints(&json!({}), None).is_none());
    }

    #[test]
    fn next_args_increments_offset_by_limit() {
        let hints = json!({"page_strategy": "offset_limit"});
        let target = next_args_from_hints(&hints, &json!({"offset": 100, "limit": 25})).unwrap();
        match target {
            PageTarget::Args(args) => {
                assert_eq!(args["offset"], json!(125));
                assert_eq!(args["limit"], json!(25));
            }
            other => panic!("expected Args target, got {other:?}"),
        }
    }

    #[test]
    fn next_args_page_number_strategy_preserves_per_page() {
        let hints = json!({"page_strategy": "page_number"});
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
    fn next_args_writes_cursor_to_existing_starting_after_param() {
        // The response advertises a token under one name (`continuation`),
        // but the request already uses `starting_after`; the existing request
        // param wins over the generic `page_token` default. Exercises the full
        // extract_pagination_hints -> next_args_from_hints pipeline.
        let hints = extract_pagination_hints(&json!({"continuation": "tok_2"}), None).unwrap();
        assert_eq!(hints["next_cursor"], json!("tok_2"));
        let target = next_args_from_hints(&hints, &json!({"starting_after": "tok_1", "limit": 5}));
        match target {
            Some(PageTarget::Args(args)) => {
                assert_eq!(args["starting_after"], json!("tok_2"));
                assert_eq!(args["limit"], json!(5));
            }
            other => panic!("expected Args target, got {other:?}"),
        }
    }

    #[test]
    fn next_args_url_target_only_when_no_cursor() {
        // A cursor wins over a Link rel="next" URL (existing behavior preserved).
        let hints = extract_pagination_hints(
            &json!({"nextPageToken": "CURSOR-2"}),
            Some("<https://api/p2>; rel=\"next\""),
        )
        .unwrap();
        let target = next_args_from_hints(&hints, &json!({"page_token": "CURSOR-1"})).unwrap();
        assert_eq!(target, PageTarget::Args(json!({"page_token": "CURSOR-2"})));
    }

    #[test]
    fn next_args_none_when_has_more_false_and_no_cursor() {
        let hints = extract_pagination_hints(&json!({"has_more": false}), None).unwrap();
        assert!(next_args_from_hints(&hints, &json!({})).is_none());
    }

    #[test]
    fn merge_page_shapes_none_is_identity_and_join_is_monotone() {
        let page1 = infer(&json!({"id": 1, "name": "a"}));
        let page2 = infer(&json!({"id": 2, "label": "b"}));

        // None seeds with the page itself.
        let seed = merge_page_shapes(None, &page1);
        assert_eq!(seed, page1);

        // Folding page2 joins both object fields (monotone: superset of keys).
        let merged = merge_page_shapes(Some(&seed), &page2);
        match &merged {
            Shape::Object { fields, .. } => {
                assert!(fields.contains_key("id"));
                assert!(fields.contains_key("name"));
                assert!(fields.contains_key("label"));
            }
            other => panic!("expected Object, got {other:?}"),
        }

        // Idempotent: joining merged with itself is a no-op.
        let again = merge_page_shapes(Some(&merged), &merged);
        assert_eq!(again, merged);
    }

    #[test]
    fn pagination_allowed_is_false_for_each_unsafe_effect_independently() {
        fn effects(
            read_only: bool,
            mutating: bool,
            shell: bool,
            sensitive: bool,
        ) -> crate::EffectSet {
            crate::EffectSet {
                read_only,
                mutating,
                network: true,
                shell,
                sensitive,
                cacheable: true,
                requires_confirmation: false,
                extra: crate::Extra::new(),
            }
        }

        // Every unsafe combination is independently forbidden.
        assert!(!pagination_allowed(&effects(true, true, false, false))); // mutating
        assert!(!pagination_allowed(&effects(true, false, true, false))); // shell
        assert!(!pagination_allowed(&effects(true, false, false, true))); // sensitive
        assert!(!pagination_allowed(&effects(false, false, false, false))); // not read_only

        // Only the all-safe combination is allowed.
        assert!(pagination_allowed(&effects(true, false, false, false)));
    }

    use serde_json::json;
}
