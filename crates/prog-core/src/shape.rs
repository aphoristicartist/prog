use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STRING_ENUM_MAX_VALUES: usize = 8;
pub const STRING_ENUM_MAX_LEN: usize = 40;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Unknown,
    Null,
    Boolean,
    Integer,
    Number,
    String {
        values: Option<BTreeSet<String>>,
    },
    Timestamp,
    Array {
        items: Box<Shape>,
    },
    Object {
        fields: BTreeMap<String, FieldShape>,
        rest: Box<Shape>,
    },
    Union {
        variants: Vec<Shape>,
    },
    Sensitive {
        inner: Box<Shape>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct FieldShape {
    pub shape: Shape,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub seen: u64,
}

impl Shape {
    pub fn string_value(value: &str) -> Self {
        if value.len() > STRING_ENUM_MAX_LEN {
            return Self::plain_string();
        }

        let mut values = BTreeSet::new();
        values.insert(value.to_string());
        Self::String {
            values: Some(values),
        }
    }

    pub fn plain_string() -> Self {
        Self::String { values: None }
    }
}

pub fn infer(value: &Value) -> Shape {
    match value {
        Value::Null => Shape::Null,
        Value::Bool(_) => Shape::Boolean,
        Value::Number(number) if number.is_i64() || number.is_u64() => Shape::Integer,
        Value::Number(_) => Shape::Number,
        Value::String(value) if value.starts_with("[REDACTED:") => Shape::Sensitive {
            inner: Box::new(Shape::plain_string()),
        },
        Value::String(value) if chrono::DateTime::parse_from_rfc3339(value).is_ok() => {
            Shape::Timestamp
        }
        Value::String(value) => Shape::string_value(value),
        Value::Array(values) => {
            let items = values
                .iter()
                .map(infer)
                .reduce(|left, right| join(&left, &right))
                .unwrap_or(Shape::Unknown);
            Shape::Array {
                items: Box::new(items),
            }
        }
        Value::Object(map) => {
            let fields = map
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        FieldShape {
                            shape: infer(value),
                            optional: false,
                            seen: 1,
                        },
                    )
                })
                .collect();
            Shape::Object {
                fields,
                rest: Box::new(Shape::Unknown),
            }
        }
    }
}

pub fn join(left: &Shape, right: &Shape) -> Shape {
    use Shape::{
        Array, Boolean, Integer, Null, Number, Object, Sensitive, String as StringShape, Timestamp,
        Union, Unknown,
    };

    match (canonicalize(left), canonicalize(right)) {
        (Unknown, shape) | (shape, Unknown) => shape,
        (Sensitive { inner: left }, Sensitive { inner: right }) => Sensitive {
            inner: Box::new(join(&left, &right)),
        },
        (Sensitive { inner }, shape) | (shape, Sensitive { inner }) => Sensitive {
            inner: Box::new(join(&inner, &shape)),
        },
        (Union { variants }, shape) | (shape, Union { variants }) => {
            canonical_union(variants.into_iter().chain([shape]).collect())
        }
        (Null, Null) => Null,
        (Null, shape) | (shape, Null) => canonical_union(vec![Null, shape]),
        (Boolean, Boolean) => Boolean,
        (Integer, Integer) => Integer,
        (Number, Number) | (Integer, Number) | (Number, Integer) => Number,
        (StringShape { values: left }, StringShape { values: right }) => join_strings(left, right),
        (Timestamp, Timestamp) => Timestamp,
        (Timestamp, StringShape { .. }) | (StringShape { .. }, Timestamp) => Shape::plain_string(),
        (Array { items: left }, Array { items: right }) => Array {
            items: Box::new(join(&left, &right)),
        },
        (
            Object {
                fields: left_fields,
                rest: left_rest,
            },
            Object {
                fields: right_fields,
                rest: right_rest,
            },
        ) => join_objects(left_fields, *left_rest, right_fields, *right_rest),
        (left, right) => canonical_union(vec![left, right]),
    }
}

pub fn render_hints(shape: &Shape, base_path: &str) -> BTreeMap<String, String> {
    let mut hints = BTreeMap::new();
    render_shape_hints(shape, base_path, None, &mut hints);
    hints
}

fn canonicalize(shape: &Shape) -> Shape {
    match shape {
        Shape::Array { items } => Shape::Array {
            items: Box::new(canonicalize(items)),
        },
        Shape::Object { fields, rest } => Shape::Object {
            fields: fields
                .iter()
                .map(|(key, field)| {
                    (
                        key.clone(),
                        FieldShape {
                            shape: canonicalize(&field.shape),
                            optional: field.optional,
                            seen: field.seen,
                        },
                    )
                })
                .collect(),
            rest: Box::new(canonicalize(rest)),
        },
        Shape::Union { variants } => canonical_union(variants.clone()),
        Shape::Sensitive { inner } => Shape::Sensitive {
            inner: Box::new(canonicalize(inner)),
        },
        Shape::String {
            values: Some(values),
        } if values.len() > STRING_ENUM_MAX_VALUES
            || values.iter().any(|value| value.len() > STRING_ENUM_MAX_LEN) =>
        {
            Shape::plain_string()
        }
        shape => shape.clone(),
    }
}

fn join_strings(left: Option<BTreeSet<String>>, right: Option<BTreeSet<String>>) -> Shape {
    match (left, right) {
        (Some(mut left), Some(right)) => {
            left.extend(right);
            if left.len() > STRING_ENUM_MAX_VALUES
                || left.iter().any(|value| value.len() > STRING_ENUM_MAX_LEN)
            {
                Shape::plain_string()
            } else {
                Shape::String { values: Some(left) }
            }
        }
        _ => Shape::plain_string(),
    }
}

fn join_objects(
    left_fields: BTreeMap<String, FieldShape>,
    left_rest: Shape,
    right_fields: BTreeMap<String, FieldShape>,
    right_rest: Shape,
) -> Shape {
    let mut fields = BTreeMap::new();
    let keys: BTreeSet<String> = left_fields
        .keys()
        .chain(right_fields.keys())
        .cloned()
        .collect();

    for key in keys {
        let field = match (left_fields.get(&key), right_fields.get(&key)) {
            (Some(left), Some(right)) => FieldShape {
                shape: join(&left.shape, &right.shape),
                optional: left.optional || right.optional,
                seen: left.seen.max(right.seen),
            },
            (Some(left), None) => FieldShape {
                shape: join(&left.shape, &right_rest),
                optional: true,
                seen: left.seen,
            },
            (None, Some(right)) => FieldShape {
                shape: join(&left_rest, &right.shape),
                optional: true,
                seen: right.seen,
            },
            (None, None) => unreachable!("key came from one side"),
        };
        fields.insert(key, field);
    }

    Shape::Object {
        fields,
        rest: Box::new(join(&left_rest, &right_rest)),
    }
}

fn canonical_union(variants: Vec<Shape>) -> Shape {
    let mut flattened = Vec::new();
    let mut sensitive = false;
    for variant in variants {
        collect_union_variant(variant, &mut flattened, &mut sensitive);
    }

    let normalized = canonical_union_from_flattened(flattened);
    if sensitive {
        Shape::Sensitive {
            inner: Box::new(normalized),
        }
    } else {
        normalized
    }
}

fn collect_union_variant(shape: Shape, flattened: &mut Vec<Shape>, sensitive: &mut bool) {
    match shape {
        Shape::Union { variants } => {
            for variant in variants {
                collect_union_variant(variant, flattened, sensitive);
            }
        }
        Shape::Sensitive { inner } => {
            *sensitive = true;
            collect_union_variant(*inner, flattened, sensitive);
        }
        Shape::Unknown => {}
        shape => flattened.push(canonicalize(&shape)),
    }
}

fn canonical_union_from_flattened(variants: Vec<Shape>) -> Shape {
    let mut has_null = false;
    let mut has_boolean = false;
    let mut has_integer = false;
    let mut has_number = false;
    let mut stringish: Option<Shape> = None;
    let mut array_items: Option<Shape> = None;
    let mut object_shape: Option<Shape> = None;
    let mut output = Vec::new();

    for variant in variants.into_iter().map(|shape| canonicalize(&shape)) {
        match variant {
            Shape::Unknown => {}
            Shape::Union { variants } => output.push(canonical_union(variants)),
            Shape::Sensitive { inner } => output.push(Shape::Sensitive { inner }),
            Shape::Null => has_null = true,
            Shape::Boolean => has_boolean = true,
            Shape::Integer => has_integer = true,
            Shape::Number => has_number = true,
            Shape::Timestamp | Shape::String { .. } => {
                stringish = Some(match stringish {
                    Some(existing) => join(&existing, &variant),
                    None => variant,
                });
            }
            Shape::Array { items } => {
                array_items = Some(match array_items {
                    Some(existing) => join(&existing, &items),
                    None => *items,
                });
            }
            Shape::Object { .. } => {
                object_shape = Some(match object_shape {
                    Some(existing) => join(&existing, &variant),
                    None => variant,
                });
            }
        }
    }

    if has_null {
        output.push(Shape::Null);
    }
    if has_boolean {
        output.push(Shape::Boolean);
    }
    if has_number {
        output.push(Shape::Number);
    } else if has_integer {
        output.push(Shape::Integer);
    }
    if let Some(shape) = stringish {
        output.push(shape);
    }
    if let Some(items) = array_items {
        output.push(Shape::Array {
            items: Box::new(items),
        });
    }
    if let Some(shape) = object_shape {
        output.push(shape);
    }

    output.sort_by_key(shape_sort_key);
    output.dedup();

    match output.len() {
        0 => Shape::Unknown,
        1 => output.pop().expect("one output"),
        _ => Shape::Union { variants: output },
    }
}

fn shape_sort_key(shape: &Shape) -> (u8, String) {
    match shape {
        Shape::Unknown => (0, std::string::String::new()),
        Shape::Null => (1, std::string::String::new()),
        Shape::Boolean => (2, std::string::String::new()),
        Shape::Integer => (3, std::string::String::new()),
        Shape::Number => (4, std::string::String::new()),
        Shape::Timestamp => (5, std::string::String::new()),
        Shape::String { .. } => (6, render_label(shape)),
        Shape::Array { .. } => (7, render_label(shape)),
        Shape::Object { .. } => (8, render_label(shape)),
        Shape::Union { .. } => (9, render_label(shape)),
        Shape::Sensitive { .. } => (10, render_label(shape)),
    }
}

fn render_shape_hints(
    shape: &Shape,
    path: &str,
    field: Option<&FieldShape>,
    hints: &mut BTreeMap<String, String>,
) {
    let label = with_field_suffix(render_label(shape), field);
    if !path.is_empty() || !matches!(shape, Shape::Object { .. }) {
        hints.insert(path.to_string(), label);
    }

    match shape {
        Shape::Array { items } => {
            render_shape_hints(items, &push_path(path, "*"), None, hints);
        }
        Shape::Object { fields, .. } => {
            for (name, field) in fields {
                render_shape_hints(&field.shape, &push_path(path, name), Some(field), hints);
            }
        }
        _ => {}
    }
}

fn with_field_suffix(mut label: String, field: Option<&FieldShape>) -> String {
    let Some(field) = field else {
        return label;
    };

    if field.optional {
        if field.seen <= 1 {
            label.push_str(", optional, rarely present");
        } else {
            label.push_str(", optional");
        }
    }

    label
}

fn render_label(shape: &Shape) -> String {
    match shape {
        Shape::Unknown => "unknown".to_string(),
        Shape::Null => "null".to_string(),
        Shape::Boolean => "boolean".to_string(),
        Shape::Integer => "integer".to_string(),
        Shape::Number => "number".to_string(),
        Shape::String {
            values: Some(values),
        } => values
            .iter()
            .map(|value| serde_json::to_string(value).expect("string labels serialize"))
            .collect::<Vec<_>>()
            .join(" | "),
        Shape::String { values: None } => "string".to_string(),
        Shape::Timestamp => "timestamp".to_string(),
        Shape::Array { items } => format!("array<{}>", render_label(items)),
        Shape::Object { rest, .. } if matches!(**rest, Shape::Unknown) => {
            "object, rest unknown".to_string()
        }
        Shape::Object { .. } => "object".to_string(),
        Shape::Union { variants } => render_union_label(variants),
        Shape::Sensitive { inner } => format!("sensitive<{}>", render_label(inner)),
    }
}

fn render_union_label(variants: &[Shape]) -> String {
    let mut labels: Vec<String> = variants
        .iter()
        .filter(|shape| !matches!(shape, Shape::Null))
        .map(render_label)
        .collect();
    labels.sort();
    if variants.iter().any(|shape| matches!(shape, Shape::Null)) {
        labels.push("null".to_string());
    }
    labels.join(" | ")
}

fn push_path(base: &str, segment: &str) -> String {
    if base.is_empty() {
        format!("/{}", escape(segment))
    } else {
        format!("{base}/{}", escape(segment))
    }
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}
