//! Rendering shared by the generator and the documentation consistency check.
use serde_json::Value;

pub const COMPETITIVE_START: &str = "### Deterministic retrieval correctness\n";
pub const COMPETITIVE_END: &str = "### Correctness, not just savings\n";

pub fn competitive_readme_section(metrics: &Value) -> String {
    let metrics = metrics.as_array().expect("competitive metrics array");
    let mut report = String::from(
        "### Deterministic retrieval correctness\n\n\
         Known-path cases measure recovery at an explicitly supplied selector. The\n\
         unknown-target cases keep the grader's path and answer private: strategies\n\
         select evidence from their actual observations. The set includes a fatal\n\
         record, its relocated counterpart, an unranked cause, and a no-answer control.\n\n\
         | Unknown-target strategy | Evidence available / attempted | Unavailable | Approx. response tokens |\n\
         | --- | ---: | ---: | ---: |\n",
    );
    for strategy in [
        "raw_context",
        "head_tail_truncation",
        "native_field_selection",
        "rtk_grep_filter",
        "broad_log_search",
        "file_capture_search",
        "prog_retrieve",
    ] {
        let rows = metrics
            .iter()
            .filter(|row| {
                row["task_mode"] == "deterministic_discovery" && row["strategy"] == strategy
            })
            .collect::<Vec<_>>();
        assert!(
            !rows.is_empty(),
            "missing unknown-target rows for {strategy}"
        );
        let available = rows.iter().filter(|row| row["available"] == true).count();
        let evidence = rows
            .iter()
            .filter(|row| row["evidence_available"] == true)
            .count();
        let tokens = rows
            .iter()
            .map(|row| {
                row["input_tokens"]
                    .as_u64()
                    .expect("approximate token count")
            })
            .sum::<u64>();
        report.push_str(&format!(
            "| `{strategy}` | {evidence}/{available} | {} | {tokens} |\n",
            rows.len() - available
        ));
    }
    report.push_str(
        "\nThese are deterministic evidence-availability results, not actual-agent task\n\
         success. Raw context counts evidence present in the delivered artifact; a\n\
         strategy with insufficient evidence receives no discovery credit. Costs use\n\
         the bytes/4 approximation and include every capture, exploration, and lookup\n\
         response. Fixture setup and live source-acquisition costs are outside this\n\
         experiment. Broader search and a capture-once file baseline are included.\n\n\
         Known-path results, assumptions, and command traces are recorded in\n\
         [`docs/competitive-baselines.md`](docs/competitive-baselines.md).\n\n",
    );
    report
}

pub fn replace_competitive_readme(readme: &str, metrics: &Value) -> String {
    let start = readme
        .find(COMPETITIVE_START)
        .expect("README competitive section start");
    let end = readme[start..]
        .find(COMPETITIVE_END)
        .expect("README competitive section end")
        + start;
    format!(
        "{}{}{}",
        &readme[..start],
        competitive_readme_section(metrics),
        &readme[end..]
    )
}
