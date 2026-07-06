use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
const MIN_TOKEN_RATIO: f64 = 2.0;

#[derive(Debug, Clone)]
struct Demo {
    id: &'static str,
    source_id: &'static str,
    kind: &'static str,
    seed: &'static str,
    operation: &'static str,
    generator: &'static str,
    collection_path: &'static str,
    target_path: &'static str,
    snippet: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DemoMetric {
    id: String,
    source_kind: String,
    raw_payload_bytes: usize,
    call_envelope_bytes: usize,
    expansion_envelope_bytes: usize,
    expansion_task_bytes: usize,
    cache_hit_status: String,
    cache_hit_envelope_bytes: usize,
    raw_tokens: usize,
    prog_task_tokens: usize,
    token_ratio: f64,
    target_path: String,
}

fn prog(root: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .current_dir(root)
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("prog binary should run")
}

fn python(root: &Path, args: &[&str]) -> Output {
    Command::new("python3")
        .current_dir(root)
        .args(args)
        .output()
        .expect("python3 should run")
}

#[test]
fn real_world_demo_suite_covers_painful_tool_outputs() {
    let root = repo_root();
    let tempdir = tempfile::tempdir().unwrap();
    let demos = demos();
    let metrics = demos
        .iter()
        .map(|demo| run_demo(&root, tempdir.path(), demo))
        .collect::<Vec<_>>();

    assert_eq!(metrics.len(), 5);
    for metric in &metrics {
        assert!(
            metric.call_envelope_bytes <= MAX_ENVELOPE_BYTES,
            "{} call envelope exceeded budget: {}",
            metric.id,
            metric.call_envelope_bytes
        );
        assert!(
            metric.expansion_envelope_bytes <= MAX_ENVELOPE_BYTES,
            "{} expansion envelope exceeded budget: {}",
            metric.id,
            metric.expansion_envelope_bytes
        );
        assert_eq!(
            metric.cache_hit_status, "hit",
            "{} should hit cache",
            metric.id
        );
        assert!(
            metric.token_ratio >= MIN_TOKEN_RATIO,
            "{} ratio too low: {:.2}x",
            metric.id,
            metric.token_ratio
        );
    }

    if std::env::var_os("PROG_REAL_WORLD_DEMO_UPDATE").is_some() {
        let docs = report(&metrics, &demos);
        fs::write(root.join("docs/real-world-demos.md"), docs).unwrap();
        fs::write(
            root.join("fixtures/evals/real-world-demo-metrics.json"),
            serde_json::to_vec_pretty(&metrics).unwrap(),
        )
        .unwrap();
    } else {
        let checked_in = root.join("fixtures/evals/real-world-demo-metrics.json");
        assert!(
            checked_in.exists(),
            "checked-in real-world demo metrics should exist"
        );
        let stored: Vec<DemoMetric> =
            serde_json::from_slice(&fs::read(checked_in).unwrap()).unwrap();
        assert_eq!(stored.len(), metrics.len());
        for metric in &stored {
            assert!(
                metric.token_ratio >= MIN_TOKEN_RATIO,
                "{} checked-in ratio too low",
                metric.id
            );
        }
    }
}

fn run_demo(root: &Path, dir: &Path, demo: &Demo) -> DemoMetric {
    let raw = python(
        root,
        &["demos/real-world/generate_payload.py", demo.generator],
    );
    assert_success(&raw);

    let discover = prog(
        root,
        dir,
        &[
            "discover",
            demo.source_id,
            "--kind",
            demo.kind,
            "--seed",
            demo.seed,
        ],
    );
    assert_success(&discover);

    let first_call = prog(
        root,
        dir,
        &["call", demo.source_id, demo.operation, "--args", "{}"],
    );
    assert_success(&first_call);
    let first_call_json = json(&first_call);
    assert_eq!(first_call_json["cache"]["status"], "stored");
    let cursor = first_call_json["cursor"].as_str().unwrap().to_string();

    let expansion = prog(root, dir, &["expand", &cursor, "--path", demo.target_path]);
    assert_success(&expansion);
    let expansion_json = json(&expansion);
    assert_eq!(expansion_json["cache"]["status"], "hit");

    let collection = prog(
        root,
        dir,
        &[
            "expand",
            &cursor,
            "--path",
            demo.collection_path,
            "--limit",
            "12",
            "--depth",
            "3",
        ],
    );
    assert_success(&collection);

    let cache_hit = prog(
        root,
        dir,
        &["call", demo.source_id, demo.operation, "--args", "{}"],
    );
    assert_success(&cache_hit);
    let cache_hit_json = json(&cache_hit);

    let call_envelope_bytes = envelope_bytes(&first_call_json);
    let expansion_envelope_bytes = envelope_bytes(&expansion_json);
    let cache_hit_envelope_bytes = envelope_bytes(&cache_hit_json);
    let expansion_task_bytes = call_envelope_bytes + expansion_envelope_bytes;
    let raw_tokens = approx_tokens(raw.stdout.len());
    let prog_task_tokens = approx_tokens(expansion_task_bytes);

    DemoMetric {
        id: demo.id.to_string(),
        source_kind: demo.kind.to_string(),
        raw_payload_bytes: raw.stdout.len(),
        call_envelope_bytes,
        expansion_envelope_bytes,
        expansion_task_bytes,
        cache_hit_status: cache_hit_json["cache"]["status"]
            .as_str()
            .unwrap()
            .to_string(),
        cache_hit_envelope_bytes,
        raw_tokens,
        prog_task_tokens,
        token_ratio: round_ratio(raw_tokens as f64 / prog_task_tokens.max(1) as f64),
        target_path: demo.target_path.to_string(),
    }
}

fn demos() -> Vec<Demo> {
    vec![
        Demo {
            id: "github-pr-review",
            source_id: "github_review",
            kind: "cli",
            seed: "demos/real-world/seeds/github-pr-review.json",
            operation: "review",
            generator: "github_pr_review",
            collection_path: "/review_threads",
            target_path: "/review_threads/37/comments/0/body",
            snippet: "prog --dir /tmp/prog-real-world discover github_review --kind cli --seed demos/real-world/seeds/github-pr-review.json",
        },
        Demo {
            id: "kubectl-events",
            source_id: "kubectl_events",
            kind: "cli",
            seed: "demos/real-world/seeds/kubectl-events.json",
            operation: "events",
            generator: "kubectl_events",
            collection_path: "/items",
            target_path: "/items/42/message",
            snippet: "prog --dir /tmp/prog-real-world discover kubectl_events --kind cli --seed demos/real-world/seeds/kubectl-events.json",
        },
        Demo {
            id: "cloudwatch-logs",
            source_id: "cloudwatch_logs",
            kind: "cli",
            seed: "demos/real-world/seeds/cloudwatch-logs.json",
            operation: "logs",
            generator: "cloudwatch_logs",
            collection_path: "/events",
            target_path: "/events/77/message",
            snippet: "prog --dir /tmp/prog-real-world discover cloudwatch_logs --kind cli --seed demos/real-world/seeds/cloudwatch-logs.json",
        },
        Demo {
            id: "jira-triage",
            source_id: "jira_triage",
            kind: "cli",
            seed: "demos/real-world/seeds/jira-triage.json",
            operation: "issues",
            generator: "jira_triage",
            collection_path: "/issues",
            target_path: "/issues/31/comments/1/body",
            snippet: "prog --dir /tmp/prog-real-world discover jira_triage --kind cli --seed demos/real-world/seeds/jira-triage.json",
        },
        Demo {
            id: "mcp-incidents",
            source_id: "incident_mcp",
            kind: "mcp",
            seed: "demos/real-world/seeds/mcp-incidents.json",
            operation: "list_incidents",
            generator: "mcp_incidents",
            collection_path: "/alerts",
            target_path: "/alerts/33/runbook",
            snippet: "prog --dir /tmp/prog-real-world discover incident_mcp --kind mcp --seed demos/real-world/seeds/mcp-incidents.json",
        },
    ]
}

fn report(metrics: &[DemoMetric], demos: &[Demo]) -> String {
    let mut output = String::from(
        "# Real-world demo metrics\n\n\
         Deterministic local demos for recognizable noisy agent workflows. Token counts use the project heuristic `bytes / 4`, rounded up. `expansion_task_bytes` is the initial `prog call` envelope plus the target `prog expand` envelope.\n\n\
         See `demos/real-world/README.md` for copy-paste commands and optional credentialed captures that can emit a local report with `demos/real-world/report_payloads.py`.\n\n\
         Regenerate with `PROG_REAL_WORLD_DEMO_UPDATE=1 cargo test -p prog-cli --test real_world_demos -- --nocapture`.\n\n\
         | Demo | Raw bytes | call envelope bytes | expansion task bytes | cache hit | Token ratio |\n\
         |---|---:|---:|---:|---|---:|\n",
    );
    for metric in metrics {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2}x |\n",
            metric.id,
            metric.raw_payload_bytes,
            metric.call_envelope_bytes,
            metric.expansion_task_bytes,
            metric.cache_hit_status,
            metric.token_ratio
        ));
    }
    output.push_str("\n## Copy-paste seeds\n\n");
    for demo in demos {
        output.push_str("```bash\n");
        output.push_str(demo.snippet);
        output.push('\n');
        output.push_str(&format!(
            "prog --dir /tmp/prog-real-world call {} {} --args '{{}}'\n",
            demo.source_id, demo.operation
        ));
        output.push_str("```\n\n");
    }
    output
}

fn envelope_bytes(value: &Value) -> usize {
    value["summary"]["envelope_bytes"].as_u64().unwrap() as usize
}

fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

fn round_ratio(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr should be empty: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest should be under crates/prog-cli")
        .to_path_buf()
}
