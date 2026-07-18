//! Cost-report command entry point.

use crate::*;

pub(crate) fn cost_report(args: &CostArgs) -> Result<CostReport> {
    cost_report_impl(args)
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
