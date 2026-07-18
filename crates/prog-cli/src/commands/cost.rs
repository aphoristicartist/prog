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
