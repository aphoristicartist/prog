//! Saved-artifact consistency, deliberately independent of fresh measurements.
#[path = "support/eval_reports.rs"]
mod eval_reports;

use std::{fs, path::Path};

use eval_reports::{ARTIFACTS, DOCUMENTS, render_documents, stale_documents, write_documents};
use serde_json::{Value, json};

fn repo_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for name in ARTIFACTS.into_iter().chain(DOCUMENTS) {
        let path = temp.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::copy(repo_root().join(name), path).unwrap();
    }
    write_documents(temp.path());
    temp
}

fn edit_artifact(root: &Path, index: usize, edit: impl FnOnce(&mut Value)) {
    let path = root.join(ARTIFACTS[index]);
    let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn snapshot(root: &Path) -> Vec<Vec<u8>> {
    ARTIFACTS
        .into_iter()
        .chain(DOCUMENTS)
        .map(|name| fs::read(root.join(name)).unwrap())
        .collect()
}

#[test]
fn sync_evaluation_documents() {
    if std::env::var_os("PROG_EVAL_DOCS_UPDATE").is_some() {
        for path in write_documents(repo_root()) {
            println!("updated {path}");
        }
    }
    let stale = stale_documents(repo_root());
    assert!(
        stale.is_empty(),
        "documentation differs from reviewed artifacts: {stale:?}; run scripts/regenerate-eval-docs.sh --write"
    );
}

#[test]
fn every_metric_family_detects_source_drift_without_writing() {
    for (source_index, index) in [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 2)] {
        let report = &DOCUMENTS[index];
        let temp = fixture();
        edit_artifact(temp.path(), source_index, |source| match source_index {
            0 => source["rows"][0]["prog_bytes"] = json!(1_000_001),
            1 => source["scenarios"][0]["top_finding_path"] = json!("/wrong"),
            2 => source[0]["expansion_task_bytes"] = json!(1_000_001),
            3 => {
                let row = source
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|row| {
                        row["task_mode"] == "deterministic_discovery"
                            && row["strategy"] == "prog_retrieve"
                    })
                    .unwrap();
                row["response_bytes"] = json!(1_000_001);
            }
            4 => source[0]["response_bytes"] = json!(1_000_001),
            5 => source["rows"][0]["response_bytes"] = json!(1_000_001),
            _ => unreachable!(),
        });
        let before = snapshot(temp.path());
        let stale = stale_documents(temp.path());
        assert!(stale.contains(report), "missing report drift: {report}");
        if index != 5 {
            assert!(
                stale.contains(&"README.md"),
                "missing headline drift: {report}"
            );
        }
        assert_eq!(snapshot(temp.path()), before, "check must not write");
        write_documents(temp.path());
        assert!(stale_documents(temp.path()).is_empty());
        // Rendering never changes any artifact, including reviewed ceilings.
        assert_eq!(
            &snapshot(temp.path())[..ARTIFACTS.len()],
            &before[..ARTIFACTS.len()]
        );
    }
}

#[test]
fn numerical_headline_edits_are_detected_and_repaired() {
    let temp = fixture();
    let path = temp.path().join("README.md");
    let original = fs::read_to_string(&path).unwrap();
    let row = original
        .lines()
        .find(|line| line.starts_with("  prog task "))
        .unwrap();
    let edited = original.replacen(row, "  prog task              1", 1);
    assert_ne!(edited, original);
    fs::write(&path, &edited).unwrap();
    assert_eq!(stale_documents(temp.path()), vec!["README.md"]);
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    write_documents(temp.path());
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn rendering_is_idempotent_and_preserves_unrelated_documentation() {
    let temp = fixture();
    for name in [
        "README.md",
        "docs/evidence-acquisition.md",
        "docs/real-world-demos.md",
    ] {
        let path = temp.path().join(name);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            format!("unrelated introduction\n{text}\nunrelated appendix\n"),
        )
        .unwrap();
    }
    edit_artifact(temp.path(), 0, |source| {
        source["rows"][0]["prog_bytes"] = json!(1_000_001)
    });
    let demos = fs::read_to_string(temp.path().join("docs/real-world-demos.md")).unwrap();
    assert!(!write_documents(temp.path()).is_empty());
    let once = snapshot(temp.path());
    assert!(write_documents(temp.path()).is_empty());
    assert_eq!(snapshot(temp.path()), once);
    assert_eq!(
        fs::read_to_string(temp.path().join("docs/real-world-demos.md")).unwrap(),
        demos
    );
    for name in [
        "README.md",
        "docs/evidence-acquisition.md",
        "docs/real-world-demos.md",
    ] {
        let text = fs::read_to_string(temp.path().join(name)).unwrap();
        assert!(text.starts_with("unrelated introduction\n"));
        assert!(text.ends_with("\nunrelated appendix\n"));
    }
}

#[test]
fn aggregates_rounding_counts_and_qualifications_follow_the_rows() {
    let temp = fixture();
    edit_artifact(temp.path(), 0, |source| {
        source["rows"] = json!([
            {"fixture":"HTTP", "task":"Discover shape", "raw_bytes":49381, "prog_bytes":4001},
            {"fixture":"CLI", "task":"Synthetic", "raw_bytes":400, "prog_bytes":12}
        ])
    });
    edit_artifact(temp.path(), 1, |source| {
        let first = &mut source["scenarios"][0];
        first["findings_output_tokens"] = json!(12345);
        first["baseline_output_tokens"] = json!(20000);
        let mut second = first.clone();
        second["top_finding_path"] = json!("/wrong");
        source["scenarios"] = json!([first.clone(), second]);
        // Redundant summaries are not an independent source of headline numbers.
        source["summary"] = json!({"scenario_count":999});
    });
    edit_artifact(temp.path(), 2, |source| {
        *source = json!([
            {"id":"synthetic-a", "raw_payload_bytes":400, "call_envelope_bytes":4, "expansion_task_bytes":12, "cache_hit_status":"hit"},
            {"id":"synthetic-b", "raw_payload_bytes":400, "call_envelope_bytes":4, "expansion_task_bytes":28, "cache_hit_status":"hit"}
        ])
    });
    edit_artifact(temp.path(), 5, |source| {
        let mut rows = vec![];
        for (strategy, bytes, calls) in [
            ("findings", 49377, 2),
            ("paths", 80000, 3),
            ("inspect", 200, 3),
        ] {
            for correct in [true, false] {
                rows.push(
                    json!({"scenario":"synthetic", "strategy":strategy, "response_bytes":bytes,
                    "tool_calls":calls, "correct":correct, "output_tokens":999999}),
                );
            }
        }
        source["rows"] = json!(rows);
    });
    let docs = render_documents(temp.path());
    let readme = &docs[0].1;
    for expected in [
        "12,346",
        "1,001",
        "12.3x-33.3x",
        "**1/2**",
        "| findings | 1/2 | 4 | 24,690 |",
        "| paths | 1/2 | 6 | 40,000 |",
        "The 2 checked-in workflow demos",
        "14.29x to 33.33x",
        "bytes/4",
        "not provider tokens",
        "not actual-agent task",
        "not credentialed live service measurements",
    ] {
        assert!(readme.contains(expected), "missing {expected}");
    }
    for artifact in ARTIFACTS.into_iter().take(4) {
        assert!(readme.contains(artifact), "missing provenance {artifact}");
    }
    assert!(readme.contains(ARTIFACTS[5]));
    assert!(docs[2].1.contains("evidence-cli-metrics.json"));
    assert!(docs[2].1.contains("Component regression measurements"));
    assert!(docs[2].1.contains("Actual CLI workflow measurements"));
    assert!(
        docs[1]
            .1
            .contains("| HTTP | Discover shape | 12346 | 1001 | 12.3x |")
    );
    assert!(docs[2].1.contains("1/2 scenarios"));
    assert!(docs[3].1.contains("33.33x"));
    assert!(docs[3].1.contains("14.29x"));
    for (index, (_, text)) in docs.iter().enumerate().skip(1) {
        let filename = Path::new(ARTIFACTS[index - 1])
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            text.contains(filename),
            "missing report provenance {filename}"
        );
        assert!(text.contains("bytes/4") || text.contains("bytes / 4"));
    }
    assert_eq!(eval_reports::approx_tokens(0), 0);
    assert_eq!(eval_reports::approx_tokens(5), 2);
    assert_eq!(eval_reports::thousands(0), "0");
    assert_eq!(eval_reports::thousands(123_456_789), "123,456,789");
}

#[test]
fn invalid_boundaries_fail_before_any_document_write() {
    let temp = fixture();
    let path = temp.path().join("docs/real-world-demos.md");
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, original.replace("<!-- eval:demo-table:end -->", "")).unwrap();
    edit_artifact(temp.path(), 0, |source| {
        source["rows"][0]["prog_bytes"] = json!(1_000_001)
    });
    let before = snapshot(temp.path());
    assert!(std::panic::catch_unwind(|| write_documents(temp.path())).is_err());
    assert_eq!(snapshot(temp.path()), before);
}
