use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;

fn prog(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(args)
        .output()
        .expect("prog binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be utf8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf8")
}

fn assert_placeholder(args: &[&str], command: &str) {
    let output = prog(args);
    assert!(
        !output.status.success(),
        "{args:?} should fail until implemented"
    );
    assert_eq!(
        stderr(&output),
        "",
        "{args:?} must not write diagnostics to stderr"
    );

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "not_implemented");
    assert!(
        value["error"]["message"]
            .as_str()
            .expect("message should be a string")
            .contains(command),
        "message should name {command}"
    );
    assert!(
        value["error"]["hint"]
            .as_str()
            .expect("hint should be a string")
            .contains("issue #1"),
        "hint should point to the scaffold state"
    );
}

#[test]
fn help_shows_complete_command_tree() {
    let output = prog(&["--help"]);
    assert!(output.status.success());
    assert_eq!(stderr(&output), "");

    let help = stdout(&output);
    for expected in [
        "discover", "hints", "call", "expand", "cache", "meta", "--dir", "--pretty",
    ] {
        assert!(help.contains(expected), "help should include {expected}");
    }
}

#[test]
fn every_placeholder_command_returns_structured_json_on_stdout() {
    let cases: &[(&[&str], &str)] = &[
        (&["call", "local", "list", "--args", "{}"], "call"),
        (&["expand", "pc1_test", "--path", "/items/0"], "expand"),
        (&["meta"], "meta"),
    ];

    for (args, command) in cases {
        assert_placeholder(args, command);
    }
}

#[test]
fn discover_http_seed_persists_profile_without_upstream_probe() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(
        dir.path(),
        "http.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "list",
            "method": "GET",
            "path": "/items",
            "args": {"owner": "string"},
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": true,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operations_found"], 1);
    assert_eq!(report["operations_probed"], 0);

    let profile = read_profile(dir.path(), "api");
    assert_eq!(profile["kind"], "http");
    assert_eq!(profile["operations"][0]["id"], "list");
}

#[test]
fn discover_http_get_defaults_to_read_only_but_post_hardens_seed_claims() {
    let dir = tempfile::tempdir().unwrap();
    let get_seed = write_seed(
        dir.path(),
        "http-get.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "list",
            "method": "GET",
            "path": "/items"
          }]
        }"#,
    );
    let post_seed = write_seed(
        dir.path(),
        "http-post.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "create",
            "method": "POST",
            "path": "/items",
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": false,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );

    let get = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api_get",
        "--kind",
        "http",
        "--seed",
        get_seed.to_str().unwrap(),
    ]);
    assert!(get.status.success(), "{}", stdout(&get));
    let get_profile = read_profile(dir.path(), "api_get");
    let get_effects = &get_profile["operations"][0]["effects"];
    assert_eq!(get_effects["read_only"], true);
    assert_eq!(get_effects["mutating"], false);
    assert_eq!(get_effects["network"], true);
    assert_eq!(get_effects["cacheable"], true);

    let post = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api_post",
        "--kind",
        "http",
        "--seed",
        post_seed.to_str().unwrap(),
    ]);
    assert!(post.status.success(), "{}", stdout(&post));
    let post_profile = read_profile(dir.path(), "api_post");
    let post_effects = &post_profile["operations"][0]["effects"];
    assert_eq!(post_effects["read_only"], false);
    assert_eq!(post_effects["mutating"], true);
    assert_eq!(post_effects["network"], true);
    assert_eq!(post_effects["requires_confirmation"], true);
}

#[test]
fn discover_probe_learns_shape_and_hints_expose_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(dir.path(), "cli.json", &cli_probe_seed(true));

    let discover = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
        "--probe",
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));
    let report: Value = serde_json::from_slice(&discover.stdout).unwrap();
    assert_eq!(report["operations_probed"], 1);
    assert_eq!(report["shapes_learned"], 1);

    let hints = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "hints",
        "local",
        "emit",
    ]);
    assert!(hints.status.success(), "{}", stdout(&hints));
    let value: Value = serde_json::from_slice(&hints.stdout).unwrap();
    let operation = &value["hints"]["operations"][0];
    assert_eq!(operation["id"], "emit");
    assert!(operation["inputs"].get("required").is_some());
    assert!(
        operation["output_fields"]["observed"]
            .as_str()
            .unwrap()
            .contains("object")
    );
    assert!(
        value["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/operations/0/output_fields/observed")
    );
    assert!(value["cursor"].as_str().unwrap().starts_with("pc1_"));
    assert!(
        !operation["expandable_regions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(operation["effects"]["read_only"], true);
}

#[test]
fn rediscover_preserves_learned_shape() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(dir.path(), "cli.json", &cli_probe_seed(true));
    let dir_arg = dir.path().to_str().unwrap();
    let seed_arg = seed.to_str().unwrap();

    let first = prog(&[
        "--dir", dir_arg, "discover", "local", "--kind", "cli", "--seed", seed_arg, "--probe",
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let second = prog(&[
        "--dir", dir_arg, "discover", "local", "--kind", "cli", "--seed", seed_arg,
    ]);
    assert!(second.status.success(), "{}", stdout(&second));

    let profile = read_profile(dir.path(), "local");
    assert!(profile["operations"][0]["output_shape"].is_object());
    assert_eq!(profile["version"], 2);
}

#[test]
fn probe_skips_effectless_operations_with_i6_warning() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(dir.path(), "unsafe-cli.json", &cli_probe_seed(false));

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "unsafe_local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
        "--probe",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operations_probed"], 0);
    assert!(report["warnings"][0].as_str().unwrap().contains("I6"));
    let profile = read_profile(dir.path(), "unsafe_local");
    assert!(profile["operations"][0]["output_shape"].is_null());
}

#[test]
fn partial_effect_metadata_keeps_missing_flags_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(
        dir.path(),
        "partial-cli.json",
        r#"{
          "kind": "cli",
          "operations": [{
            "name": "emit",
            "command": "python3",
            "args": ["-c", "print('{name}')"],
            "input_schema": {
              "type": "object",
              "required": ["name"],
              "properties": {"name": {"type": "string", "default": "ok"}}
            },
            "effect": {"read_only": true, "shell": false, "network": false}
          }]
        }"#,
    );

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "partial",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
        "--probe",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operations_probed"], 0);
    assert!(report["warnings"][0].as_str().unwrap().contains("mutating"));
    let profile = read_profile(dir.path(), "partial");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], true);
    assert_eq!(effects["mutating"], true);
    assert_eq!(effects["cacheable"], false);
    assert_eq!(effects["requires_confirmation"], true);
}

#[test]
fn discover_bad_seed_names_bad_field() {
    let dir = tempfile::tempdir().unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "bad",
        "--kind",
        "http",
        "--seed",
        r#"{"kind":"http","base_url":1,"operations":[]}"#,
    ]);

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "bad_args");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("base_url")
    );
}

#[test]
fn cache_list_and_purge_are_real_json_commands() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let list = prog(&["--dir", dir_arg, "cache", "list"]);
    assert!(list.status.success());
    assert_eq!(stderr(&list), "");
    let value: Value = serde_json::from_slice(&list.stdout).expect("stdout must be JSON");
    assert_eq!(value["entries"], json_array());

    let purge = prog(&["--dir", dir_arg, "cache", "purge", "--all"]);
    assert!(purge.status.success());
    assert_eq!(stderr(&purge), "");
    let value: Value = serde_json::from_slice(&purge.stdout).expect("stdout must be JSON");
    assert_eq!(value["purged_entries"], 0);
    assert_eq!(value["purged_payloads"], 0);
    assert_eq!(value["purged_cursors"], 0);
}

#[test]
fn cache_get_missing_uses_structured_cache_miss_error() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let output = prog(&["--dir", dir_arg, "cache", "get", "sha256:missing"]);

    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "cache_miss");
}

fn json_array() -> Value {
    Value::Array(Vec::new())
}

fn write_seed(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn read_profile(dir: &Path, id: &str) -> Value {
    serde_json::from_slice(&fs::read(dir.join("profiles").join(format!("{id}.json"))).unwrap())
        .unwrap()
}

fn cli_probe_seed(with_effect: bool) -> String {
    let effect = if with_effect {
        r#","effect":{"read_only":true,"mutating":false,"network":false,"shell":false,"sensitive":false,"cacheable":true,"requires_confirmation":false}"#
    } else {
        ""
    };
    format!(
        r#"{{
          "kind": "cli",
          "operations": [{{
            "name": "emit",
            "command": "python3",
            "args": ["-c", "import json; print(json.dumps({{'items':[{{'id':1,'state':'open','body':'{}'}}]}}))"]
            {effect}
          }}]
        }}"#,
        "x".repeat(1000)
    )
}

#[test]
fn pretty_errors_are_still_machine_readable() {
    let output = prog(&["--pretty", "meta"]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");

    let text = stdout(&output);
    assert!(text.starts_with("{\n"));
    let value: Value = serde_json::from_str(&text).expect("pretty stdout must still be JSON");
    assert_eq!(value["error"]["kind"], "not_implemented");
}

#[test]
fn parser_errors_use_the_same_json_error_contract() {
    let output = prog(&["unknown"]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "cli_usage");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    assert!(
        value["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("prog --help")
    );
}
