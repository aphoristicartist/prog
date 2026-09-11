//! End-to-end negative controls for coding-provider completion evidence (#281).

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use serde_json::{Value, json};

mod support;
use support::{prog, stdout};

fn run_json(args: &[&str]) -> Value {
    let output = prog(args);
    assert!(output.status.success(), "{}", stdout(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn coding_readiness_requires_consistent_and_exhaustive_evidence() {
    let mut cases = vec![
        (
            "early-stop",
            "pytest",
            vec!["-x"],
            "1 passed in 0.01s\n".to_string(),
        ),
        (
            "no-tests",
            "pytest",
            vec![],
            "no tests ran in 0.01s\n".to_string(),
        ),
        (
            "conflicting-json-summary",
            "pytest",
            vec![],
            json!({
                "exitcode": 0,
                "summary": {"passed": 1, "failed": 1, "total": 2},
                "tests": [{"nodeid": "test_api.py::test_ok", "outcome": "passed"}]
            })
            .to_string(),
        ),
        (
            "conflicting-text-summary",
            "pytest",
            vec![],
            "FAILED test_api.py::test_bad - AssertionError\n1 passed in 0.01s\n".to_string(),
        ),
        (
            "empty-cargo-selection",
            "cargo",
            vec!["test"],
            "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 3 filtered out\n"
                .to_string(),
        ),
    ];
    let green = json!({
        "exitcode": 0, "summary": {"passed": 1, "total": 1},
        "tests": [{"nodeid": "test_api.py::test_ok", "outcome": "passed"}]
    });
    let mut collectors = green.clone();
    collectors["collectors"] =
        json!([{"nodeid": "test_broken.py", "outcome": "failed", "longrepr": "SyntaxError"}]);
    let mut teardown = green.clone();
    teardown["tests"][0]["teardown"] =
        json!({"outcome": "failed", "longrepr": "fixture cleanup failed"});
    cases.extend([
        ("collection-error", "pytest", vec![], collectors.to_string()),
        ("teardown-conflict", "pytest", vec![], teardown.to_string()),
        ("late-json-failure", "pytest", vec![], format!("{green}\nFAILED test_api.py::test_late - AssertionError\n")),
        ("json-skipped", "pytest", vec![], json!({"exitcode": 0, "summary": {"skipped": 1, "total": 1}, "tests": [{"nodeid": "test_api.py::test_skip", "outcome": "skipped"}]}).to_string()),
        ("deselected", "pytest", vec![], "1 passed, 2 deselected in 0.01s\n".to_string()),
        ("duplicate-summary", "pytest", vec![], "1 passed in 0.01s\n1 passed in 0.02s\n".to_string()),
        ("early-stop-override", "pytest", vec!["-x"], "1 passed in 0.01s\n".to_string()),
        ("cargo-conflict", "cargo", vec!["test"], "test module::bad ... FAILED\ntest result: ok. 1 passed; 0 failed\n".to_string()),
        ("cargo-malformed-summary", "cargo", vec!["test"], "test result: ok. unknown passed; 0 failed\n".to_string()),
        ("valid-pytest-text", "pytest", vec![], "test_api.py::test_ok PASSED\n1 passed in 0.01s\n".to_string()),
        ("valid-pytest-json", "pytest", vec![], green.to_string()),
        ("valid-cargo", "cargo", vec!["test"], "test module::good ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n".to_string()),
    ]);
    let mut false_successes = Vec::new();
    for (name, program, args, text) in cases {
        let directory = workspace();
        let dir = directory.path().to_str().unwrap();
        let executable = directory.path().join(program);
        let script = format!(
            "#!/usr/bin/env python3\nimport sys\nwith open('executions', 'a') as trace: trace.write('run\\n')\nsys.stdout.write({})\n",
            serde_json::to_string(&text).unwrap()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let expected_ready = name.starts_with("valid-");
        let mut command = vec!["--dir", dir, "run"];
        if name.ends_with("-override") {
            command.extend(["--selection-scope", "suite", "--selection-exhaustive"]);
        }
        command.extend(["--", executable.to_str().unwrap()]);
        command.extend(args);
        let captured = run_json(&command);
        let id = captured["observation"]["observation_id"].as_str().unwrap();
        let cursor = captured["cursor"].as_str().unwrap();
        if matches!(name, "collection-error" | "teardown-conflict") {
            let diagnostic = captured["findings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|finding| {
                    finding["severity"] == "error"
                        && finding["path"].as_str().is_some_and(|path| {
                            path.starts_with("/provider/normalized/diagnostics/")
                        })
                })
                .unwrap_or_else(|| panic!("missing visible failure for {name}: {captured}"));
            let evidence = run_json(&[
                "--dir",
                dir,
                "evidence",
                cursor,
                "--path",
                diagnostic["path"].as_str().unwrap(),
            ]);
            assert_eq!(evidence["evidence_ref"]["cursor"], cursor);
            assert_eq!(evidence["evidence_ref"]["availability"], "recoverable");
        }
        let exported = directory.path().join("stdout.json");
        run_json(&[
            "--dir",
            dir,
            "expand",
            cursor,
            "--path",
            "/stdout/text",
            "--out",
            exported.to_str().unwrap(),
        ]);
        let exact: Value = serde_json::from_slice(&fs::read(exported).unwrap()).unwrap();
        assert_eq!(exact, text.trim_end(), "{name}");
        run_json(&[
            "--dir",
            dir,
            "session",
            "obligation-add",
            name,
            "--check",
            "verify the complete selected test suite",
            "--scope",
            "suite",
            "--evidence-observation",
            id,
        ]);
        let readiness = run_json(&["--dir", dir, "session", "show", "--readiness"]);
        assert_ne!(
            readiness["evaluations"][0]["status"], "stale",
            "{name}: {readiness}"
        );
        if readiness["ready"] != expected_ready
            || (readiness["evaluations"][0]["status"] == "passed") != expected_ready
        {
            false_successes.push(format!("{name}: {readiness}"));
        }
        assert_eq!(
            fs::read_to_string(directory.path().join("executions")).unwrap(),
            "run\n",
            "offline reads must not rerun {name}"
        );
    }
    assert!(false_successes.is_empty(), "{}", false_successes.join("\n"));
}

fn workspace() -> tempfile::TempDir {
    let directory = support::test_git_repo();
    let dir = directory.path().to_str().unwrap();
    fs::write(directory.path().join(".gitignore"), "*\n!.gitignore\n").unwrap();
    for args in [
        vec!["add", ".gitignore"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    directory
}

#[test]
fn coding_delta_never_resolves_from_empty_conflicting_or_late_failure_evidence() {
    for (name, text, stderr, code, absence_proven) in [
        ("empty", "no tests ran in 0.01s", "", 0, false),
        (
            "reported-success-process-failure",
            "1 passed in 0.01s",
            "",
            2,
            false,
        ),
        (
            "late-failure",
            "1 passed in 0.01s",
            "FAILED test_api.py::test_late - AssertionError",
            0,
            false,
        ),
        ("skipped", "1 passed, 1 skipped in 0.01s", "", 0, false),
        ("completed", "1 passed in 0.01s", "", 0, true),
    ] {
        let directory = workspace();
        let dir = directory.path().to_str().unwrap();
        let executable = directory.path().join("pytest");
        let mut observations = Vec::new();
        for (out, err, exit) in [
            (
                "FAILED test_api.py::test_original - AssertionError\n1 failed in 0.01s",
                "",
                1,
            ),
            (text, stderr, code),
        ] {
            let script = format!(
                "#!/usr/bin/env python3\nimport sys\nsys.stdout.write({})\nsys.stderr.write({})\nsys.exit({exit})\n",
                serde_json::to_string(out).unwrap(),
                serde_json::to_string(err).unwrap(),
            );
            fs::write(&executable, script).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let captured = run_json(&[
                "--dir",
                dir,
                "run",
                "--comparison-family",
                "suite",
                "--",
                executable.to_str().unwrap(),
            ]);
            observations.push(captured);
        }
        let baseline = observations[0]["observation"]["observation_id"]
            .as_str()
            .unwrap();
        let subject = observations[1]["observation"]["observation_id"]
            .as_str()
            .unwrap();
        let delta = run_json(&["--dir", dir, "delta", baseline, subject]);
        assert_eq!(
            delta["assessment"]["can_prove_absence"], absence_proven,
            "{name}: {delta}"
        );
        assert_eq!(
            delta["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| finding["status"] == "resolved"),
            absence_proven,
            "{name}: {delta}"
        );
        // The retained subject remains available after offline comparison.
        let cursor = observations[1]["cursor"].as_str().unwrap();
        let expanded = run_json(&["--dir", dir, "expand", cursor, "--path", "/stdout/text"]);
        assert_eq!(expanded["data_preview"], text, "{name}");
        if !stderr.is_empty() {
            let expanded = run_json(&["--dir", dir, "expand", cursor, "--path", "/stderr/text"]);
            assert_eq!(expanded["data_preview"], stderr, "{name}");
        }
    }
}
