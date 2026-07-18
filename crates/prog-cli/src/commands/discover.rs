//! Source discovery command orchestration.

use crate::*;

pub(crate) async fn discover_source(store: &Store, args: &DiscoverArgs) -> Result<DiscoverReport> {
    if let Some(format) = args.import {
        return discover_from_import(store, args, format).await;
    }
    let seed = read_seed(&args.seed)?;
    discover_from_seed(store, &args.source_id, args.kind, seed, args.probe).await
}

async fn discover_from_import(
    store: &Store,
    args: &DiscoverArgs,
    format: ImportFormat,
) -> Result<DiscoverReport> {
    let raw = read_import_raw(&args.seed)?;
    let ctx = ImportContext {
        max_schema_depth: args.max_schema_depth,
        ..ImportContext::default()
    };
    let (profile, report, import_format) = import_profile_from_raw(args, format, &raw, &ctx)?;
    let expected = core_kind(args.kind);
    if profile.kind != expected {
        return Err(CoreError::BadArgs {
            operation: "discover --import".to_string(),
            reason: format!(
                "--kind {:?} does not match imported profile kind {:?}",
                expected, profile.kind
            ),
        });
    }
    let mut warnings = report.warnings.clone();
    warnings.extend(
        report
            .errors
            .iter()
            .map(|error| format!("import warning: {error}")),
    );
    if args.probe {
        warnings.push(
            "probe is skipped for imported profiles; import never executes upstream calls"
                .to_string(),
        );
    }
    let source_id = args.source_id.clone();
    let profile = store.update_profile(&source_id, |current| {
        merge_profiles(current, profile.clone())
    })?;
    Ok(DiscoverReport {
        schema: DISCLOSURE_SCHEMA,
        source_id,
        kind: profile.kind,
        profile_revision: profile.revision,
        operations_found: report.operations_imported,
        operations_probed: 0,
        shapes_learned: 0,
        import_format: Some(import_format.to_string()),
        schemas_imported: report.schemas_imported,
        examples_inferred: report.examples_inferred,
        warnings,
        effects_assumed: Vec::new(),
    })
}

pub(crate) async fn discover_from_seed(
    store: &Store,
    source_id: &str,
    kind: SourceKind,
    seed: Value,
    probe: bool,
) -> Result<DiscoverReport> {
    validate_seed_kind(kind, &seed)?;
    let mut prepared = prepare_discovery(source_id, kind, seed).await?;
    let operations_found = prepared.profile.operations.len();
    let mut operations_probed = 0usize;
    let mut shapes_learned = 0usize;

    if probe {
        let probe = prepared.probe.take();
        if let Some(probe) = &probe {
            probe_profile(
                &mut prepared.profile,
                probe,
                &mut prepared.warnings,
                &mut operations_probed,
                &mut shapes_learned,
            )
            .await;
        } else {
            prepared.warnings.push(
                "probe requested, but this seed cannot be executed by the V1 probe path"
                    .to_string(),
            );
        }
    }

    let profile = store.update_profile(source_id, |current| {
        merge_profiles(current, prepared.profile.clone())
    })?;

    Ok(DiscoverReport {
        schema: DISCLOSURE_SCHEMA,
        source_id: source_id.to_string(),
        kind: profile.kind,
        profile_revision: profile.revision,
        operations_found,
        operations_probed,
        shapes_learned,
        import_format: None,
        schemas_imported: 0,
        examples_inferred: 0,
        warnings: prepared.warnings,
        effects_assumed: prepared.effects_assumed,
    })
}
