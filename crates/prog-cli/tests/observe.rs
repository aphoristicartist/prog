//! Integration coverage for artifact observation and parsing.

use serde_json::Value;

mod support;

use support::*;

#[test]
fn observe_parser_metadata_covers_structured_and_fallback_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let diff = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/x-diff",
            "--name",
            "diff",
        ],
        b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new-target\n",
    );
    assert!(diff.status.success(), "{}", stdout(&diff));
    let diff_value: Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(diff_value["data_preview"]["format"], "unified_diff");
    assert_eq!(diff_value["observation"]["parser"]["id"], "unified_diff");
    assert_eq!(diff_value["observation"]["parser"]["lossy"], false);
    assert!(
        diff_value["observation"]["parser"]["confidence"]
            .as_f64()
            .unwrap()
            > 0.8
    );
    let diff_cursor = diff_value["cursor"].as_str().unwrap();
    let diff_expand = prog(&[
        "--dir",
        dir_arg,
        "expand",
        diff_cursor,
        "--path",
        "/lines/5/text",
    ]);
    assert!(diff_expand.status.success(), "{}", stdout(&diff_expand));
    let diff_expanded: Value = serde_json::from_slice(&diff_expand.stdout).unwrap();
    assert_eq!(diff_expanded["data_preview"], "+new-target");

    let sarif = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/sarif+json",
            "--name",
            "sarif",
        ],
        br#"{"version":"2.1.0","runs":[{"results":[{"message":{"text":"sarif-target"}}]}]}"#,
    );
    assert!(sarif.status.success(), "{}", stdout(&sarif));
    let sarif_value: Value = serde_json::from_slice(&sarif.stdout).unwrap();
    assert_eq!(sarif_value["observation"]["parser"]["id"], "sarif");
    let sarif_cursor = sarif_value["cursor"].as_str().unwrap();
    let sarif_expand = prog(&[
        "--dir",
        dir_arg,
        "expand",
        sarif_cursor,
        "--path",
        "/runs/0/results/0/message/text",
    ]);
    assert!(sarif_expand.status.success(), "{}", stdout(&sarif_expand));
    let sarif_expanded: Value = serde_json::from_slice(&sarif_expand.stdout).unwrap();
    assert_eq!(sarif_expanded["data_preview"], "sarif-target");

    let junit = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/junit+xml",
            "--name",
            "junit",
        ],
        br#"<testsuite><testcase classname="suite" name="case_a" time="0.1"><failure>junit-target</failure></testcase></testsuite>"#,
    );
    assert!(junit.status.success(), "{}", stdout(&junit));
    let junit_value: Value = serde_json::from_slice(&junit.stdout).unwrap();
    assert_eq!(junit_value["observation"]["parser"]["id"], "junit_xml");
    assert_eq!(junit_value["observation"]["parser"]["lossy"], true);
    assert_eq!(
        junit_value["data_preview"]["testcases"][0]["name"],
        "case_a"
    );

    let html = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/html",
            "--name",
            "html",
        ],
        br#"<!doctype html><html><head><title>Doc Target</title></head><body><h1>Heading Target</h1><a href="/next">Next</a></body></html>"#,
    );
    assert!(html.status.success(), "{}", stdout(&html));
    let html_value: Value = serde_json::from_slice(&html.stdout).unwrap();
    assert_eq!(html_value["observation"]["parser"]["id"], "html_basic");
    assert_eq!(html_value["data_preview"]["title"], "Doc Target");
    assert_eq!(html_value["data_preview"]["headings"][0], "Heading Target");
    assert_eq!(html_value["data_preview"]["links"][0]["href"], "/next");

    let fallback = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/x-unknown",
            "--name",
            "fallback",
        ],
        b"plain fallback target",
    );
    assert!(fallback.status.success(), "{}", stdout(&fallback));
    let fallback_value: Value = serde_json::from_slice(&fallback.stdout).unwrap();
    assert_eq!(
        fallback_value["observation"]["parser"]["id"],
        "text_fallback"
    );
    assert_eq!(fallback_value["observation"]["parser"]["fallback"], true);
}

#[test]
fn observe_parser_edge_cases_cover_malformed_json_and_stack_traces() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let malformed = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/json",
            "--name",
            "malformed-json",
        ],
        b"{not valid json but still useful text}",
    );
    assert!(malformed.status.success(), "{}", stdout(&malformed));
    let malformed_value: Value = serde_json::from_slice(&malformed.stdout).unwrap();
    assert_eq!(
        malformed_value["observation"]["parser"]["id"],
        "text_fallback"
    );
    assert!(
        malformed_value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("json:"))
    );

    let repeated_stack = "Error: boom\n    at service (app.js:10)\n    at main (app.js:20)\nError: boom\n    at service (app.js:10)\n    at main (app.js:20)\n";
    let stack = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
            "--name",
            "stack",
        ],
        repeated_stack.as_bytes(),
    );
    assert!(stack.status.success(), "{}", stdout(&stack));
    let stack_value: Value = serde_json::from_slice(&stack.stdout).unwrap();
    assert_eq!(stack_value["data_preview"]["repeated_stack_traces"], 2);
    assert_eq!(
        stack_value["observation"]["parser"]["range_semantics"],
        "line ranges from text"
    );
}
