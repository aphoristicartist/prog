//! Rendering shared by the generator and the documentation consistency check.
// Compiled independently by several integration-test crates.
#![allow(dead_code)]

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
            .map(|row| approx_tokens(number(row, "response_bytes")))
            .sum::<u64>();
        report.push_str(&format!(
            "| `{strategy}` | {evidence}/{available} | {} | {} |\n",
            rows.len() - available,
            thousands(tokens)
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
         [`docs/competitive-baselines.md`](docs/competitive-baselines.md) and the\n\
         [measurement rows](fixtures/evals/competitive-baseline-metrics.json).\n\n",
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

pub const ARTIFACTS: [&str; 5] = [
    "fixtures/evals/token-economics-metrics.json",
    "fixtures/evals/evidence-acquisition-metrics.json",
    "fixtures/evals/real-world-demo-metrics.json",
    "fixtures/evals/competitive-baseline-metrics.json",
    "fixtures/evals/task-success-metrics.json",
];
pub const DOCUMENTS: [&str; 6] = [
    "README.md",
    "docs/token-economics.md",
    "docs/evidence-acquisition.md",
    "docs/real-world-demos.md",
    "docs/competitive-baselines.md",
    "docs/task-success-eval.md",
];

fn number(row: &Value, key: &str) -> u64 {
    row[key]
        .as_u64()
        .unwrap_or_else(|| panic!("missing nonnegative metric {key}"))
}
fn label<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key]
        .as_str()
        .unwrap_or_else(|| panic!("missing metric label {key}"))
}
fn rows(value: &Value) -> &[Value] {
    value.as_array().expect("metric rows array")
}
fn sum(values: &[&Value], key: &str) -> u64 {
    values.iter().map(|row| number(row, key)).sum()
}
pub fn approx_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}
fn token_ratio(raw: u64, delivered: u64) -> f64 {
    assert!(delivered > 0, "a ratio requires a nonzero delivered cost");
    approx_tokens(raw) as f64 / approx_tokens(delivered) as f64
}
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    digits
        .chars()
        .enumerate()
        .fold(String::new(), |mut result, (index, digit)| {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                result.push(',');
            }
            result.push(digit);
            result
        })
}
fn range(values: impl Iterator<Item = f64>, precision: usize) -> (String, String) {
    let values = values.collect::<Vec<_>>();
    assert!(!values.is_empty() && values.iter().all(|v| v.is_finite()));
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (format!("{low:.precision$}"), format!("{high:.precision$}"))
}

/// Boundaries are explicit and unique. Everything outside them is preserved.
pub fn replace_block(document: &str, name: &str, body: &str) -> String {
    let start_marker = format!("<!-- eval:{name}:start -->");
    let end_marker = format!("<!-- eval:{name}:end -->");
    assert_eq!(
        document.matches(&start_marker).count(),
        1,
        "unique start boundary for {name}"
    );
    assert_eq!(
        document.matches(&end_marker).count(),
        1,
        "unique end boundary for {name}"
    );
    let start = document.find(&start_marker).unwrap() + start_marker.len();
    let end = document.find(&end_marker).unwrap();
    assert!(start <= end, "ordered boundaries for {name}");
    format!(
        "{}\n{}\n{}",
        &document[..start],
        body.trim(),
        &document[end..]
    )
}

pub fn token_report(source: &Value) -> String {
    assert_eq!(source["schema"], "prog.token_economics_eval");
    assert_eq!(source["token_estimator"], "bytes_div_4_approximate");
    let mut report = String::from(
        "# Token economics eval\n\n\
         Token counts use the project heuristic `bytes / 4`, rounded up. Raw cost is the full fixture payload entering context. prog cost is the sum of every bounded envelope or expansion stdout consumed for the task, including the initial call envelope before any expansion. This is not a latency benchmark or a model-success benchmark.\n\n\
         Every `DisclosureEnvelope` reports a `disclosure_verdict`. When original input cost is known, its ratio is `baseline.bytes / envelope_bytes`: below `1.0` is `raw_cheaper`, from `1.0` through less than `1.25` is `neutral`, and `1.25` or above is `bounded_win` (the envelope is at least 20 percent smaller). The displayed ratio is rounded down to hundredths (or a conservative lower value when its encoded width cycles), but classification uses exact integer byte counts. `baseline.basis` identifies original command streams counted once, file/stdin bytes, or decoded HTTP body bytes. Unknown original costs (including MCP SDK-normalized content and incomplete HTTP bodies) report `unavailable` with null baseline and ratio. `summary.payload_bytes` is normalized, redacted storage size and is never the source comparison baseline. `summary.envelope_bytes`, verdict bytes, and disclosure-budget actual bytes all count final stdout including budget metadata, formatting, and its trailing newline. The verdict reports cost; it does not automatically replace the envelope with raw output.\n\n\
         Source: [`token-economics-metrics.json`](../fixtures/evals/token-economics-metrics.json). Refresh measurements with `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`; render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write` or check them with `--check`. Runtime cost gates remain separate from documentation consistency.\n\n\
         | Fixture | Task | Raw tokens | prog tokens | Ratio |\n\
         |---|---:|---:|---:|---:|\n",
    );
    for row in rows(&source["rows"]) {
        let raw = number(row, "raw_bytes");
        let delivered = number(row, "prog_bytes");
        report.push_str(&format!(
            "| {} | {} | {} | {} | {:.1}x |\n",
            label(row, "fixture"),
            label(row, "task"),
            approx_tokens(raw),
            approx_tokens(delivered),
            token_ratio(raw, delivered)
        ));
    }
    report
}

fn correct_top_path(row: &Value) -> bool {
    row["correct"] == true
        && row["top_finding_rank"] == 1
        && row["top_finding_path"].is_string()
        && row["top_finding_path"] == row["expected_path"]
}

fn evidence_totals(source: &Value) -> (usize, usize, u64, u64, u64, u64) {
    assert_eq!(source["schema"], "prog.evidence_acquisition_eval");
    let scenarios = rows(&source["scenarios"]);
    let correct = scenarios.iter().filter(|row| correct_top_path(row)).count();
    let scenarios = scenarios.iter().collect::<Vec<_>>();
    (
        scenarios.len(),
        correct,
        sum(&scenarios, "findings_tool_calls"),
        sum(&scenarios, "baseline_tool_calls"),
        sum(&scenarios, "findings_output_tokens"),
        sum(&scenarios, "baseline_output_tokens"),
    )
}
fn evidence_table(source: &Value) -> String {
    let (count, correct, ..) = evidence_totals(source);
    let mut report = format!(
        "## Recorded measurements\n\n{correct}/{count} scenarios retain the expected top-ranked path. Output tokens are\napproximate bytes/4 counts of core structures, with modeled workflow calls;\nthese are not complete CLI stdout or acquisition costs. Source: [`evidence-acquisition-metrics.json`](../fixtures/evals/evidence-acquisition-metrics.json).\n\n| Scenario | Top rank | Correct path | Baseline calls | Findings calls | Baseline output tokens | Findings output tokens | Inspect output tokens |\n|---|---:|---|---:|---:|---:|---:|---:|\n"
    );
    for row in rows(&source["scenarios"]) {
        report.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            label(row, "name"),
            number(row, "top_finding_rank"),
            correct_top_path(row),
            number(row, "baseline_tool_calls"),
            number(row, "findings_tool_calls"),
            number(row, "baseline_output_tokens"),
            number(row, "findings_output_tokens"),
            number(row, "inspect_output_tokens")
        ));
    }
    report
}
fn demo_table(source: &Value) -> String {
    let mut report = String::from(
        "Source: [`real-world-demo-metrics.json`](../fixtures/evals/real-world-demo-metrics.json). Render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write`; `--check` reads saved measurements without rerunning demos. Ratios use approximate bytes/4 counts rounded up.\n\n\
         | Demo | Raw bytes | call envelope bytes | expansion task bytes | cache hit | Token ratio |\n|---|---:|---:|---:|---|---:|\n",
    );
    for row in rows(source) {
        report.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2}x |\n",
            label(row, "id"),
            number(row, "raw_payload_bytes"),
            number(row, "call_envelope_bytes"),
            number(row, "expansion_task_bytes"),
            label(row, "cache_hit_status"),
            token_ratio(
                number(row, "raw_payload_bytes"),
                number(row, "expansion_task_bytes")
            )
        ));
    }
    report
}

pub fn competitive_report(metrics: &Value) -> String {
    let mut output = String::from(
        "# Competitive baselines\n\n\
         This is a deterministic evidence-availability experiment, not actual-agent task success. Known-path tasks publish their selector; unknown-target strategies receive only the task and artifact. The grader's path and answer are consulted only after execution. No strategy receives answer-derived grep terms.\n\n\
         All arms start from the same fixture artifact outside model context. Source-profile discovery/setup is excluded. Raw context discloses that artifact; filters transform it; file capture writes it once and returns a receipt before searching; prog returns a capture envelope before retrieval. Every strategy response (including exploration, receipt, retrieval, and repeated retrieval) contributes to response bytes and tool-call counts. These are disclosure costs, not end-to-end live acquisition or latency comparisons. Python implements the declared JSON/line filters with actual argv and stdout; it is not the RTK or jq executable.\n\n\
         Broad search uses the declared case-insensitive severity pattern `fatal|panic|error|failed|exception` with word boundaries. The narrow unknown-target grep assumes `ERROR`. File search uses the public known-task term or that broad pattern. Minified JSON can make line search return the entire artifact.\n\n\
         Unknown-target prog retrieval expands the first finding returned in the initial envelope, or calls `inspect --goal root_cause` if none is returned. With no candidate it stops; an incorrect candidate has no oracle fallback. Repeated retrieval uses the same observed path. Known-path retrieval is explicitly assisted recoverability.\n\n\
         Token counts approximate response bytes/4 rounded up. No model answers are generated: the separate assumed-answer tokens and illustrative Fable prices in JSON are hypothetical, never provider usage. Unavailable arms are excluded from attempted counts; no-answer and missed/unranked evidence remain insufficient rather than being counted as discovery successes.\n\n\
         Source: [`competitive-baseline-metrics.json`](../fixtures/evals/competitive-baseline-metrics.json). Refresh measurements with `PROG_BASELINE_EVAL_UPDATE=1 cargo test -p prog-cli --test competitive_baselines -- --nocapture`. Render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write`; `--check` detects documentation drift without running measurements.\n\n",
    );
    for mode in ["known_path_recoverability", "deterministic_discovery"] {
        output.push_str(&format!("## {mode}\n\n| Strategy | Evidence available | Attempted | Unavailable | Response bytes | Approx. input tokens | Tool calls |\n|---|---:|---:|---:|---:|---:|---:|\n"));
        let strategies = rows(metrics)
            .iter()
            .filter(|row| row["task_mode"] == mode)
            .map(|row| label(row, "strategy"))
            .collect::<std::collections::BTreeSet<_>>();
        for strategy in strategies {
            let selected = rows(metrics)
                .iter()
                .filter(|row| row["task_mode"] == mode && row["strategy"] == strategy)
                .collect::<Vec<_>>();
            output.push_str(&format!(
                "| {strategy} | {} | {} | {} | {} | {} | {} |\n",
                selected
                    .iter()
                    .filter(|row| row["evidence_available"] == true)
                    .count(),
                selected
                    .iter()
                    .filter(|row| row["available"] == true)
                    .count(),
                selected
                    .iter()
                    .filter(|row| row["available"] == false)
                    .count(),
                sum(&selected, "response_bytes"),
                selected
                    .iter()
                    .map(|row| approx_tokens(number(row, "response_bytes")))
                    .sum::<u64>(),
                sum(&selected, "tool_calls")
            ));
        }
        output.push('\n');
    }
    output.push_str("## Unknown-target outcomes\n\n| Scenario | Strategy | Outcome | Selected path | Response bytes |\n|---|---|---|---|---:|\n");
    for row in rows(metrics)
        .iter()
        .filter(|row| row["task_mode"] == "deterministic_discovery")
    {
        output.push_str(&format!(
            "| {} | {} | {} | `{}` | {} |\n",
            label(row, "scenario_id"),
            label(row, "strategy"),
            label(row, "outcome"),
            row["selected_path"].as_str().unwrap_or("none"),
            number(row, "response_bytes")
        ));
    }
    output.push_str("\n## Known-path fixtures\n\n| Scenario | Public selector |\n|---|---|\n");
    for row in rows(metrics).iter().filter(|row| {
        row["task_mode"] == "known_path_recoverability" && row["strategy"] == "raw_context"
    }) {
        output.push_str(&format!(
            "| {} | `{}` |\n",
            label(row, "scenario_id"),
            label(row, "oracle_path")
        ));
    }
    output.push_str("\nThe tiny payload counterexample is retained. These tables impose no requirement that prog win a cost comparison. Broad search, file search, and raw input may cost less. Exact command traces and every response size are recorded in the generated JSON; traces are audit data, not additional strategy observations.\n");
    output
}

pub fn task_report(metrics: &Value) -> String {
    let mut report = String::from(
        "# Known-path recoverability eval\n\n\
         This deterministic suite supplies the exact lookup selector in every task and grades evidence availability after execution. It does not measure discovery or actual-agent task success. Real-agent outcomes require separate live trials. The historical filenames remain for compatibility.\n\n\
         The expected answer is private to grading. Line-search terms are derived from the public selector, never from the answer. Native JSON selection is unavailable for non-JSON artifacts. The raw fixture is supplied outside model context to every strategy; source setup is excluded. All actual strategy stdout, including initial capture and expansion, is counted. No model answer tokens are generated, and timings are local measurements rather than assumed jq/RTK latency.\n\n\
         Token counts approximate total response bytes/4, rounded up per task before aggregation.\n\n\
         Source: [`task-success-metrics.json`](../fixtures/evals/task-success-metrics.json). Refresh measurements with `PROG_TASK_EVAL_UPDATE=1 cargo test -p prog-cli --test task_success -- --nocapture`; render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write` or check them with `--check`.\n\n\
         ## Aggregate\n\n\
         | Strategy | Evidence available | Attempted | Unavailable | Response bytes | Approx. input tokens | Tool calls | Expansions | Cache hits |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for strategy in [
        "raw",
        "simple_truncation",
        "native_json_selection",
        "rtk_grep_filter",
        "prog_call_only",
        "prog_expand",
    ] {
        let selected = rows(metrics)
            .iter()
            .filter(|row| row["strategy"] == strategy)
            .collect::<Vec<_>>();
        report.push_str(&format!(
            "| {strategy} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            selected
                .iter()
                .filter(|row| row["evidence_available"] == true)
                .count(),
            selected
                .iter()
                .filter(|row| row["available"] == true)
                .count(),
            selected
                .iter()
                .filter(|row| row["available"] == false)
                .count(),
            sum(&selected, "response_bytes"),
            selected
                .iter()
                .map(|row| approx_tokens(number(row, "response_bytes")))
                .sum::<u64>(),
            sum(&selected, "tool_calls"),
            sum(&selected, "expansion_count"),
            sum(&selected, "cache_hits")
        ));
    }
    report.push_str("\n## Scenarios\n\n| Scenario | Artifact | Public lookup path | Counterexample |\n|---|---|---|---:|\n");
    for row in rows(metrics).iter().filter(|row| row["strategy"] == "raw") {
        report.push_str(&format!(
            "| {} | {} | `{}` | {} |\n",
            label(row, "scenario_id"),
            label(row, "artifact"),
            label(row, "public_lookup_path"),
            row["counterexample"]
        ));
    }
    report.push_str("\n## Counterexamples\n\nThe tiny payload scenario remains visible without a required cost ordering. Successful `prog_expand` rows prove that supplied paths are recoverable; they do not prove an agent discovered the path or solved the task.\n");
    report
}

pub fn render_documents(root: &std::path::Path) -> Vec<(&'static str, String)> {
    let load = |path: &str| -> Value {
        serde_json::from_slice(
            &std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}")),
        )
        .unwrap()
    };
    let token = load(ARTIFACTS[0]);
    let evidence = load(ARTIFACTS[1]);
    let demos = load(ARTIFACTS[2]);
    let competitive = load(ARTIFACTS[3]);
    let task = load(ARTIFACTS[4]);
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
    let hero = rows(&token["rows"])
        .iter()
        .find(|row| row["fixture"] == "HTTP" && row["task"] == "Discover shape")
        .expect("HTTP Discover shape hero workload");
    let (low, high) = range(
        rows(&token["rows"])
            .iter()
            .map(|row| token_ratio(number(row, "raw_bytes"), number(row, "prog_bytes"))),
        1,
    );
    let raw_tokens = approx_tokens(number(hero, "raw_bytes"));
    let prog_tokens = approx_tokens(number(hero, "prog_bytes"));
    let hero_text = format!(
        "```text\n                       approximate tokens into the model\n  raw fixture     {:>10}\n  prog task       {:>10}\n```\n\n<sub>HTTP “Discover shape” from [`docs/token-economics.md`](docs/token-economics.md),\nrendered from the [checked-in rows](fixtures/evals/token-economics-metrics.json).\nRatios across these deterministic fixtures range {low}x-{high}x.\nCounts use the bytes/4 approximation, not provider tokens or a promise about your workload.</sub>",
        thousands(raw_tokens),
        thousands(prog_tokens)
    );
    let mut readme = replace_block(&read("README.md"), "hero", &hero_text);
    let token_text = format!(
        "Across the checked-in HTTP, CLI, and MCP tasks, raw-fixture token estimates\ndivided by complete `prog` task estimates range from **{low}x-{high}x**. Every\ntask includes its initial envelope and any expansions. Estimates use bytes/4,\nrounded up; these are fixture measurements, not provider token counts. See\n[`docs/token-economics.md`](docs/token-economics.md) and the\n[measurement rows](fixtures/evals/token-economics-metrics.json)."
    );
    readme = replace_block(&readme, "tokens", &token_text);
    let (count, correct, findings_calls, baseline_calls, findings_tokens, baseline_tokens) =
        evidence_totals(&evidence);
    let evidence_text = format!(
        "The {count} checked-in evidence-acquisition scenarios rank the expected causal\npath first in **{correct}/{count}** cases. The modeled findings workflow uses {} tool calls\nversus {} for `envelope -> paths -> evidence`; approximate output costs are\n{} versus {} tokens using bytes/4. These costs serialize core structures and\nmodel workflow calls; they do not measure complete CLI stdout or acquisition. See\n[`docs/evidence-acquisition.md`](docs/evidence-acquisition.md) and the\n[checked measurements](fixtures/evals/evidence-acquisition-metrics.json).",
        thousands(findings_calls),
        thousands(baseline_calls),
        thousands(findings_tokens),
        thousands(baseline_tokens)
    );
    readme = replace_block(&readme, "evidence", &evidence_text);
    let (low, high) = range(
        rows(&demos).iter().map(|row| {
            token_ratio(
                number(row, "raw_payload_bytes"),
                number(row, "expansion_task_bytes"),
            )
        }),
        2,
    );
    let demo_text = format!(
        "The {} checked-in workflow demos report raw-to-envelope-plus-expansion ratios\nfrom **{low}x to {high}x**, using the bytes/4 token approximation. These are\ngenerated local payloads, not credentialed live service measurements. See\n[`docs/real-world-demos.md`](docs/real-world-demos.md) and the\n[recorded metrics](fixtures/evals/real-world-demo-metrics.json).",
        rows(&demos).len()
    );
    readme = replace_block(&readme, "demos", &demo_text);
    readme = replace_competitive_readme(&readme, &competitive);
    vec![
        (DOCUMENTS[0], readme),
        (DOCUMENTS[1], token_report(&token)),
        (
            DOCUMENTS[2],
            replace_block(
                &read(DOCUMENTS[2]),
                "evidence-table",
                &evidence_table(&evidence),
            ),
        ),
        (
            DOCUMENTS[3],
            replace_block(&read(DOCUMENTS[3]), "demo-table", &demo_table(&demos)),
        ),
        (DOCUMENTS[4], competitive_report(&competitive)),
        (DOCUMENTS[5], task_report(&task)),
    ]
}

pub fn stale_documents(root: &std::path::Path) -> Vec<&'static str> {
    render_documents(root)
        .into_iter()
        .filter(|(path, text)| std::fs::read_to_string(root.join(path)).unwrap() != *text)
        .map(|(path, _)| path)
        .collect()
}
pub fn write_documents(root: &std::path::Path) -> Vec<&'static str> {
    // Render and validate everything before the first write. Never change source
    // measurements or their reviewed ceilings, and do not touch unchanged files.
    let documents = render_documents(root);
    let mut changed = Vec::new();
    for (path, text) in documents {
        if std::fs::read_to_string(root.join(path)).unwrap() != text {
            std::fs::write(root.join(path), text).unwrap();
            changed.push(path);
        }
    }
    changed
}
