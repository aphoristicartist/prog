//! Cursor-expansion command.

use crate::*;

pub(crate) fn expand_cursor(store: &Store, args: &ExpandArgs) -> Result<DisclosureEnvelope> {
    let record = store.get_cursor(&args.cursor)?;
    let entry = store
        .get_entry(&record.cache_key)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let observation = record
        .observation_id
        .as_deref()
        .map(|observation_id| store.get_observation(observation_id))
        .transpose()?
        .flatten();
    let payload = store
        .get_payload(&entry.payload_hash)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let slice = SliceRequest {
        path: if args.path.is_empty() {
            None
        } else {
            Some(args.path.clone())
        },
        limit: args.limit,
        depth: args.depth,
        fields: args.fields.clone(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let scoped = ScopedSlice::new(ExpansionScope::from_cursor(&record)?, slice.clone())?;
    let age = age_seconds(&entry.created_at)?;
    let cache = cache_info(CacheStatus::Hit, &entry, Some(age));
    let mut warnings = Vec::new();
    if cache_is_stale(Some(&cache)) {
        warnings.push(format!(
            "cached payload age_seconds={age}; re-run `prog call {} {} --refresh` to refresh",
            record.source_id, record.operation
        ));
    }

    if let Some(path) = &args.out {
        let (target_path, selected) = slice_value(&payload, &scoped)?;
        let bytes = serde_json::to_vec_pretty(&selected)?;
        write_private_file(path, &bytes)?;
        let evidence_ref = evidence_ref(EvidenceRefInput {
            source_id: &record.source_id,
            operation: &record.operation,
            cursor: Some(&args.cursor),
            path: &target_path,
            value: &selected,
            observation: observation.as_ref(),
            provenance: entry.provenance.as_ref(),
            cache: Some(&cache),
            omitted: &[],
            redacted_paths: 0,
        });
        let receipt = json!({
            "path": path,
            "json_pointer": target_path,
            "bytes": bytes.len(),
            "sha256": hex_sha256(&bytes),
            "evidence_ref": evidence_ref
        });
        let receipt = RawPayload::new(receipt)
            .redact(&RedactionPolicy::default())
            .payload;
        let mut envelope = envelope_for_payload(
            store,
            EnvelopeInput {
                value_scan: None,
                source_id: record.source_id.clone(),
                operation: record.operation.clone(),
                source_kind: source_kind_for_source_id(&record.source_id),
                payload: receipt,
                root_path: "".to_string(),
                slice: SliceRequest {
                    path: None,
                    limit: Some(5),
                    // The receipt contains a nested EvidenceRef. Keep the
                    // small receipt intact so its completeness describes the
                    // exported selection rather than a formatter omission.
                    depth: Some(8),
                    fields: Vec::new(),
                    omit: Vec::new(),
                    extra: Extra::new(),
                },
                payload_bytes: bytes.len().try_into().unwrap_or(u64::MAX),
                observation_id: entry.observation_id.clone(),
                provenance: entry.provenance.clone(),
                cache: Some(cache),
                effects: None,
                auto_upgrade_audit: None,
                redacted_paths: 0,
                cache_disabled_reason: None,
                warnings,
                schema_hints: BTreeMap::new(),
                next_action_operation: None,
                additional_next_actions: Vec::new(),
                observation_parser: None,
                lens: None,
            },
            None,
        )?;
        if let Some(observation) = envelope.observation.as_mut() {
            observation.completeness.root_path = target_path;
            observation.completeness.path_scoped = !observation.completeness.root_path.is_empty();
        }
        return Ok(envelope);
    }

    envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: None,
            source_id: record.source_id.clone(),
            operation: record.operation.clone(),
            source_kind: source_kind_for_source_id(&record.source_id),
            payload: payload.into_redacted(),
            root_path: record.root_path.clone(),
            slice,
            payload_bytes: entry.payload_bytes,
            observation_id: entry.observation_id.clone(),
            provenance: entry.provenance.clone(),
            cache: Some(cache),
            effects: None,
            auto_upgrade_audit: None,
            redacted_paths: 0,
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            observation_parser: None,
            lens: None,
        },
        Some(args.cursor.clone()),
    )
}
