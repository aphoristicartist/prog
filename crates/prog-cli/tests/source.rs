//! Integration coverage for safe source onboarding.

use std::fs;

use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

mod support;

use support::*;

#[tokio::test]
async fn source_add_http_creates_working_profile_from_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 1, "state": "open", "body": "x".repeat(500)}]
        })))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let url = format!("{}/items", server.uri());

    let output = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-http",
        "api",
        "--operation",
        "list",
        "--url",
        &url,
        "--probe",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_id"], "api");
    assert_eq!(report["kind"], "http");
    assert_eq!(report["generated_seed"]["kind"], "http");
    assert_eq!(report["generated_seed"]["base_url"], server.uri());
    assert_eq!(report["generated_seed"]["operations"][0]["path"], "/items");
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["read_only"],
        true
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["network"],
        true
    );
    assert_eq!(report["discovery"]["operations_found"], 1);
    assert_eq!(report["discovery"]["operations_probed"], 1);
    assert_eq!(report["discovery"]["shapes_learned"], 1);
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap()
                == format!("prog --dir {dir_arg} call api list --args '{{}}'"))
    );

    let profile = read_profile(dir.path(), "api");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], true);
    assert_eq!(effects["mutating"], false);
    assert_eq!(effects["network"], true);
    assert_eq!(effects["cacheable"], true);
    assert!(profile["operations"][0]["output_shape"].is_object());

    let call = prog(&["--dir", dir_arg, "call", "api", "list", "--args", "{}"]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["source_id"], "api");
    assert_eq!(envelope["operation"], "list");
    assert_eq!(
        envelope["observation"]["capture"]["budget"]["source"],
        "profile"
    );
    assert_eq!(
        envelope["observation"]["capture"]["budget"]["limits"][0]["scope"],
        "body"
    );
    assert_eq!(
        envelope["observation"]["capture"]["budget"]["limits"][0]["max_bytes"],
        2 * 1024 * 1024
    );
    assert_eq!(
        envelope["capture_budget"],
        envelope["observation"]["capture"]["budget"]
    );
    assert_eq!(envelope["storage_budget"]["source"], "default");
}

#[test]
fn source_add_cli_creates_working_profile_from_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "import json\nprint(json.dumps({'items':[{'id': 1, 'state': 'open', 'body': 'x' * 500}]}))\n",
    )
    .unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let output = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-cli",
        "local",
        "--operation",
        "list",
        "--read-only",
        "--",
        "python3",
        script.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_id"], "local");
    assert_eq!(report["kind"], "cli");
    assert_eq!(
        report["generated_seed"]["operations"][0]["command"],
        "python3"
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["args"][0],
        script.to_str().unwrap()
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["read_only"],
        true
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["network"],
        false
    );
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap()
                == format!("prog --dir {dir_arg} call local list --args '{{}}'"))
    );
    assert_eq!(
        report["discovery"]["effects_assumed"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let profile = read_profile(dir.path(), "local");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], true);
    assert_eq!(effects["mutating"], false);
    assert_eq!(effects["network"], false);
    assert_eq!(effects["shell"], false);
    assert_eq!(effects["cacheable"], true);

    let call = prog(&["--dir", dir_arg, "call", "local", "list", "--args", "{}"]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["items"][0]["state"], "open");
    assert_eq!(
        envelope["observation"]["capture"]["budget"]["source"],
        "profile"
    );
    assert_eq!(
        envelope["observation"]["capture"]["budget"]["limits"][0]["scope"],
        "stdout"
    );
    assert_eq!(
        envelope["observation"]["capture"]["budget"]["limits"][1]["scope"],
        "stderr"
    );
}

#[test]
fn source_add_preserves_fail_closed_defaults_and_reports_invalid_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let invalid = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-http",
        "api",
        "--operation",
        "list",
        "--url",
        "ftp://example.test/items",
    ]);
    assert!(!invalid.status.success());
    let error: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("scheme must be http or https")
    );

    let output = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-cli",
        "danger",
        "--operation",
        "show",
        "--",
        "python3",
        "-c",
        "import json; print(json.dumps({'ok': True}))",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("confirmation-gated"))
    );
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap()
                == format!("prog --dir {dir_arg} call danger show --args '{{}}' --yes"))
    );
    let profile = read_profile(dir.path(), "danger");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], false);
    assert_eq!(effects["mutating"], true);
    assert_eq!(effects["requires_confirmation"], true);
    assert_eq!(effects["sensitive"], true);
    assert_eq!(effects["cacheable"], false);

    let call = prog(&["--dir", dir_arg, "call", "danger", "show", "--args", "{}"]);
    assert!(!call.status.success());
    let error: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "requires_confirmation");
}

#[test]
fn source_add_cli_detects_and_optionally_applies_structured_output() {
    let dir = tempfile::tempdir().unwrap();
    let suggested = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "source",
        "add-cli",
        "pods",
        "--operation",
        "list",
        "--read-only",
        "--",
        "kubectl",
        "get",
        "pods",
    ]);
    assert!(suggested.status.success(), "{}", stdout(&suggested));
    let suggested_value: Value = serde_json::from_slice(&suggested.stdout).unwrap();
    assert_eq!(
        suggested_value["structured_output"][0]["status"],
        "suggested"
    );
    assert_eq!(
        suggested_value["structured_output"][0]["flag"],
        json!(["-o", "json"])
    );

    let applied = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "source",
        "add-cli",
        "pods_json",
        "--operation",
        "list",
        "--read-only",
        "--prefer-json",
        "--",
        "kubectl",
        "get",
        "pods",
    ]);
    assert!(applied.status.success(), "{}", stdout(&applied));
    let applied_value: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(
        applied_value["generated_seed"]["operations"][0]["args"],
        json!(["get", "pods", "-o", "json"])
    );
    assert_eq!(applied_value["structured_output"][0]["status"], "detected");
}
