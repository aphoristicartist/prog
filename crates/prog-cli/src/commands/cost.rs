//! Cost-report command entry point.

use crate::*;

pub(crate) fn cost_report(args: &CostArgs) -> Result<CostReport> {
    cost_report_impl(args)
}
