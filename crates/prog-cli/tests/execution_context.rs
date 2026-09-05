//! Process-level regressions for shared-store execution context isolation.

use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    root: tempfile::TempDir,
    store: PathBuf,
    first: PathBuf,
    second: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        let first = root.path().join("first");
        let second = root.path().join("second");
        for dir in [&first, &second] {
            fs::create_dir(dir).unwrap();
        }
        Self {
            root,
            store,
            first,
            second,
        }
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        // Stable ambient inputs make a repeated invocation genuinely identical;
        // individual cases then change one explicit environment dependency.
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .args(["--dir", self.store.to_str().unwrap()]);
        command
    }

    fn add_cli(&self, argv: &[&str]) {
        success(
            self.command(&self.first)
                .args([
                    "source",
                    "add-cli",
                    "context",
                    "--operation",
                    "read",
                    "--read-only",
                    "--",
                ])
                .args(argv),
        );
    }

    fn edit_cli(&self, key: &str, value: Value) {
        let path = self.store.join("profiles/context.json");
        let mut profile: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        profile["operations"][0]["invocation"]["cli"][key] = value;
        fs::write(path, profile.to_string()).unwrap();
    }

    fn call(&self, cwd: &Path) -> Command {
        let mut command = self.command(cwd);
        command.args(["call", "context", "read", "--args", "{}"]);
        command
    }
}

fn success(command: &mut Command) -> Value {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_cwd(envelope: &Value, path: &Path, status: &str) {
    assert_eq!(envelope["cache"]["status"], status, "{envelope}");
    assert_eq!(
        envelope["data_preview"]["head"][0],
        path.canonicalize().unwrap().to_str().unwrap(),
        "{envelope}"
    );
}

#[test]
fn shared_store_scopes_unset_relative_and_absolute_working_directories() {
    let fixture = Fixture::new();
    fixture.add_cli(&["/bin/pwd"]);
    assert_cwd(
        &success(&mut fixture.call(&fixture.first)),
        &fixture.first,
        "stored",
    );
    assert_cwd(
        &success(&mut fixture.call(&fixture.second)),
        &fixture.second,
        "stored",
    );
    assert_cwd(
        &success(&mut fixture.call(&fixture.second)),
        &fixture.second,
        "hit",
    );

    for dir in [&fixture.first, &fixture.second] {
        fs::create_dir(dir.join("project")).unwrap();
    }
    fixture.edit_cli("working_dir", json!("project"));
    assert_cwd(
        &success(&mut fixture.call(&fixture.first)),
        &fixture.first.join("project"),
        "stored",
    );
    assert_cwd(
        &success(&mut fixture.call(&fixture.second)),
        &fixture.second.join("project"),
        "stored",
    );
    assert_cwd(
        &success(&mut fixture.call(&fixture.second)),
        &fixture.second.join("project"),
        "hit",
    );

    fixture.edit_cli("working_dir", json!(fixture.first));
    assert_cwd(
        &success(&mut fixture.call(&fixture.first)),
        &fixture.first,
        "stored",
    );
    assert_cwd(
        &success(&mut fixture.call(&fixture.second)),
        &fixture.first,
        "hit",
    );
}

#[test]
fn directory_aliases_resolve_before_identity_and_invalid_directories_fail() {
    let fixture = Fixture::new();
    fixture.add_cli(&["/bin/pwd"]);
    let shared = fixture.root.path().join("shared");
    fs::create_dir(&shared).unwrap();
    for dir in [&fixture.first, &fixture.second] {
        symlink(&shared, dir.join("alias")).unwrap();
    }
    fixture.edit_cli("working_dir", json!("alias"));
    assert_cwd(
        &success(&mut fixture.call(&fixture.first)),
        &shared,
        "stored",
    );
    assert_cwd(&success(&mut fixture.call(&fixture.second)), &shared, "hit");
    fs::remove_dir(&shared).unwrap();
    let missing = fixture.call(&fixture.second).output().unwrap();
    assert!(!missing.status.success());
    let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing["error"]["kind"], "io");
}

#[test]
fn inherited_environment_changes_and_configured_overrides_are_respected() {
    let fixture = Fixture::new();
    fixture.add_cli(&[
        "python3",
        "-c",
        "import os; print(os.getenv('PROG_CONTEXT_VALUE', 'missing'))",
    ]);
    for (value, status) in [
        ("context-A", "stored"),
        ("context-A", "hit"),
        ("context-B", "stored"),
        ("context-B", "hit"),
    ] {
        let result = success(
            fixture
                .call(&fixture.first)
                .env("PROG_CONTEXT_VALUE", value),
        );
        assert_eq!(result["cache"]["status"], status);
        assert_eq!(result["data_preview"]["head"][0], value);
    }
    let missing = success(&mut fixture.call(&fixture.first));
    assert_eq!(missing["data_preview"]["head"][0], "missing");
    assert_eq!(missing["cache"]["status"], "stored");
    let empty = success(fixture.call(&fixture.first).env("PROG_CONTEXT_VALUE", ""));
    assert_eq!(empty["cache"]["status"], "stored");
    assert_ne!(
        empty["observation"]["observation_id"],
        missing["observation"]["observation_id"]
    );
    for value in ["configured-A", "configured-B"] {
        fixture.edit_cli("env", json!({"PROG_CONTEXT_VALUE": value}));
        let result = success(
            fixture
                .call(&fixture.first)
                .env("PROG_CONTEXT_VALUE", "ambient"),
        );
        assert_eq!(result["data_preview"]["head"][0], value);
        assert_eq!(result["cache"]["status"], "stored");
        assert_eq!(
            success(
                fixture
                    .call(&fixture.first)
                    .env("PROG_CONTEXT_VALUE", "ambient")
            )["cache"]["status"],
            "hit"
        );
    }
}

#[test]
fn non_utf8_environment_values_do_not_alias_or_change_during_execution() {
    let fixture = Fixture::new();
    fixture.add_cli(&[
        "python3",
        "-c",
        "import os; print(os.environb[b'PROG_CONTEXT_VALUE'].hex())",
    ]);
    for (bytes, expected, status) in [
        (vec![0xff], "ff", "stored"),
        (vec![0xfe], "fe", "stored"),
        (vec![0xfe], "fe", "hit"),
    ] {
        let result = success(
            fixture
                .call(&fixture.first)
                .env("PROG_CONTEXT_VALUE", OsString::from_vec(bytes)),
        );
        assert_eq!(result["data_preview"]["head"][0], expected);
        assert_eq!(result["cache"]["status"], status);
    }
}

fn executable(dir: &Path, name: &str, marker: &str) {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{marker}'\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn path_search_and_relative_executables_use_the_resolved_child_context() {
    let fixture = Fixture::new();
    let first_bin = fixture.first.join("bin");
    let second_bin = fixture.second.join("bin");
    executable(&first_bin, "context-fixture", "first");
    executable(&second_bin, "context-fixture", "second");
    fixture.add_cli(&["context-fixture"]);
    for (path, expected, status) in [
        (&first_bin, "first", "stored"),
        (&second_bin, "second", "stored"),
        (&second_bin, "second", "hit"),
    ] {
        let result = success(fixture.call(&fixture.first).env("PATH", path));
        assert_eq!(result["data_preview"]["head"][0], expected);
        assert_eq!(result["cache"]["status"], status);
    }
    // Relative PATH entries resolve inside the configured child directory.
    fixture.edit_cli("working_dir", json!(fixture.second));
    let child_lookup = success(fixture.call(&fixture.first).env("PATH", "bin"));
    assert_eq!(child_lookup["data_preview"]["head"][0], "second");
    fixture.edit_cli("working_dir", Value::Null);
    fixture.edit_cli("command", json!("./bin/context-fixture"));
    for (cwd, expected) in [(&fixture.first, "first"), (&fixture.second, "second")] {
        let result = success(&mut fixture.call(cwd));
        assert_eq!(result["data_preview"]["head"][0], expected);
        assert_eq!(result["cache"]["status"], "stored");
    }
}

#[test]
fn secret_environment_values_never_enter_output_errors_or_persisted_metadata() {
    let fixture = Fixture::new();
    fixture.add_cli(&["/bin/pwd"]);
    let secrets = ["fake-context-secret-alpha", "fake-context-secret-bravo"];
    for secret in secrets {
        let result = success(
            fixture
                .call(&fixture.first)
                .env("PROG_CACHE_TEST_SECRET", secret),
        );
        assert_eq!(result["cache"]["status"], "stored");
        assert!(!result.to_string().contains(secret));
        assert!(!result.to_string().contains("process_context"));
    }
    fixture.edit_cli("command", json!("/nonexistent/prog-context-test"));
    let failed = fixture
        .call(&fixture.first)
        .env("PROG_CACHE_TEST_SECRET", secrets[0])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    for bytes in [&failed.stdout, &failed.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(secrets[0]));
    }
    fn inspect_files(path: &Path, secrets: &[&str]) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                inspect_files(&path, secrets);
            } else {
                let bytes = fs::read(&path).unwrap();
                for secret in secrets {
                    assert!(
                        !bytes
                            .windows(secret.len())
                            .any(|window| window == secret.as_bytes()),
                        "secret leaked into {}",
                        path.display()
                    );
                }
            }
        }
    }
    inspect_files(&fixture.store, &secrets);
}

#[test]
fn mcp_stdio_tools_and_resources_share_the_context_isolation_contract() {
    let fixture = Fixture::new();
    let script = fixture.root.path().join("context_mcp.py");
    fs::write(&script, r#"import json, os, sys

def reply(request, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        reply(request, {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "resources": {}}, "serverInfo": {"name": "context-fixture", "version": "1"}})
    elif method == "tools/list":
        reply(request, {"tools": [{"name": "context", "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": True}}]})
    elif method == "resources/list":
        reply(request, {"resources": [{"uri": "context://value", "name": "context", "mimeType": "application/json"}]})
    elif method == "resources/templates/list":
        reply(request, {"resourceTemplates": []})
    elif method in ("tools/call", "resources/read"):
        value = {"cwd": os.getcwd(), "context": os.getenv("PROG_CONTEXT_VALUE", "missing")}
        if method == "tools/call":
            reply(request, {"structuredContent": value, "content": [], "isError": False})
        else:
            reply(request, {"contents": [{"uri": "context://value", "mimeType": "application/json", "text": json.dumps(value)}]})
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "unknown method"}}), flush=True)
"#).unwrap();
    success(fixture.command(&fixture.first).args([
        "source",
        "add-mcp",
        "mcp-context",
        "--",
        "python3",
        script.to_str().unwrap(),
    ]));
    for (operation, args) in [
        ("context", "{}"),
        ("resource:context", r#"{"uri":"context://value"}"#),
    ] {
        for (cwd, context, status) in [
            (&fixture.first, "context-A", "stored"),
            (&fixture.second, "context-A", "stored"),
            (&fixture.second, "context-B", "stored"),
            (&fixture.second, "context-B", "hit"),
        ] {
            let result = success(
                fixture
                    .command(cwd)
                    .env("PROG_CONTEXT_VALUE", context)
                    .args(["call", "mcp-context", operation, "--args", args]),
            );
            assert_eq!(result["cache"]["status"], status, "{result}");
            assert_eq!(
                result["data_preview"]["cwd"],
                cwd.canonicalize().unwrap().to_str().unwrap(),
                "{result}"
            );
            assert_eq!(result["data_preview"]["context"], context, "{result}");
        }
    }
}
