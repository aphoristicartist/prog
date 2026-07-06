use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::{
    CoreError, ExpandablePayload, ExpansionScope, Extra, LENS_MANIFEST_VERSION, LensManifest,
    LensOmission, NextAction, OmittedRegion, PreviewPolicy, Projection, Result, ScopedSlice,
    SliceRequest,
    disclosure::{expand, project},
    pointer::{get, is_within, parse},
};

#[derive(Debug, Clone, PartialEq)]
pub struct LensProjection {
    pub projection: Projection,
    pub next_actions: Vec<NextAction>,
}

pub fn validate_lens_manifest(manifest: &LensManifest) -> Result<()> {
    if manifest.schema_version != LENS_MANIFEST_VERSION {
        return Err(lens_error(
            manifest,
            format!(
                "schema_version must be '{LENS_MANIFEST_VERSION}', got '{}'",
                manifest.schema_version
            ),
        ));
    }
    if manifest.id.trim().is_empty() {
        return Err(lens_error(manifest, "id must not be empty"));
    }
    if manifest.version == 0 {
        return Err(lens_error(manifest, "version must be greater than zero"));
    }
    if let Some(root) = &manifest.view.root {
        validate_pointer(root, false, manifest, "view.root")?;
    }
    for (name, selector) in &manifest.view.fields {
        if name.trim().is_empty() {
            return Err(lens_error(
                manifest,
                "view.fields keys must not be empty strings",
            ));
        }
        validate_pointer(selector, true, manifest, &format!("view.fields.{name}"))?;
    }
    for omission in &manifest.omit {
        validate_omission(manifest, omission)?;
    }
    for action in &manifest.next_actions {
        if let Some(path) = &action.path
            && !path.contains('{')
        {
            validate_pointer(path, true, manifest, "next_actions[].path")?;
        }
    }
    Ok(())
}

pub fn lens_slice_request(
    manifest: &LensManifest,
    fallback: &SliceRequest,
) -> Result<SliceRequest> {
    validate_lens_manifest(manifest)?;
    Ok(SliceRequest {
        path: manifest.view.root.clone().or_else(|| fallback.path.clone()),
        limit: manifest.view.limit.or(fallback.limit),
        depth: manifest.view.depth.or(fallback.depth),
        fields: if manifest.view.fields.is_empty() {
            fallback.fields.clone()
        } else {
            Vec::new()
        },
        omit: fallback.omit.clone(),
        extra: Extra::new(),
    })
}

pub fn project_with_lens(
    payload: &impl ExpandablePayload,
    root_path: &str,
    slice: &SliceRequest,
    policy: &PreviewPolicy,
    manifest: Option<&LensManifest>,
) -> Result<LensProjection> {
    let Some(manifest) = manifest else {
        let scoped = ScopedSlice::new(ExpansionScope::new(root_path)?, slice.clone())?;
        return Ok(LensProjection {
            projection: expand(payload, &scoped, policy)?,
            next_actions: Vec::new(),
        });
    };

    validate_lens_manifest(manifest)?;
    let mut effective_policy = policy.with_limit_and_depth(slice.limit, slice.depth);
    if let Some(limit) = manifest.view.limit {
        effective_policy.array_items = limit;
    }
    if let Some(depth) = manifest.view.depth {
        effective_policy.depth = depth;
    }

    let projection = if manifest.view.fields.is_empty() {
        let scoped = ScopedSlice::new(ExpansionScope::new(root_path)?, slice.clone())?;
        expand(payload, &scoped, &effective_policy)?
    } else {
        let value = payload.expansion_value();
        let target = get(value, root_path)?.ok_or_else(|| CoreError::PathNotFound {
            path: root_path.to_string(),
            hint: crate::pointer::siblings_hint(value, root_path),
        })?;
        let selected = select_fields_with_pointers(target, &manifest.view.fields)?;
        project(&selected, &effective_policy, root_path)
    };

    let mut omitted = projection.omitted;
    omitted.extend(manifest_omissions(manifest));
    dedupe_omitted(&mut omitted);

    Ok(LensProjection {
        projection: Projection {
            preview: projection.preview,
            omitted,
        },
        next_actions: manifest.next_actions.clone(),
    })
}

fn validate_pointer(
    pointer: &str,
    allow_wildcards: bool,
    manifest: &LensManifest,
    field: &str,
) -> Result<()> {
    let segments =
        parse(pointer).map_err(|error| lens_error(manifest, format!("{field}: {error}")))?;
    if !allow_wildcards && segments.iter().any(|segment| segment == "*") {
        return Err(lens_error(
            manifest,
            format!("{field}: wildcards are not allowed here"),
        ));
    }
    Ok(())
}

fn validate_omission(manifest: &LensManifest, omission: &LensOmission) -> Result<()> {
    validate_pointer(&omission.path, true, manifest, "omit[].path")?;
    if let Some(root) = &manifest.view.root
        && !omission.path.contains('*')
        && !is_within(root, &omission.path)?
    {
        return Err(lens_error(
            manifest,
            format!(
                "omit path '{}' is outside view.root '{}'",
                omission.path, root
            ),
        ));
    }
    Ok(())
}

fn select_fields_with_pointers(value: &Value, fields: &BTreeMap<String, String>) -> Result<Value> {
    match value {
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| select_object_fields_with_pointers(item, fields))
                .collect::<Result<Vec<_>>>()?,
        )),
        _ => select_object_fields_with_pointers(value, fields),
    }
}

fn select_object_fields_with_pointers(
    value: &Value,
    fields: &BTreeMap<String, String>,
) -> Result<Value> {
    let mut selected = Map::new();
    for (name, selector) in fields {
        if let Some(field_value) = select_pointer_with_wildcards(value, selector)? {
            selected.insert(name.clone(), field_value);
        }
    }
    Ok(Value::Object(selected))
}

fn select_pointer_with_wildcards(value: &Value, pointer: &str) -> Result<Option<Value>> {
    let segments = parse(pointer)?;
    select_segments(value, &segments)
}

fn select_segments(value: &Value, segments: &[String]) -> Result<Option<Value>> {
    let Some((head, tail)) = segments.split_first() else {
        return Ok(Some(value.clone()));
    };

    if head == "*" {
        let mut selected = Vec::new();
        match value {
            Value::Array(items) => {
                for item in items {
                    if let Some(value) = select_segments(item, tail)? {
                        selected.push(value);
                    }
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    if let Some(value) = select_segments(item, tail)? {
                        selected.push(value);
                    }
                }
            }
            _ => return Ok(None),
        }
        return Ok(Some(Value::Array(selected)));
    }

    match value {
        Value::Object(map) => match map.get(head) {
            Some(next) => select_segments(next, tail),
            None => Ok(None),
        },
        Value::Array(items) => match head
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index))
        {
            Some(next) => select_segments(next, tail),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

fn manifest_omissions(manifest: &LensManifest) -> Vec<OmittedRegion> {
    manifest
        .omit
        .iter()
        .map(|omission| {
            let mut extra = omission.extra.clone();
            extra.insert("expandable".to_string(), json!(omission.expandable));
            OmittedRegion {
                path: omission.path.clone(),
                reason: omission.reason,
                detail: omission.detail.clone(),
                extra,
            }
        })
        .collect()
}

fn dedupe_omitted(omitted: &mut Vec<OmittedRegion>) {
    let mut seen = std::collections::BTreeSet::new();
    omitted.retain(|entry| seen.insert((entry.path.clone(), entry.reason)));
}

fn lens_error(manifest: &LensManifest, reason: impl Into<String>) -> CoreError {
    CoreError::BadArgs {
        operation: format!("lens {}", manifest.id),
        reason: reason.into(),
    }
}
