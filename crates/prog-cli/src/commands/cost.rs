//! Cost-report command family.

use crate::*;

struct CostFlowEstimate {
    observe_tokens: u64,
    paths_tokens: u64,
    expansion_tokens: u64,
    warnings: Vec<String>,
}

pub(crate) struct CostScenarioInput {
    pub(crate) name: &'static str,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) baseline_input_tokens: u64,
    pub(crate) baseline_cost: f64,
    pub(crate) input_price: f64,
    pub(crate) output_price: f64,
    pub(crate) context_window_tokens: u64,
    pub(crate) lossless: bool,
    pub(crate) notes: Vec<String>,
}

pub(crate) fn cost_report(args: &CostArgs) -> Result<CostReport> {
    if args.repeated_inspections == 0 {
        return Err(CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: "--repeated-inspections must be at least 1".to_string(),
        });
    }
    let (profile, profile_warnings) = read_model_cost_profile(&args.model_profile)?;
    let input_price = profile
        .input_price_per_million_tokens
        .ok_or_else(|| CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: "model profile must include input_price_per_million_tokens".to_string(),
        })?;
    let output_price =
        profile
            .output_price_per_million_tokens
            .ok_or_else(|| CoreError::BadArgs {
                operation: "cost".to_string(),
                reason: "model profile must include output_price_per_million_tokens".to_string(),
            })?;
    validate_nonnegative_price(input_price, "input_price_per_million_tokens")?;
    validate_nonnegative_price(output_price, "output_price_per_million_tokens")?;
    if profile.context_window_tokens == 0 {
        return Err(CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: "model profile context_window_tokens must be greater than 0".to_string(),
        });
    }

    let raw = std::fs::read(&args.raw_file).map_err(|error| CoreError::BadArgs {
        operation: "cost".to_string(),
        reason: format!(
            "raw file '{}' could not be read: {error}",
            args.raw_file.to_string_lossy()
        ),
    })?;
    let mime = args
        .mime
        .clone()
        .unwrap_or_else(|| sniff_mime_from_bytes(&raw).to_string());
    let raw_bytes = raw.len().try_into().unwrap_or(u64::MAX);
    let raw_tokens = approx_tokens_for_bytes(raw_bytes);
    let flow = estimate_prog_cost_flow(&raw, &mime, &args.expand_paths)?;
    let output_tokens = args.estimated_output_tokens;
    let raw_single_cost = token_cost(raw_tokens, output_tokens, input_price, output_price);
    let truncation_tokens = raw_tokens.min(profile.context_window_tokens);
    let observe_tokens = flow.observe_tokens;
    let targeted_tokens = flow
        .observe_tokens
        .saturating_add(flow.paths_tokens)
        .saturating_add(flow.expansion_tokens);
    let repeated_input_tokens = flow.observe_tokens.saturating_add(
        args.repeated_inspections
            .saturating_mul(flow.paths_tokens.saturating_add(flow.expansion_tokens)),
    );
    let repeated_raw_tokens = raw_tokens.saturating_mul(args.repeated_inspections);
    let repeated_raw_cost = token_cost(
        repeated_raw_tokens,
        output_tokens.saturating_mul(args.repeated_inspections),
        input_price,
        output_price,
    );

    let scenarios = vec![
        cost_scenario(CostScenarioInput {
            name: "raw_payload",
            input_tokens: raw_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: true,
            notes: vec!["places the complete raw artifact in model context".to_string()],
        }),
        cost_scenario(CostScenarioInput {
            name: "simple_truncation",
            input_tokens: truncation_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: raw_tokens <= profile.context_window_tokens,
            notes: vec![
                "baseline for clipping to the model context window; may drop needed evidence"
                    .to_string(),
            ],
        }),
        cost_scenario(CostScenarioInput {
            name: "prog_observe_only",
            input_tokens: observe_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: false,
            notes: vec![
                "bounded first view only; full redacted artifact remains cursor-backed".to_string(),
            ],
        }),
        cost_scenario(CostScenarioInput {
            name: "prog_observe_paths_expand",
            input_tokens: targeted_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: !args.expand_paths.is_empty(),
            notes: vec![
                "bounded observation plus path listing and requested exact expansions".to_string(),
            ],
        }),
        cost_scenario(CostScenarioInput {
            name: "repeated_cache_hits",
            input_tokens: repeated_input_tokens,
            output_tokens: output_tokens.saturating_mul(args.repeated_inspections),
            baseline_input_tokens: repeated_raw_tokens,
            baseline_cost: repeated_raw_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: !args.expand_paths.is_empty(),
            notes: vec![format!(
                "models {} repeated inspections as one capture plus cached paths/expansions",
                args.repeated_inspections
            )],
        }),
    ];

    let mut warnings = vec![
        "model pricing is profile-driven; refresh model_profile pricing before making budget decisions".to_string(),
    ];
    warnings.extend(profile_warnings);
    warnings.extend(flow.warnings);
    if raw_tokens < 512 {
        warnings.push(
            "tiny payload counterexample: prog overhead may exceed raw context cost".to_string(),
        );
    }
    if output_tokens > targeted_tokens.max(1) {
        warnings.push(
            "estimated output tokens dominate this scenario; input-token savings may not control total cost".to_string(),
        );
    }
    if args.expand_paths.is_empty() {
        warnings.push(
            "no --expand-path was provided; targeted expansion scenario includes path discovery only".to_string(),
        );
    }

    Ok(CostReport {
        schema: "prog.cost_report",
        model: CostModelSummary {
            model: profile.model,
            input_price_per_million_tokens: input_price,
            output_price_per_million_tokens: output_price,
            context_window_tokens: profile.context_window_tokens,
            cache_read_price_per_million_tokens: profile.cache_read_price_per_million_tokens,
            cache_write_price_per_million_tokens: profile.cache_write_price_per_million_tokens,
            pricing_source: profile.pricing_source,
            priced_at: profile.priced_at,
        },
        input: CostInputSummary {
            raw_file: args.raw_file.to_string_lossy().to_string(),
            raw_bytes,
            raw_tokens,
            mime,
            expand_paths: args.expand_paths.clone(),
            estimated_output_tokens: output_tokens,
            repeated_inspections: args.repeated_inspections,
        },
        scenarios,
        warnings,
        counterexamples: vec![
            "tiny payloads can be cheaper to send raw".to_string(),
            "one expansion can reveal nearly the entire artifact".to_string(),
            "large expected model outputs can dominate total cost".to_string(),
            "low-cost local models may make latency more important than token spend".to_string(),
        ],
    })
}

fn estimate_prog_cost_flow(
    raw: &[u8],
    mime: &str,
    expand_paths: &[String],
) -> Result<CostFlowEstimate> {
    let normalized = normalize_observation(raw, mime)?;
    let redacted = RawPayload::new(normalized.payload).redact(&RedactionPolicy::default());
    let redacted_paths = redacted.redacted_paths;
    let redacted = redacted.payload;
    let redacted_bytes = canonical_json(redacted.as_value())?;
    let payload_bytes = redacted_bytes.len().try_into().unwrap_or(u64::MAX);
    let projection = project(redacted.as_value(), &PreviewPolicy::default(), "");
    let observe_envelope = json!({
        "schema": DISCLOSURE_SCHEMA,
        "source_id": "observe",
        "operation": "cost",
        "summary": {
            "kind": value_kind(redacted.as_value()),
            "payload_bytes": payload_bytes,
            "approx_tokens": approx_tokens_for_bytes(payload_bytes)
        },
        "data_preview": projection.preview,
        "omitted": projection.omitted,
        "cursor": "pc1_cost_example",
        "warnings": normalized.warnings,
        "redacted_paths": redacted_paths.len()
    });
    let observe_tokens = approx_tokens_for_json(&observe_envelope)?;

    let mut paths = Vec::new();
    let truncated = collect_paths(redacted.as_value(), "", 6, 200, &mut paths);
    let root_projection = project(redacted.as_value(), &PreviewPolicy::default(), "");
    annotate_path_omissions(&mut paths, &root_projection.omitted);
    append_missing_omitted_paths(&mut paths, &root_projection.omitted, 200);
    let paths_doc = json!({
        "schema": DISCLOSURE_SCHEMA,
        "cursor": "pc1_cost_example",
        "prefix": "",
        "paths": paths,
        "omitted": root_projection.omitted,
        "truncated": truncated
    });
    let paths_tokens = approx_tokens_for_json(&paths_doc)?;

    let mut expansion_tokens = 0u64;
    for path in expand_paths {
        let slice = SliceRequest {
            path: Some(path.clone()),
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        };
        let scoped = ScopedSlice::root(slice)?;
        let (target_path, selected) = slice_value(&redacted, &scoped)?;
        let expansion = project(&selected, &PreviewPolicy::default(), &target_path);
        let expansion_envelope = json!({
            "schema": DISCLOSURE_SCHEMA,
            "source_id": "observe",
            "operation": "cost",
            "data_preview": expansion.preview,
            "omitted": expansion.omitted,
            "cursor": "pc1_cost_example",
            "evidence_ref": {
                "schema": "prog.evidence_ref",
                "path": target_path
            }
        });
        expansion_tokens =
            expansion_tokens.saturating_add(approx_tokens_for_json(&expansion_envelope)?);
    }

    let mut warnings = Vec::new();
    if truncated {
        warnings.push("estimated path listing reached the default path limit".to_string());
    }
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "cost estimate uses redacted payload; {} sensitive path(s) were removed before estimates",
            redacted_paths.len()
        ));
    }

    Ok(CostFlowEstimate {
        observe_tokens,
        paths_tokens,
        expansion_tokens,
        warnings,
    })
}

pub(crate) fn cost_scenario(input: CostScenarioInput) -> CostScenario {
    let total = token_cost(
        input.input_tokens,
        input.output_tokens,
        input.input_price,
        input.output_price,
    );
    CostScenario {
        name: input.name,
        input_tokens: input.input_tokens,
        output_tokens: input.output_tokens,
        total_estimated_cost_usd: total,
        baseline_input_tokens: input.baseline_input_tokens,
        baseline_estimated_cost_usd: input.baseline_cost,
        savings_ratio: ratio(input.baseline_cost, total),
        fits_context: input.input_tokens <= input.context_window_tokens,
        lossless: input.lossless,
        notes: input.notes,
    }
}

pub(crate) fn read_model_cost_profile(path: &Path) -> Result<(ModelCostProfile, Vec<String>)> {
    let raw = std::fs::read_to_string(path).map_err(|error| CoreError::BadArgs {
        operation: "cost".to_string(),
        reason: format!(
            "model profile '{}' could not be read: {error}",
            path.to_string_lossy()
        ),
    })?;
    let profile: ModelCostProfile =
        serde_json::from_str(&raw).map_err(|error| CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: format!(
                "model profile '{}' must be valid JSON: {error}",
                path.to_string_lossy()
            ),
        })?;
    let mut warnings = Vec::new();
    if profile.schema.as_deref() != Some("prog.model_profile") {
        warnings.push("model profile schema should be prog.model_profile".to_string());
    }
    if profile.pricing_source.is_none() || profile.priced_at.is_none() {
        warnings
            .push("model profile should include pricing_source and priced_at metadata".to_string());
    }
    Ok((profile, warnings))
}

pub(crate) fn validate_nonnegative_price(price: f64, field: &str) -> Result<()> {
    if price.is_finite() && price >= 0.0 {
        Ok(())
    } else {
        Err(CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: format!("model profile {field} must be a non-negative finite number"),
        })
    }
}

pub(crate) fn approx_tokens_for_json(value: &Value) -> Result<u64> {
    let bytes = serde_json::to_vec(value)?
        .len()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(approx_tokens_for_bytes(bytes))
}

pub(crate) fn approx_tokens_for_bytes(bytes: u64) -> u64 {
    bytes.saturating_add(3) / 4
}

pub(crate) fn token_cost(
    input_tokens: u64,
    output_tokens: u64,
    input_price: f64,
    output_price: f64,
) -> f64 {
    round_usd(
        (input_tokens as f64 * input_price / 1_000_000.0)
            + (output_tokens as f64 * output_price / 1_000_000.0),
    )
}

pub(crate) fn ratio(baseline: f64, candidate: f64) -> f64 {
    if candidate <= 0.0 {
        return f64::INFINITY;
    }
    round_ratio(baseline / candidate)
}

fn round_usd(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn round_ratio(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
