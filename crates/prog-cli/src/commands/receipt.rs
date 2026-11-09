//! Compact verdict receipts over recorded observations (#288).

use std::fs;

use crate::*;
use prog_core::{
    ObservationDelta, ObservationRecord, PersistedPayload, RECEIPT_SCHEMA, Receipt,
    ReceiptBaseline, ReceiptList, ReceiptSubject, ReceiptVerdict, build_receipt,
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
    let baseline = comparison_baseline(store, observation)?;
    Ok(build_receipt(
        ReceiptSubject {
            observation,
            payload: payload.as_value(),
            cursor: cursor.as_deref(),
        },
        baseline.as_ref().map(Baseline::as_receipt_baseline),
    ))
}

/// What a run is compared with in its receipt and in the stop gate.
pub(crate) struct Baseline {
    observation: ObservationRecord,
    payload: PersistedPayload,
    delta: ObservationDelta,
    earlier_failure: bool,
}

impl Baseline {
    pub(crate) fn as_receipt_baseline(&self) -> ReceiptBaseline<'_> {
        ReceiptBaseline {
            observation: &self.observation,
            payload: self.payload.as_value(),
            delta: &self.delta,
            earlier_failure: self.earlier_failure,
        }
    }
}

/// How far back a chain is searched for a failure no run has proven fixed.
const MAX_UNRESOLVED_HOPS: usize = 8;

/// The store links each capture to the previous capture of the same canonical
/// invocation; that is the only automatic comparison partner. When that
/// previous run passed without proving an earlier failure fixed (`NOT
/// PROVEN`), the comparison partner is that earlier failure instead, so an
/// identical rerun cannot turn an unproven fix into a clean result.
pub(crate) fn comparison_baseline(
    store: &Store,
    observation: &ObservationRecord,
) -> Result<Option<Baseline>> {
    let Some((previous, previous_payload)) = previous_run(store, observation)? else {
        return Ok(None);
    };
    let (baseline, payload, earlier_failure) =
        if prog_core::payload_reports_failure(previous_payload.as_value()) {
            (previous, previous_payload, false)
        } else {
            match unresolved_failure(store, &previous, MAX_UNRESOLVED_HOPS)? {
                Some((failure, failure_payload)) => (failure, failure_payload, true),
                None => (previous, previous_payload, false),
            }
        };
    let delta =
        compare_observation_ids(store, &baseline.observation_id, &observation.observation_id)?;
    Ok(Some(Baseline {
        observation: baseline,
        payload,
        delta,
        earlier_failure,
    }))
}

fn previous_run(
    store: &Store,
    observation: &ObservationRecord,
) -> Result<Option<(ObservationRecord, PersistedPayload)>> {
    let Some(id) = &observation.lineage.supersedes_id else {
        return Ok(None);
    };
    let Some(previous) = store.get_observation(id)? else {
        return Ok(None);
    };
    Ok(store
        .get_payload(&previous.payload_hash)?
        .map(|payload| (previous, payload)))
}

/// The failing run that the passing `run` was compared with, when that
/// comparison could not prove the failure fixed.
fn unresolved_failure(
    store: &Store,
    run: &ObservationRecord,
    hops: usize,
) -> Result<Option<(ObservationRecord, PersistedPayload)>> {
    if hops == 0 {
        return Ok(None);
    }
    let Some((previous, previous_payload)) = previous_run(store, run)? else {
        return Ok(None);
    };
    let failure = if prog_core::payload_reports_failure(previous_payload.as_value()) {
        Some((previous, previous_payload))
    } else {
        unresolved_failure(store, &previous, hops - 1)?
    };
    let Some((failure, failure_payload)) = failure else {
        return Ok(None);
    };
    let proven = compare_observation_ids(store, &failure.observation_id, &run.observation_id)?
        .assessment
        .can_prove_absence;
    Ok((!proven).then_some((failure, failure_payload)))
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
