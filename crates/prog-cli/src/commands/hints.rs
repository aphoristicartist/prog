//! Source-hint disclosure command.

use crate::*;

pub(crate) fn hints_source(
    store: &Store,
    args: &HintsArgs,
    ctx: &mut InvocationContext,
) -> Result<HintsResponse> {
    let profile = store
        .read_profile(&args.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(args.source_id.clone()))?;
    ctx.apply_profile_disclosure(&profile)?;
    let hints = build_hints_document(&profile, args.operation.as_deref())?;
    let redacted = RawPayload::new(hints).redact(&resolve_redaction(Some(&profile)));
    let payload = redacted.payload;
    let payload_hash = store.put_payload(&payload)?;
    let projection = project(payload.as_value(), &PreviewPolicy::default(), "");
    let cache_key = Store::cache_key(
        &args.source_id,
        "hints",
        &json!({"operation": args.operation}),
    )?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
        args.source_id.clone(),
        "hints".to_string(),
        serde_json::to_vec(payload.as_value())?
            .len()
            .try_into()
            .unwrap_or(u64::MAX),
        86_400,
    );
    let (availability, capture) = complete_capture(entry.payload_bytes, true, false);
    ctx.set_capture(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash,
        availability,
        capture,
        cache_key.clone(),
        args.source_id.clone(),
        "hints".to_string(),
        None,
        SelectionCoverage::default(),
        entry.provenance.clone(),
        Some(cache_key.clone()),
        false,
        Some(source_kind_provider(profile.kind)),
        None,
        None,
        None,
        // Discovery/schema-hint path: no source-revalidation signal is
        // computed here, so this stays the conservative default.
        prog_core::SourceValidity::Unknown,
    )?;
    entry.observation_id = Some(observation_id.clone());
    let cache_retained = store.put_entry(&cache_key, &entry)?;
    let cursor = if projection.omitted.is_empty() || !cache_retained {
        None
    } else {
        Some(store.create_cursor(&cache_key, &args.source_id, "hints", "", 86_400)?)
    };

    Ok(HintsResponse {
        schema: DISCLOSURE_SCHEMA,
        source_id: args.source_id.clone(),
        profile_revision: profile.revision,
        observation_id,
        hints: projection.preview,
        omitted: projection.omitted,
        cursor,
        warnings: Vec::new(),
    })
}
