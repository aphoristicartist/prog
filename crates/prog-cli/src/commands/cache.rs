//! Cache-management command family.

use crate::*;

#[derive(Serialize)]
struct CacheGetOutput {
    entry: prog_core::CacheEntryMeta,
    projection: prog_core::Projection,
}

pub(crate) fn cache_command(
    store: &Store,
    command: &CacheCommand,
    pretty: bool,
    ctx: &mut InvocationContext,
) -> Result<ExitCode> {
    match command {
        CacheCommand::List => {
            write_success(&store.list_entries(100)?, pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        CacheCommand::Observations(args) => {
            write_success(&store.list_observations(args.limit)?, pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        CacheCommand::Get(args) => {
            let entry = store
                .get_entry(&args.key)?
                .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
            let payload = store
                .get_payload(&entry.payload_hash)?
                .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
            let scoped = ScopedSlice::root(SliceRequest {
                path: None,
                limit: None,
                depth: None,
                fields: Vec::new(),
                omit: Vec::new(),
                extra: serde_json::Map::new(),
            })?;
            let projection = expand(&payload, &scoped, &PreviewPolicy::default())?;
            write_success(&CacheGetOutput { entry, projection }, pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        CacheCommand::Purge(args) => {
            let selected = usize::from(args.all)
                + usize::from(args.expired)
                + usize::from(args.source.is_some())
                + usize::from(args.payload_budget_bytes.is_some());
            if selected != 1 {
                return Err(CoreError::BadArgs {
                    operation: "cache purge".to_string(),
                    reason: "pass exactly one of --all, --expired, --source <id>, or --payload-budget-bytes <bytes>".to_string(),
                });
            }
            let summary = if args.all {
                store.purge_all()?
            } else if args.expired {
                store.purge_expired(chrono::Utc::now())?
            } else if let Some(source) = &args.source {
                store.purge_source(source)?
            } else if let Some(max_payload_bytes) = args.payload_budget_bytes {
                write_success(
                    &store.enforce_payload_quota(max_payload_bytes)?,
                    pretty,
                    ctx,
                )?;
                return Ok(ExitCode::SUCCESS);
            } else {
                unreachable!("validated one cache purge selector")
            };
            write_success(&summary, pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        CacheCommand::Retention(args) => {
            let changes = usize::from(args.max_payload_bytes.is_some())
                + usize::from(args.max_age_seconds.is_some())
                + usize::from(args.clear_max_payload_bytes)
                + usize::from(args.clear_max_age_seconds);
            if changes == 0 {
                write_success(&store.storage_budget()?, pretty, ctx)?;
                return Ok(ExitCode::SUCCESS);
            }
            let mut budget = store.storage_budget()?;
            budget.source = BudgetSource::StorePolicy;
            if let Some(max_payload_bytes) = args.max_payload_bytes {
                budget.max_payload_bytes = Some(max_payload_bytes);
            } else if args.clear_max_payload_bytes {
                budget.max_payload_bytes = None;
            }
            if let Some(max_age_seconds) = args.max_age_seconds {
                budget.max_age_seconds = Some(max_age_seconds);
            } else if args.clear_max_age_seconds {
                budget.max_age_seconds = None;
            }
            let summary = store.set_storage_budget(&budget)?;
            ctx.set_storage(summary.budget.clone());
            write_success(&summary, pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
