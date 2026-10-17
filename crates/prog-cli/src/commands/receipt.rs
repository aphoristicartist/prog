//! Compact verdict receipts over recorded observations (#288).

use std::fs;

use crate::*;
use prog_core::{
    ObservationRecord, RECEIPT_SCHEMA, Receipt, ReceiptBaseline, ReceiptList, ReceiptSubject,
    ReceiptVerdict, build_receipt,
};

/// Newest observations a single `--since`/`--pending` receipt request covers.
const MAX_RECEIPTS: usize = 5;
const RECEIPT_CURSOR_TTL_SECONDS: i64 = 86_400;

pub(crate) fn receipt_report(store: &Store, args: &ReceiptArgs) -> Result<ReceiptList> {
    let targets = if args.pending {
        pending_targets(store)?
    } else {
        receipt_targets(store, args)?
    };
    let receipts = targets
        .iter()
        .map(|observation| receipt_for(store, observation))
        .collect::<Result<Vec<_>>>()?;
    let text = receipts
        .iter()
        .map(|receipt| receipt.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ReceiptList {
        schema: RECEIPT_SCHEMA.to_string(),
        receipts,
        text,
        extra: Extra::new(),
    })
}

fn receipt_targets(store: &Store, args: &ReceiptArgs) -> Result<Vec<ObservationRecord>> {
    if let Some(id) = &args.observation {
        let observation = store
            .get_observation(id)?
            .ok_or_else(|| CoreError::BadArgs {
                operation: "receipt".to_string(),
                reason: format!("unknown observation '{id}'"),
            })?;
        return Ok(vec![observation]);
    }
    if let Some(since) = &args.since {
        let since =
            chrono::DateTime::parse_from_rfc3339(since).map_err(|error| CoreError::BadArgs {
                operation: "receipt".to_string(),
                reason: format!("--since must be an RFC 3339 timestamp: {error}"),
            })?;
        let mut targets = store
            .list_observations(100)?
            .observations
            .into_iter()
            .filter(|observation| {
                chrono::DateTime::parse_from_rfc3339(&observation.captured_at)
                    .is_ok_and(|captured| captured >= since)
            })
            .take(MAX_RECEIPTS)
            .collect::<Vec<_>>();
        targets.reverse();
        return Ok(targets);
    }
    Ok(store.latest_observation()?.into_iter().collect())
}

/// Shim captures whose receipt has not been read. Each marker is consumed as
/// it is read, so concurrent hooks do not repeat a receipt indefinitely.
/// Markers older than the newest `MAX_RECEIPTS` are consumed without a
/// receipt, so they never surface later as if they were new.
fn pending_targets(store: &Store) -> Result<Vec<ObservationRecord>> {
    let pending = store
        .dir()
        .join(crate::commands::shim::PENDING_RECEIPTS_DIR);
    let mut markers = match fs::read_dir(&pending) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
                Some((modified, entry.path()))
            })
            .collect::<Vec<_>>(),
        Err(_) => return Ok(Vec::new()),
    };
    markers.sort();
    let first_reported = markers.len().saturating_sub(MAX_RECEIPTS);
    let mut targets = Vec::new();
    for (index, (_, marker)) in markers.into_iter().enumerate() {
        let id = marker
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string);
        if fs::remove_file(&marker).is_err() || index < first_reported {
            continue;
        }
        if let Some(observation) = id
            .as_deref()
            .map(|id| store.get_observation(id))
            .transpose()?
            .flatten()
        {
            targets.push(observation);
        }
    }
    Ok(targets)
}

fn receipt_for(store: &Store, observation: &ObservationRecord) -> Result<Receipt> {
    let Some(payload) = store.get_payload(&observation.payload_hash)? else {
        return Ok(Receipt {
            observation_id: observation.observation_id.clone(),
            cursor: None,
            compared_with: None,
            verdict: ReceiptVerdict::NotProven,
            text: format!(
                "prog: evidence for {} is no longer retained; rerun to observe again",
                observation.observation_id
            ),
            extra: Extra::new(),
        });
    };
    let cursor = receipt_cursor(store, observation)?;
    // The store links each capture to the previous capture of the same
    // canonical invocation; that is the only automatic comparison partner.
    let previous = match &observation.lineage.supersedes_id {
        Some(id) => store.get_observation(id)?,
        None => None,
    };
    let previous = match previous {
        Some(previous) => match store.get_payload(&previous.payload_hash)? {
            Some(previous_payload) => {
                let delta = compare_observation_ids(
                    store,
                    &previous.observation_id,
                    &observation.observation_id,
                )?;
                Some((previous, previous_payload, delta))
            }
            None => None,
        },
        None => None,
    };
    Ok(build_receipt(
        ReceiptSubject {
            observation,
            payload: payload.as_value(),
            cursor: cursor.as_deref(),
        },
        previous
            .as_ref()
            .map(|(previous, previous_payload, delta)| ReceiptBaseline {
                observation: previous,
                payload: previous_payload.as_value(),
                delta,
            }),
    ))
}

/// Mint a cursor only when the observation still owns its cache entry, so the
/// cursor can never resolve to a newer payload than the one summarized.
fn receipt_cursor(store: &Store, observation: &ObservationRecord) -> Result<Option<String>> {
    let Some(cache_key) = &observation.cache_key else {
        return Ok(None);
    };
    let owns_entry = store
        .get_entry(cache_key)?
        .is_some_and(|entry| entry.observation_id.as_deref() == Some(&observation.observation_id));
    if !owns_entry {
        return Ok(None);
    }
    store
        .create_cursor(
            cache_key,
            &observation.source_id,
            &observation.operation,
            "",
            RECEIPT_CURSOR_TTL_SECONDS,
        )
        .map(Some)
}
