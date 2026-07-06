use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    CoreError, ExpandablePayload, OmissionReason, OmittedRegion, Result, ScopedSlice,
    pointer::{get, push, siblings_hint},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PreviewPolicy {
    pub array_items: usize,
    pub object_fields: usize,
    pub string_chars: usize,
    pub depth: usize,
    pub node_budget: usize,
    pub max_envelope_bytes: usize,
}

impl Default for PreviewPolicy {
    fn default() -> Self {
        Self {
            array_items: 5,
            object_fields: 24,
            string_chars: 160,
            depth: 4,
            node_budget: 400,
            max_envelope_bytes: 16 * 1024,
        }
    }
}

impl PreviewPolicy {
    pub fn with_limit_and_depth(&self, limit: Option<usize>, depth: Option<usize>) -> Self {
        let mut policy = self.clone();
        if let Some(limit) = limit {
            policy.array_items = limit;
        }
        if let Some(depth) = depth {
            policy.depth = depth;
        }
        policy
    }

    fn coarsen(&self) -> Self {
        Self {
            array_items: halve_to_zero(self.array_items),
            object_fields: halve_to_zero(self.object_fields),
            string_chars: halve_to_zero(self.string_chars).max(16),
            depth: self.depth.saturating_sub(1),
            node_budget: halve_to_zero(self.node_budget).max(1),
            max_envelope_bytes: self.max_envelope_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Projection {
    pub preview: Value,
    #[serde(default)]
    pub omitted: Vec<OmittedRegion>,
}

pub fn project(value: &Value, policy: &PreviewPolicy, base_path: &str) -> Projection {
    let mut current = policy.clone();
    let mut last = project_once(value, &current, base_path);

    for _ in 0..16 {
        let Ok(bytes) = serde_json::to_vec(&last) else {
            return last;
        };
        if bytes.len() <= current.max_envelope_bytes {
            return last;
        }

        let next = current.coarsen();
        if next == current {
            return last;
        }
        current = next;
        last = project_once(value, &current, base_path);
    }

    last
}

pub fn expand(
    payload: &impl ExpandablePayload,
    slice: &ScopedSlice,
    policy: &PreviewPolicy,
) -> Result<Projection> {
    let (target_path, selected) = slice_value(payload, slice)?;
    let request = slice.request();
    let effective_policy = policy.with_limit_and_depth(request.limit, request.depth);
    Ok(project(&selected, &effective_policy, &target_path))
}

pub fn slice_value(
    payload: &impl ExpandablePayload,
    slice: &ScopedSlice,
) -> Result<(String, Value)> {
    let value = payload.expansion_value();
    let target_path = slice.target_path().as_str();
    let target = get(value, target_path)?.ok_or_else(|| CoreError::PathNotFound {
        path: target_path.to_string(),
        hint: siblings_hint(value, target_path),
    })?;
    let request = slice.request();
    let selected = select_fields(target, &request.fields, &request.omit);
    Ok((target_path.to_string(), selected))
}

fn project_once(value: &Value, policy: &PreviewPolicy, base_path: &str) -> Projection {
    let mut projector = Projector {
        policy,
        nodes_used: 0,
        omitted: Vec::new(),
    };
    let preview = projector.project_value(value, base_path, policy.depth);
    Projection {
        preview,
        omitted: projector.omitted,
    }
}

struct Projector<'p> {
    policy: &'p PreviewPolicy,
    nodes_used: usize,
    omitted: Vec<OmittedRegion>,
}

impl Projector<'_> {
    fn project_value(&mut self, value: &Value, path: &str, depth: usize) -> Value {
        if self.nodes_used >= self.policy.node_budget {
            self.omit(
                path,
                OmissionReason::NodeBudget,
                "global node budget reached",
            );
            return marker_for(value);
        }
        self.nodes_used += 1;

        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
            Value::String(text) if text.starts_with("[REDACTED:") => {
                self.omit(path, OmissionReason::Redacted, "redacted by policy");
                Value::String("«redacted»".to_string())
            }
            Value::String(text) => self.project_string(text, path),
            Value::Array(items) => self.project_array(items, path, depth),
            Value::Object(map) => self.project_object(map, path, depth),
        }
    }

    fn project_string(&mut self, text: &str, path: &str) -> Value {
        let char_count = text.chars().count();
        if char_count <= self.policy.string_chars {
            return Value::String(text.to_string());
        }

        let prefix: String = text.chars().take(self.policy.string_chars).collect();
        self.omit(
            path,
            OmissionReason::LargeString,
            format!("{} chars, showing {}", char_count, self.policy.string_chars),
        );
        Value::String(format!("{prefix}…"))
    }

    fn project_array(&mut self, items: &[Value], path: &str, depth: usize) -> Value {
        if depth == 0 {
            self.omit(
                path,
                OmissionReason::DeepObject,
                format!("array has {} items", items.len()),
            );
            return Value::String(format!("«array: {} items»", items.len()));
        }

        let showing = items.len().min(self.policy.array_items);
        if items.len() > showing {
            self.omit(
                path,
                OmissionReason::LongArray,
                format!("{} items, showing {showing}", items.len()),
            );
        }

        Value::Array(
            items
                .iter()
                .take(showing)
                .enumerate()
                .map(|(index, value)| {
                    self.project_value(value, &push(path, &index.to_string()), depth - 1)
                })
                .collect(),
        )
    }

    fn project_object(&mut self, map: &Map<String, Value>, path: &str, depth: usize) -> Value {
        if depth == 0 {
            self.omit(
                path,
                OmissionReason::DeepObject,
                format!("object has {} fields", map.len()),
            );
            return Value::String(format!("«object: {} fields»", map.len()));
        }

        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        let showing = keys.len().min(self.policy.object_fields);
        if keys.len() > showing {
            self.omit(
                path,
                OmissionReason::ManyFields,
                format!("{} fields, showing {showing}", keys.len()),
            );
        }

        let mut preview = Map::new();
        for key in keys.into_iter().take(showing) {
            preview.insert(
                key.clone(),
                self.project_value(&map[key], &push(path, key), depth - 1),
            );
        }
        Value::Object(preview)
    }

    fn omit(&mut self, path: &str, reason: OmissionReason, detail: impl Into<String>) {
        self.omitted.push(OmittedRegion {
            path: path.to_string(),
            reason,
            detail: Some(detail.into()),
            extra: Map::new(),
        });
    }
}

fn select_fields(value: &Value, fields: &[String], omit: &[String]) -> Value {
    match value {
        Value::Object(map) => Value::Object(select_object_fields(map, fields, omit)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| select_fields(item, fields, omit))
                .collect(),
        ),
        scalar => scalar.clone(),
    }
}

fn select_object_fields(
    map: &Map<String, Value>,
    fields: &[String],
    omit: &[String],
) -> Map<String, Value> {
    let omit: std::collections::BTreeSet<&str> = omit.iter().map(String::as_str).collect();
    let mut selected = Map::new();

    if fields.is_empty() {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys {
            if !omit.contains(key.as_str()) {
                selected.insert(key.clone(), map[key].clone());
            }
        }
        return selected;
    }

    for field in fields {
        if !omit.contains(field.as_str())
            && let Some(value) = map.get(field)
        {
            selected.insert(field.clone(), value.clone());
        }
    }
    selected
}

fn marker_for(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::String(format!("«array: {} items»", items.len())),
        Value::Object(map) => Value::String(format!("«object: {} fields»", map.len())),
        Value::String(text) => Value::String(format!("«string: {} chars»", text.chars().count())),
        _ => Value::String("«omitted»".to_string()),
    }
}

fn halve_to_zero(value: usize) -> usize {
    if value <= 1 { 0 } else { value / 2 }
}
