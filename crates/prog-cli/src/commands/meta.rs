//! Public-contract metadata command.

use crate::*;

pub(crate) fn meta_contracts(
    store: &Store,
    args: &MetaArgs,
    ctx: &mut InvocationContext,
) -> Result<DisclosureEnvelope> {
    let schemas = public_contract_schemas()?;
    let payload = match &args.contract {
        Some(contract) => schemas
            .get(contract)
            .cloned()
            .ok_or_else(|| CoreError::BadArgs {
                operation: "meta".to_string(),
                reason: format!(
                    "unknown contract '{contract}'; expected one of {}",
                    schemas.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            })?,
        None => json!({
            "contracts": schemas.keys().cloned().collect::<Vec<_>>()
        }),
    };
    let operation = args.contract.as_deref().unwrap_or("contracts").to_string();
    let cache_key = Store::cache_key("prog", "meta", &json!({"contract": args.contract}))?;
    let redacted = RawPayload::new(payload).redact(&RedactionPolicy::default());
    let payload = redacted.payload;
    let payload_hash = store.put_payload(&payload)?;
    let payload_bytes = json_len_u64(payload.as_value())?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
        "prog".to_string(),
        operation.clone(),
        payload_bytes,
        86_400,
    );
    let (availability, capture) = complete_capture(payload_bytes, true, false);
    ctx.set_capture(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash,
        availability,
        capture,
        cache_key.clone(),
        "prog".to_string(),
        operation.clone(),
        None,
        SelectionCoverage::default(),
        entry.provenance.clone(),
        Some(cache_key.clone()),
        false,
        None,
        None,
        None,
        None,
    )?;
    entry.observation_id = Some(observation_id);
    let cache_retained = store.put_entry(&cache_key, &entry)?;
    let slice = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let scoped = ScopedSlice::root(slice.clone())?;
    let projection = expand(&payload, &scoped, &PreviewPolicy::default())?;
    let cursor = if projection.omitted.is_empty() || !cache_retained {
        None
    } else {
        Some(store.create_cursor(&cache_key, "prog", &operation, "", 86_400)?)
    };
    envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: None,
            source_id: "prog".to_string(),
            operation,
            source_kind: Some("internal".to_string()),
            payload,
            root_path: "".to_string(),
            slice,
            payload_bytes,
            observation_id: entry.observation_id.clone(),
            provenance: entry.provenance.clone(),
            cache: Some(if cache_retained {
                cache_info(CacheStatus::Stored, &entry, Some(0))
            } else {
                CacheInfo {
                    status: CacheStatus::Skipped,
                    ttl_seconds: None,
                    expires_at: None,
                    age_seconds: None,
                }
            }),
            effects: None,
            auto_upgrade_audit: None,
            redacted_paths: 0,
            cache_disabled_reason: None,
            warnings: Vec::new(),
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            observation_parser: None,
            lens: None,
        },
        cursor,
        ctx.max_envelope_bytes(),
    )
}
