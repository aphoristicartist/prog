//! Lens parsing, loading, and compatibility validation.

use crate::*;

pub(crate) fn parse_json_argument(raw: &str, operation: &str) -> Result<Value> {
    serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
        operation: operation.to_string(),
        reason: format!("must be valid JSON: {error}"),
    })
}

pub(crate) fn parse_view(raw: Option<&str>) -> Result<SliceRequest> {
    match raw {
        Some(raw) => serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: "call --view".to_string(),
            reason: format!("must be a SliceRequest JSON object: {error}"),
        }),
        None => Ok(SliceRequest {
            path: None,
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        }),
    }
}

pub(crate) fn load_lens(lens_dir: &Path, id: &str, context: &str) -> Result<LensManifest> {
    let manifests = load_lens_manifests(lens_dir, context)?;
    let mut matches = manifests
        .into_iter()
        .filter(|manifest| manifest.id == id)
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!("lens '{id}' not found in '{}'", lens_dir.to_string_lossy()),
        }),
        1 => Ok(matches.remove(0)),
        _ => Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{id}' is defined more than once in '{}'",
                lens_dir.to_string_lossy()
            ),
        }),
    }
}

fn load_lens_manifests(lens_dir: &Path, context: &str) -> Result<Vec<LensManifest>> {
    if !lens_dir.exists() {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens directory '{}' does not exist",
                lens_dir.to_string_lossy()
            ),
        });
    }

    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(lens_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || !is_lens_manifest_path(&path) {
            continue;
        }
        let bytes = std::fs::metadata(&path)?.len();
        if bytes > 1024 * 1024 {
            return Err(CoreError::BadArgs {
                operation: context.to_string(),
                reason: format!(
                    "lens '{}' is {bytes} bytes; manifests are limited to 1 MiB",
                    path.to_string_lossy()
                ),
            });
        }
        let raw = std::fs::read_to_string(&path).map_err(|error| CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!("could not read lens '{}': {error}", path.to_string_lossy()),
        })?;
        let manifest = parse_lens_manifest(&path, &raw, context)?;
        validate_lens_manifest(&manifest)?;
        manifests.push(manifest);
    }
    Ok(manifests)
}

fn is_lens_manifest_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("json" | "yaml" | "yml")
    )
}

fn parse_lens_manifest(path: &Path, raw: &str, context: &str) -> Result<LensManifest> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' must be valid JSON: {error}",
                path.to_string_lossy()
            ),
        }),
        Some("yaml" | "yml") => serde_yaml_ng::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' must be valid YAML: {error}",
                path.to_string_lossy()
            ),
        }),
        _ => Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' must use .json, .yaml, or .yml",
                path.to_string_lossy()
            ),
        }),
    }
}

pub(crate) fn validate_lens_matches_call(
    lens: &LensManifest,
    profile: &SourceProfile,
    operation: &OperationProfile,
) -> Result<()> {
    validate_lens_matches(
        lens,
        "call --lens",
        LensMatchSubject {
            actual_source_id: &profile.id,
            source_kind: Some(profile.kind),
            actual_operation: &operation.id,
            mime: None,
            artifact_kind: None,
        },
    )
}

pub(crate) fn validate_lens_matches_observe(
    lens: &LensManifest,
    input: &ObservationInput,
    normalized: &NormalizedObservation,
) -> Result<()> {
    validate_lens_matches(
        lens,
        "observe --lens",
        LensMatchSubject {
            actual_source_id: "observe",
            source_kind: None,
            actual_operation: &input.name,
            mime: Some(&input.mime),
            artifact_kind: Some(&normalized.kind),
        },
    )
}

pub(crate) fn validate_lens_matches_run(lens: &LensManifest, operation: &str) -> Result<()> {
    validate_lens_matches(
        lens,
        "run --lens",
        LensMatchSubject {
            actual_source_id: "run",
            source_kind: Some(prog_core::SourceKind::Cli),
            actual_operation: operation,
            mime: None,
            artifact_kind: Some("run"),
        },
    )
}

struct LensMatchSubject<'a> {
    actual_source_id: &'a str,
    source_kind: Option<prog_core::SourceKind>,
    actual_operation: &'a str,
    mime: Option<&'a str>,
    artifact_kind: Option<&'a str>,
}

fn validate_lens_matches(
    lens: &LensManifest,
    context: &str,
    subject: LensMatchSubject<'_>,
) -> Result<()> {
    if let Some(source_id) = &lens.match_rules.source_id
        && source_id != subject.actual_source_id
    {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' matches source_id '{}', not '{}'",
                lens.id, source_id, subject.actual_source_id
            ),
        });
    }
    if let Some(source_kind) = lens.match_rules.source_kind {
        match subject.source_kind {
            Some(actual_source_kind) if source_kind == actual_source_kind => {}
            Some(actual_source_kind) => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches source_kind '{:?}', not '{:?}'",
                        lens.id, source_kind, actual_source_kind
                    ),
                });
            }
            None => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches source_kind '{:?}', but this artifact has no source_kind",
                        lens.id, source_kind
                    ),
                });
            }
        }
    }
    if let Some(expected_operation) = &lens.match_rules.operation
        && expected_operation != subject.actual_operation
    {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' matches operation '{}', not '{}'",
                lens.id, expected_operation, subject.actual_operation
            ),
        });
    }
    if let Some(expected_mime) = &lens.match_rules.mime {
        match subject.mime {
            Some(actual_mime) if expected_mime.eq_ignore_ascii_case(actual_mime) => {}
            Some(actual_mime) => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches mime '{}', not '{}'",
                        lens.id, expected_mime, actual_mime
                    ),
                });
            }
            None => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches mime '{}', but this artifact has no mime",
                        lens.id, expected_mime
                    ),
                });
            }
        }
    }
    if let Some(expected_artifact_kind) = &lens.match_rules.artifact_kind {
        match subject.artifact_kind {
            Some(actual_artifact_kind) if expected_artifact_kind == actual_artifact_kind => {}
            Some(actual_artifact_kind) => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches artifact_kind '{}', not '{}'",
                        lens.id, expected_artifact_kind, actual_artifact_kind
                    ),
                });
            }
            None => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches artifact_kind '{}', but this artifact has no artifact_kind",
                        lens.id, expected_artifact_kind
                    ),
                });
            }
        }
    }
    Ok(())
}
