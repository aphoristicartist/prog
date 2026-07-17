//! Integration coverage for upstream auto-pagination (issue #69).
//!
//! These tests own the executable side of invariant I10
//! ("auto-pagination never escapes the effect policy or the envelope budget")
//! against the pure core machinery: the effect gate, the monotone page-shape
//! merge, finite page/byte/time caps, fail-closed page cursors, and the
//! continuation target. The end-to-end envelope-byte budget assertion lives in
//! `crates/prog-cli/tests/cli.rs`.

use prog_core::{
    EffectSet, Extra, PageCaps, PageTarget, Shape, Store, extract_pagination_hints, infer, join,
    merge_page_shapes, next_args_from_hints, pagination_allowed,
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn effects(read_only: bool, mutating: bool, shell: bool, sensitive: bool) -> EffectSet {
    EffectSet {
        read_only,
        mutating,
        network: true,
        shell,
        sensitive,
        cacheable: true,
        requires_confirmation: false,
        extra: Extra::new(),
    }
}

/// I10 (effect-policy half): pagination is only allowed for the all-safe
/// (read-only, non-mutating, non-shell, non-sensitive) combination. Every
/// unsafe axis independently forbids auto-pagination, so the follow loop can
/// never chase pages on an operation that could mutate state or touch secrets.
#[test]
fn pagination_respects_effect_policy_and_envelope_budget() {
    // Effect policy: each unsafe axis independently blocks pagination.
    assert!(!pagination_allowed(&effects(true, true, false, false)));
    assert!(!pagination_allowed(&effects(true, false, true, false)));
    assert!(!pagination_allowed(&effects(true, false, false, true)));
    assert!(!pagination_allowed(&effects(false, false, false, false)));
    assert!(pagination_allowed(&effects(true, false, false, false)));

    // Envelope budget: the page/byte/time caps are always finite, so a runaway
    // upstream can never grow the follow loop (or the envelope) without bound.
    let caps = PageCaps::default();
    assert!(caps.max_pages >= 1);
    assert!(caps.max_total_bytes > 0);
    assert!(caps.max_wall_ms > 0);
    // The default byte cap is well within the disclosure envelope class.
    assert!(caps.max_total_bytes <= 16 * 1024 * 1024);

    // Page shapes merge monotonically: the fold is a superset of every input,
    // so projecting the merged shape never invents values (I1) and the union
    // shape stays bounded by the I5 enum-absorbing caps.
    let pages = [
        infer(&json!({"id": 1, "name": "a"})),
        infer(&json!({"id": 2, "label": "b"})),
        infer(&json!({"id": 3, "extra": {"nested": true}})),
    ];
    let merged = pages
        .iter()
        .fold(None::<Shape>, |acc, page| {
            Some(merge_page_shapes(acc.as_ref(), page))
        })
        .unwrap();
    for page in &pages {
        // join(merged, page) == merged: the merged shape already absorbed page.
        assert_eq!(join(&merged, page), merged);
    }
}

/// I5 backing law for page merges: monotone, idempotent, and `None` is the
/// identity on either side.
#[test]
fn shape_merges_monotonically_across_n_pages() {
    let page_a = infer(&json!({"id": 1, "name": "a"}));
    let page_b = infer(&json!({"id": 2, "label": "b"}));

    // None is identity on both sides.
    assert_eq!(merge_page_shapes(None, &page_a), page_a);
    assert_eq!(merge_page_shapes(Some(&page_a), &Shape::Unknown), page_a);

    let merged = merge_page_shapes(Some(&page_a), &page_b);
    // Idempotent.
    assert_eq!(merge_page_shapes(Some(&merged), &merged), merged);
    // Commutative across the Option wrapper.
    assert_eq!(
        merge_page_shapes(Some(&page_a), &page_b),
        merge_page_shapes(Some(&page_b), &page_a)
    );
}

/// A resume continuation is surfaced only when the loop paused at a cap (the
/// next page target is still resolvable). `NoMore` never surfaces one.
#[test]
fn continuation_next_actions_surfaced_when_stop_reason_is_cap() {
    // Cursor target: hints advertise another token.
    let hints = extract_pagination_hints(&json!({"next_cursor": "tok_2"}), None).unwrap();
    let target = next_args_from_hints(&hints, &json!({"page_token": "tok_1"}));
    assert!(
        matches!(target, Some(PageTarget::Args(_))),
        "cap stop must surface a resume target, got {target:?}"
    );

    // Offset target: strategy + increment.
    let hints = extract_pagination_hints(&json!({"offset": 0, "limit": 25}), None).unwrap();
    let target = next_args_from_hints(&hints, &json!({"offset": 25, "limit": 25}));
    match target {
        Some(PageTarget::Args(args)) => assert_eq!(args["offset"], json!(50)),
        other => panic!("expected Args (offset) resume, got {other:?}"),
    }
}

#[test]
fn continuation_absent_on_no_more() {
    // has_more false and no cursor/strategy => no next page.
    let hints = extract_pagination_hints(&json!({"has_more": false}), None).unwrap();
    assert!(next_args_from_hints(&hints, &json!({})).is_none());

    // No pagination signals at all.
    assert!(extract_pagination_hints(&json!({"items": [1, 2, 3]}), None).is_none());
}

/// Regression: `next_args_from_hints` resolves a cursor (and a page-number
/// increment) from the NORMALIZED hint keys alone. Raw body fields are no
/// longer copied into the hint object, so resolution must succeed purely via
/// `next_cursor` / `cursor_param` / `page_strategy` / `has_more` /
/// `next_url` / `link_rel_next`.
#[test]
fn next_args_resolves_cursor_and_page_from_normalized_keys_only() {
    // Cursor: extract_pagination_hints normalizes nextPageToken -> next_cursor.
    let hints = extract_pagination_hints(&json!({"nextPageToken": "tok_2"}), None).unwrap();
    assert!(
        hints.get("nextPageToken").is_none(),
        "raw key must not be copied"
    );
    assert_eq!(hints["next_cursor"], json!("tok_2"));
    let target = next_args_from_hints(&hints, &json!({"page_token": "tok_1"})).unwrap();
    match target {
        PageTarget::Args(args) => assert_eq!(args["page_token"], json!("tok_2")),
        other => panic!("expected Args (cursor), got {other:?}"),
    }

    // Page-number increment from normalized page_strategy + base args.
    let hints =
        extract_pagination_hints(&json!({"page": 1, "per_page": 20, "has_more": true}), None)
            .unwrap();
    assert_eq!(hints["page_strategy"], json!("page_number"));
    let target = next_args_from_hints(&hints, &json!({"page": 2, "per_page": 20})).unwrap();
    match target {
        PageTarget::Args(args) => {
            assert_eq!(args["page"], json!(3));
            assert_eq!(args["per_page"], json!(20));
        }
        other => panic!("expected Args (page), got {other:?}"),
    }
}

/// I9 fail-closed reuse: a page cursor minted with `create_cursor_with_extra`
/// is rejected when missing and an unknown token is rejected
/// outright, exactly like a normal expand cursor. The page metadata in `extra`
/// rides along but never weakens validation.
#[test]
fn page_cursors_fail_closed_when_missing_or_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let mut extra = serde_json::Map::new();
    extra.insert("kind".to_string(), json!("page"));
    extra.insert("page".to_string(), json!(2));
    let token = store
        .create_cursor_with_extra("ck_page2", "api", "list", "", 60, extra)
        .unwrap();
    assert!(token.starts_with("pc1_"));

    assert!(store.get_cursor(&token).is_ok());

    // Foreign / unknown token fails closed.
    let err = store.get_cursor("pc1_foreign").unwrap_err();
    assert!(
        matches!(err, prog_core::CoreError::CursorNotFound(_)),
        "expected CursorNotFound, got {err:?}"
    );
}

/// I9 fail-closed (expiry axis): a page cursor minted with
/// `create_cursor_with_extra` must fail closed once its `expires_at` has
/// passed, exactly like a normal expand cursor.
#[test]
fn page_cursor_fails_closed_when_expired() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let mut extra = serde_json::Map::new();
    extra.insert("kind".to_string(), json!("page"));
    extra.insert("page".to_string(), json!(2));
    let token = store
        .create_cursor_with_extra("ck_expiring", "api", "list", "", 60, extra)
        .unwrap();

    // A `now` well past the cursor's expires_at must fail closed.
    let future = chrono::Utc::now() + chrono::Duration::seconds(3600);
    let err = store.get_cursor_at(&token, future).unwrap_err();
    assert!(
        matches!(err, prog_core::CoreError::CursorExpired(_, _)),
        "expected CursorExpired, got {err:?}"
    );
}

// --- Property tests: the pure pagination laws ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// extract_pagination_hints is total over arbitrary JSON: it never panics
    /// and always returns either None or an Object.
    #[test]
    fn extract_pagination_hints_is_total(value in arbitrary_json(3, 6)) {
        let hints = extract_pagination_hints(&value, None);
        match hints {
            None => {}
            Some(Value::Object(_)) => {}
            other => prop_assert!(false, "expected None or Object, got {other:?}"),
        }
    }

    /// next_args_from_hints is a pure deterministic function of (hints, args).
    #[test]
    fn next_args_from_hints_is_deterministic(
        body in arbitrary_json(3, 5),
        args in arbitrary_json(2, 5),
    ) {
        let hints = extract_pagination_hints(&body, None);
        let a = hints.as_ref().and_then(|h| next_args_from_hints(h, &args));
        let b = hints.as_ref().and_then(|h| next_args_from_hints(h, &args));
        prop_assert_eq!(a, b);
    }

    /// merge_page_shapes is associative and commutative over arbitrary shapes
    /// (it delegates to shape::join / I5; this pins the Option<&Shape> wrapper).
    #[test]
    fn merge_page_shapes_is_associative_and_commutative(
        a in arbitrary_json(3, 5),
        b in arbitrary_json(3, 5),
        c in arbitrary_json(3, 5),
    ) {
        let sa = infer(&a);
        let sb = infer(&b);
        let sc = infer(&c);

        // Commutative.
        prop_assert_eq!(
            merge_page_shapes(Some(&sa), &sb),
            merge_page_shapes(Some(&sb), &sa)
        );

        // Associative: (a join b) join c == a join (b join c).
        let left = merge_page_shapes(Some(&merge_page_shapes(Some(&sa), &sb)), &sc);
        let right = merge_page_shapes(Some(&sa), &merge_page_shapes(Some(&sb), &sc));
        prop_assert_eq!(left, right);
    }
}

/// Bounded arbitrary JSON generator (depth + width capped), mirroring the
/// disclosure.rs strategy so pagination property tests stay fast and total.
fn arbitrary_json(max_depth: u32, max_width: usize) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_map(|n| serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null)),
        ".*".prop_map(Value::String),
    ];
    leaf.prop_recursive(
        max_depth,
        max_width as u32,
        max_width as u32,
        move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..max_width).prop_map(Value::Array),
                prop::collection::vec((".*", inner), 0..max_width)
                    .prop_map(|pairs| Value::Object(pairs.into_iter().collect())),
            ]
        },
    )
}
