use prog_core::{FieldShape, STRING_ENUM_MAX_VALUES, Shape, infer, join, render_hints};
use proptest::prelude::*;
use serde_json::json;

#[test]
fn infer_splits_json_scalar_shapes() {
    assert_eq!(infer(&json!(1)), Shape::Integer);
    assert_eq!(infer(&json!(1.25)), Shape::Number);
    assert_eq!(infer(&json!(true)), Shape::Boolean);
    assert_eq!(infer(&json!(null)), Shape::Null);
    assert_eq!(infer(&json!("2026-07-04T03:00:00Z")), Shape::Timestamp);
    assert_eq!(infer(&json!("open")), string_values(["open"]));
}

#[test]
fn redaction_sentinels_infer_as_sensitive_plain_strings() {
    assert_eq!(
        infer(&json!("[REDACTED:token]")),
        Shape::Sensitive {
            inner: Box::new(Shape::plain_string())
        }
    );
}

#[test]
fn infer_arrays_fold_join_and_keep_empty_arrays_unknown() {
    assert_eq!(
        infer(&json!([])),
        Shape::Array {
            items: Box::new(Shape::Unknown)
        }
    );
    assert_eq!(
        infer(&json!([1, 2.5, null])),
        Shape::Array {
            items: Box::new(Shape::Union {
                variants: vec![Shape::Null, Shape::Number]
            })
        }
    );
}

#[test]
fn object_join_marks_one_sided_fields_optional_and_counts_seen_fields() {
    let left = infer(&json!({"id": 1, "state": "open"}));
    let right = infer(&json!({"id": 2, "title": "hello"}));

    let Shape::Object { fields, rest } = join(&left, &right) else {
        panic!("join should stay object-shaped");
    };

    assert_eq!(*rest, Shape::Unknown);
    assert_eq!(fields["id"].shape, Shape::Integer);
    assert!(!fields["id"].optional);
    assert_eq!(fields["id"].seen, 1);
    assert!(fields["state"].optional);
    assert_eq!(fields["state"].seen, 1);
    assert!(fields["title"].optional);
}

#[test]
fn string_value_sets_absorb_to_plain_string_past_bounds() {
    let mut shape = Shape::Unknown;
    for value in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        shape = join(&shape, &string_values([value]));
    }
    assert!(matches!(shape, Shape::String { values: Some(_) }));

    shape = join(&shape, &string_values(["i"]));
    assert_eq!(shape, Shape::plain_string());

    assert_eq!(
        infer(&json!("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")),
        Shape::plain_string()
    );
}

#[test]
fn sensitivity_is_sticky_under_join() {
    let sensitive = Shape::Sensitive {
        inner: Box::new(Shape::Integer),
    };

    assert_eq!(
        join(&sensitive, &Shape::Number),
        Shape::Sensitive {
            inner: Box::new(Shape::Number)
        }
    );
    assert_eq!(
        join(&Shape::Null, &sensitive),
        Shape::Sensitive {
            inner: Box::new(Shape::Union {
                variants: vec![Shape::Null, Shape::Integer]
            })
        }
    );
}

#[test]
fn render_hints_uses_json_pointers_wildcards_and_optional_suffixes() {
    let shape = join(
        &infer(&json!({
            "items": [
                {"state": "open", "body/text": "hidden", "owner~name": "alice"}
            ]
        })),
        &infer(&json!({
            "items": [
                {"state": "closed"}
            ]
        })),
    );

    let hints = render_hints(&shape, "");
    assert_eq!(hints["/items/*/state"], "\"closed\" | \"open\"");
    assert_eq!(
        hints["/items/*/body~1text"],
        "\"hidden\", optional, rarely present"
    );
    assert_eq!(
        hints["/items/*/owner~0name"],
        "\"alice\", optional, rarely present"
    );
    assert_eq!(hints["/items/*"], "object, rest unknown");
}

#[test]
fn nullable_values_render_null_last() {
    let shape = join(&infer(&json!("ready")), &Shape::Null);
    assert_eq!(render_hints(&shape, "")[""], "\"ready\" | null");
}

fn string_values<const N: usize>(values: [&str; N]) -> Shape {
    Shape::String {
        values: Some(values.into_iter().map(str::to_string).collect()),
    }
}

fn shape_strategy() -> impl Strategy<Value = Shape> {
    let leaf = prop_oneof![
        Just(Shape::Unknown),
        Just(Shape::Null),
        Just(Shape::Boolean),
        Just(Shape::Integer),
        Just(Shape::Number),
        Just(Shape::Timestamp),
        Just(Shape::plain_string()),
        enum_string_shape(),
    ];

    leaf.prop_recursive(4, 48, 4, |inner| {
        prop_oneof![
            inner.clone().prop_map(|items| Shape::Array {
                items: Box::new(items)
            }),
            prop::collection::btree_map("[a-z]{1,4}", field_strategy(inner.clone()), 0..4)
                .prop_map(|fields| Shape::Object {
                    fields,
                    rest: Box::new(Shape::Unknown)
                }),
            prop::collection::vec(inner.clone(), 2..5).prop_map(canonical_from_parts),
            inner.prop_map(|shape| Shape::Sensitive {
                inner: Box::new(shape)
            }),
        ]
    })
}

fn field_strategy(shape: impl Strategy<Value = Shape>) -> impl Strategy<Value = FieldShape> {
    (shape, any::<bool>(), 1_u64..8).prop_map(|(shape, optional, seen)| FieldShape {
        shape,
        optional,
        seen,
    })
}

fn enum_string_shape() -> impl Strategy<Value = Shape> {
    prop::collection::btree_set("[a-l]{1,3}", 1..=8).prop_map(|values| Shape::String {
        values: Some(values),
    })
}

fn enum_cap_shape() -> impl Strategy<Value = Shape> {
    prop::collection::btree_set(
        "[a-l]{1,3}",
        STRING_ENUM_MAX_VALUES..=STRING_ENUM_MAX_VALUES,
    )
    .prop_map(|values| Shape::String {
        values: Some(values),
    })
}

fn canonical_from_parts(parts: Vec<Shape>) -> Shape {
    parts
        .into_iter()
        .reduce(|left, right| join(&left, &right))
        .unwrap_or(Shape::Unknown)
}

proptest! {
    #[test]
    fn join_is_commutative(a in shape_strategy(), b in shape_strategy()) {
        prop_assert_eq!(join(&a, &b), join(&b, &a));
    }

    #[test]
    fn join_is_associative(a in shape_strategy(), b in shape_strategy(), c in shape_strategy()) {
        prop_assert_eq!(join(&join(&a, &b), &c), join(&a, &join(&b, &c)));
    }

    #[test]
    fn join_is_idempotent(a in shape_strategy()) {
        prop_assert_eq!(join(&a, &a), a);
    }

    #[test]
    fn unknown_is_join_identity(a in shape_strategy()) {
        prop_assert_eq!(join(&Shape::Unknown, &a), a.clone());
        prop_assert_eq!(join(&a, &Shape::Unknown), a);
    }

    #[test]
    fn join_is_monotone_by_absorption(a in shape_strategy(), b in shape_strategy()) {
        let joined = join(&a, &b);
        prop_assert_eq!(join(&joined, &a), joined.clone());
        prop_assert_eq!(join(&joined, &b), joined);
    }

    #[test]
    fn string_enum_absorption_is_associative_at_cap_boundary(
        a in enum_cap_shape(),
        b in enum_cap_shape(),
        c in enum_cap_shape()
    ) {
        prop_assert_eq!(join(&join(&a, &b), &c), join(&a, &join(&b, &c)));
    }
}
