use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

use prog_adapters::cli::{CliOperation, CliSource};
use prog_core::TrustSettings;
use serde_json::json;

#[tokio::test]
async fn parses_json_stdout() {
    let source = source(operation(
        "json",
        &["-c", "import json; print(json.dumps({'items':[1,2]}))"],
    ));

    let result = source.execute("json", &json!({})).await.unwrap();

    assert_eq!(result.data["items"], json!([1, 2]));
    assert_eq!(result.provenance.exit_code, Some(0));
    assert_eq!(result.diagnostics.stderr["line_count"], 0);
}

#[tokio::test]
async fn wraps_text_output_with_head_tail_counts() {
    let source = source(operation(
        "text",
        &["-c", "for i in range(25): print('row-' + str(i))"],
    ));

    let result = source.execute("text", &json!({})).await.unwrap();

    assert_eq!(result.data["format"], "text");
    assert_eq!(result.data["line_count"], 25);
    assert_eq!(result.data["head"][0], "row-0");
    assert_eq!(result.data["tail"][9], "row-24");
}

#[tokio::test]
async fn text_output_redacts_common_secret_formats() {
    let source = source(operation(
        "text_secret",
        &[
            "-c",
            "print('Authorization: Bearer SECRET123')\nprint('token=abc api-key: def')",
        ],
    ));

    let result = source.execute("text_secret", &json!({})).await.unwrap();

    let rendered = serde_json::to_string(&result.data).unwrap();
    for secret in ["Bearer SECRET123", "abc", "def"] {
        assert!(!rendered.contains(secret), "{secret} leaked in {rendered}");
    }
    assert!(rendered.contains("[REDACTED:observed_text_secret]"));
}

#[tokio::test]
async fn argv_template_substitution_never_resplits_values() {
    let source = source(operation(
        "argv",
        &[
            "-c",
            "import sys; print(len(sys.argv)); print(sys.argv[1])",
            "{value}",
        ],
    ));

    let result = source
        .execute("argv", &json!({"value": "one value with spaces"}))
        .await
        .unwrap();

    assert_eq!(result.data["head"][0], "2");
    assert_eq!(result.data["head"][1], "one value with spaces");
}

#[tokio::test]
async fn env_templates_are_explicit_and_available_to_child() {
    let mut op = operation(
        "env",
        &["-c", "import os; print(os.environ['FIXTURE_VALUE'])"],
    );
    op.env
        .insert("FIXTURE_VALUE".to_string(), "prefix-{value}".to_string());
    let source = source(op);

    let result = source
        .execute("env", &json!({"value": "ok"}))
        .await
        .unwrap();

    assert_eq!(result.data["head"][0], "prefix-ok");
}

#[tokio::test]
async fn missing_and_unknown_args_are_actionable() {
    let source = source(operation("args", &["-c", "print('{name}')"]));

    let error = source
        .execute("args", &json!({"extra": true}))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), "bad_args");
    let message = error.to_string();
    assert!(message.contains("name"));
    assert!(message.contains("extra"));
}

#[tokio::test]
async fn input_schema_drives_missing_and_unknown_args() {
    let mut op = operation("schema", &["-c", "print('schema gate')"]);
    op.input_schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "required": ["name"]
    });
    let source = source(op);

    let error = source
        .execute("schema", &json!({"extra": "ignored"}))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), "bad_args");
    let message = error.to_string();
    assert!(message.contains("name"));
    assert!(message.contains("extra"));
}

#[tokio::test]
async fn non_zero_exit_returns_structured_error_with_bounded_stderr() {
    let mut op = operation(
        "fail",
        &[
            "-c",
            "import sys; sys.stderr.write('bad\\n' * 100); sys.exit(7)",
        ],
    );
    op.max_stderr_bytes = Some(12);
    let source = source(op);

    let error = source.execute("fail", &json!({})).await.unwrap_err();

    assert_eq!(error.kind(), "cli_exit");
    let rendered = serde_json::to_string(&error.envelope()).unwrap();
    assert!(rendered.contains("bad"));
    assert!(rendered.len() < 2048);
}

#[tokio::test]
async fn timeout_kills_child_and_returns_structured_error() {
    let mut op = operation("slow", &["-c", "import time; time.sleep(5)"]);
    op.timeout_ms = Some(50);
    let source = source(op);
    let started = Instant::now();

    let error = source.execute("slow", &json!({})).await.unwrap_err();

    assert_eq!(error.kind(), "cli_timeout");
    assert!(started.elapsed().as_secs() < 2);
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_does_not_wait_for_detached_pipe_holders() {
    let mut op = operation(
        "detached",
        &[
            "-c",
            r#"import os, time
pid = os.fork()
if pid == 0:
    os.setsid()
    time.sleep(2)
    os._exit(0)
time.sleep(5)
"#,
        ],
    );
    op.timeout_ms = Some(50);
    let source = source(op);
    let started = Instant::now();

    let error = source.execute("detached", &json!({})).await.unwrap_err();

    assert_eq!(error.kind(), "cli_timeout");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_process_group_children() {
    let tempdir = tempfile::tempdir().unwrap();
    let pid_file = tempdir.path().join("grandchild.pid");
    let mut op = operation(
        "tree",
        &[
            "-c",
            r#"import os, pathlib, subprocess, sys, time
subprocess.Popen([sys.executable, "-c", "import os, pathlib, time; pathlib.Path(os.environ['PID_FILE']).write_text(str(os.getpid())); time.sleep(5)"])
deadline = time.time() + 1
while not pathlib.Path(os.environ["PID_FILE"]).exists() and time.time() < deadline:
    time.sleep(0.01)
time.sleep(5)
"#,
        ],
    );
    op.timeout_ms = Some(500);
    op.env.insert(
        "PID_FILE".to_string(),
        pid_file.to_string_lossy().into_owned(),
    );
    let source = source(op);

    let error = source.execute("tree", &json!({})).await.unwrap_err();

    assert_eq!(error.kind(), "cli_timeout");
    let pid = wait_for_pid_file(&pid_file).await;
    for _ in 0..40 {
        if !pid_exists(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("grandchild process {pid} survived the adapter timeout");
}

#[tokio::test]
async fn stdout_and_stderr_are_capped_independently() {
    let mut op = operation(
        "caps",
        &[
            "-c",
            "import sys; print('x' * 1000); sys.stderr.write('y' * 1000)",
        ],
    );
    op.max_stdout_bytes = Some(16);
    op.max_stderr_bytes = Some(8);
    let source = source(op);

    let result = source.execute("caps", &json!({})).await.unwrap();

    assert!(result.provenance.stdout_truncated);
    assert!(result.provenance.stderr_truncated);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("stdout"))
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("stderr"))
    );
}

#[tokio::test]
async fn sensitive_args_are_redacted_from_provenance() {
    let mut op = operation(
        "secret",
        &["-c", "import sys; print('ok')", "--token", "{token}"],
    );
    op.sensitive_args = vec!["token".to_string()];
    let source = source(op);

    let result = source
        .execute("secret", &json!({"token": "SECRET_VALUE"}))
        .await
        .unwrap();

    let provenance = serde_json::to_string(&result.provenance).unwrap();
    assert!(!provenance.contains("SECRET_VALUE"));
    assert!(provenance.contains("[REDACTED]"));
}

#[tokio::test]
async fn declared_sensitive_arg_names_are_redacted_from_provenance_args() {
    // "service_key" is not a default secret keyword, so before the fix it was
    // redacted from argv (which consults sensitive_args) but leaked verbatim
    // into provenance.args, which is persisted to disk with the cache entry.
    let mut op = operation(
        "secret",
        &[
            "-c",
            "import sys; print('ok')",
            "--service-key",
            "{service_key}",
        ],
    );
    op.sensitive_args = vec!["service_key".to_string()];
    let source = source(op);

    let result = source
        .execute("secret", &json!({"service_key": "SK-LIVE-1234"}))
        .await
        .unwrap();

    assert_eq!(
        result.provenance.args["service_key"],
        json!("[REDACTED:declared_sensitive]")
    );
    let provenance = serde_json::to_string(&result.provenance).unwrap();
    assert!(!provenance.contains("SK-LIVE-1234"));
}

#[tokio::test]
async fn non_string_sensitive_args_are_redacted_from_argv() {
    let mut op = operation(
        "secret",
        &["-c", "import sys; print('ok')", "--pin", "{pin}"],
    );
    op.sensitive_args = vec!["pin".to_string()];
    let source = source(op);

    let result = source
        .execute("secret", &json!({"pin": 12345}))
        .await
        .unwrap();

    let provenance = serde_json::to_string(&result.provenance).unwrap();
    assert!(!provenance.contains("12345"));
    assert!(provenance.contains("[REDACTED]"));
}

#[tokio::test]
async fn shell_backed_operation_fails_closed_without_profile_trust() {
    let mut op = operation("shell", &["-c", "print('should not run')"]);
    op.shell = true;
    let source = source(op);

    let error = source.execute("shell", &json!({})).await.unwrap_err();

    assert_eq!(error.kind(), "shell_not_trusted");
}

#[test]
fn adapter_source_does_not_construct_shell_command_strings() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli.rs")).unwrap();
    let forbidden = ["sh", " -c"].concat();
    assert!(!source.contains(&forbidden));
}

fn source(operation: CliOperation) -> CliSource {
    CliSource {
        id: "local".to_string(),
        timeout_ms: 2_000,
        max_stdout_bytes: 1024 * 1024,
        max_stderr_bytes: 1024 * 1024,
        trust: TrustSettings::default(),
        operations: vec![operation],
    }
}

fn operation(id: &str, args: &[&str]) -> CliOperation {
    CliOperation {
        id: id.to_string(),
        input_schema: json!(null),
        command: "python3".to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        env: BTreeMap::new(),
        working_dir: None,
        shell: false,
        timeout_ms: None,
        max_stdout_bytes: None,
        max_stderr_bytes: None,
        sensitive_args: Vec::new(),
    }
}

#[cfg(unix)]
async fn wait_for_pid_file(path: &Path) -> u32 {
    for _ in 0..40 {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(pid) = contents.trim().parse()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("grandchild pid file was not written: {}", path.display());
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    !String::from_utf8_lossy(&output.stdout).trim().is_empty()
}
