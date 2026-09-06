#![cfg(unix)]

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};

use prog_core::Store;
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};

mod support;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn store(&self) -> std::path::PathBuf {
        self.0.path().join("store")
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .current_dir(self.0.path())
            .arg("--dir")
            .arg(self.store())
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
    fn empty_store(&self) {
        let store = Store::open(self.store()).unwrap();
        assert!(
            store
                .list_observations(100)
                .unwrap()
                .observations
                .is_empty()
        );
        assert!(store.list_entries(100).unwrap().entries.is_empty());
    }
    fn no_persisted_secret(&self, secret: &[u8]) {
        fn check(path: &Path, secret: &[u8]) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    check(&path, secret);
                } else {
                    assert!(
                        !fs::read(path)
                            .unwrap()
                            .windows(secret.len())
                            .any(|w| w == secret)
                    );
                }
            }
        }
        check(&self.store(), secret);
    }
}

fn pipe() -> (File, File) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: successful pipe created two separately owned valid descriptors.
    let read = unsafe { File::from_raw_fd(fds[0]) };
    let write = unsafe { File::from_raw_fd(fds[1]) };
    for fd in fds {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    (read, write)
}

fn flags(file: &File) -> i32 {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    flags
}

async fn finish(mut child: Child) -> Output {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let err = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(result) => result.unwrap(),
        Err(_) => {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            out.await.unwrap();
            err.await.unwrap();
            panic!("observe exceeded the independent guard deadline");
        }
    };
    Output {
        status,
        stdout: out.await.unwrap(),
        stderr: err.await.unwrap(),
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn rejected(output: &Output, reason: &str, captured: Option<u64>) -> Value {
    assert!(!output.status.success());
    assert!(output.stdout.len() <= 16 * 1024);
    let value = json(output);
    assert_eq!(value["error"]["kind"], "capture_stopped", "{value}");
    let capture = &value["error"]["capture"];
    assert_eq!(capture["stop_reason"], reason);
    assert_eq!(capture["total_bytes"], Value::Null);
    assert_eq!(capture["stored_bytes"], 0);
    assert_eq!(capture["can_prove_absence"], false);
    assert_eq!(capture["affected"][0]["total_bytes"], Value::Null);
    if let Some(captured) = captured {
        assert_eq!(capture["captured_bytes"], captured);
    }
    assert!(value.get("cursor").is_none());
    assert!(value.get("observation").is_none());
    assert_eq!(
        value["disclosure_budget"]["actual_bytes"],
        output.stdout.len()
    );
    value
}

async fn finite_input(f: &Fixture, bytes: &[u8], file: bool, cap: u64, mime: &str) -> Output {
    let mut command = f.command(&[
        "observe",
        "--max-input-bytes",
        &cap.to_string(),
        "--mime",
        mime,
    ]);
    if file {
        let path = f.0.path().join("input");
        fs::write(&path, bytes).unwrap();
        command.arg("--file").arg(path);
        finish(command.spawn().unwrap()).await
    } else {
        // All finite fixtures fit in this pipe before spawn; no writer process
        // or blocking writer thread can survive a failed assertion.
        assert!(bytes.len() < 8192);
        let (read, mut write) = pipe();
        write.write_all(bytes).unwrap();
        drop(write);
        let original_flags = flags(&read);
        let output = finish(
            command
                .arg("--stdin")
                .stdin(Stdio::from(read.try_clone().unwrap()))
                .spawn()
                .unwrap(),
        )
        .await;
        assert_eq!(
            flags(&read),
            original_flags,
            "inherited stdin flags must be restored"
        );
        output
    }
}

#[tokio::test]
async fn file_and_stdin_boundaries_preserve_complete_evidence_and_reject_overflow() {
    for file in [true, false] {
        let f = Fixture::new();
        let bytes = b"exact evidence";
        let accepted = finite_input(&f, bytes, file, bytes.len() as u64, "text/plain").await;
        assert!(
            accepted.status.success(),
            "{}",
            String::from_utf8_lossy(&accepted.stdout)
        );
        let value = json(&accepted);
        let capture = &value["observation"]["capture"];
        assert_eq!(capture["captured_bytes"], bytes.len());
        assert_eq!(capture["total_bytes"], bytes.len());
        assert_eq!(capture["stored_bytes"], value["summary"]["payload_bytes"]);
        assert_ne!(capture["stored_bytes"], capture["captured_bytes"]);
        assert_eq!(capture["budget"]["limits"][0]["max_bytes"], bytes.len());
        assert_eq!(
            value["disclosure_verdict"]["baseline"]["bytes"],
            bytes.len()
        );
        let expanded = finish(
            f.command(&[
                "expand",
                value["cursor"].as_str().unwrap(),
                "--path",
                "/lines/0/text",
            ])
            .spawn()
            .unwrap(),
        )
        .await;
        assert!(expanded.status.success());
        assert_eq!(json(&expanded)["data_preview"], "exact evidence");
        let store = Store::open(f.store()).unwrap();
        let before = serde_json::to_value(store.list_observations(100).unwrap()).unwrap();
        store.release().unwrap();
        rejected(
            &finite_input(&f, bytes, file, bytes.len() as u64 - 1, "text/plain").await,
            "byte_limit",
            Some(bytes.len() as u64),
        );
        assert_eq!(
            serde_json::to_value(store.list_observations(100).unwrap()).unwrap(),
            before,
            "rejected input must not create or replace an observation"
        );
    }
}

#[tokio::test]
async fn rejected_json_and_ndjson_are_not_normalized_or_persisted() {
    let secret = "sk-test-rejected-artifact-never-stored-0123456789";
    let cases = [
        (
            format!("\"{}{}\"", secret, "x".repeat(2048)),
            "application/json",
        ),
        (
            format!("{{\"status\":\"passed\"}}\n{{\"token\":\"{secret}\"}}\n"),
            "application/x-ndjson",
        ),
    ];
    for (bytes, mime) in cases {
        for file in [true, false] {
            let f = Fixture::new();
            let output = finite_input(&f, bytes.as_bytes(), file, 25, mime).await;
            rejected(&output, "byte_limit", Some(26));
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
            f.empty_store();
            f.no_persisted_secret(secret.as_bytes());
        }
    }
}

#[tokio::test]
async fn probe_consumes_only_one_extra_byte_and_restores_inherited_flags() {
    let f = Fixture::new();
    let (mut read, mut write) = pipe();
    let original_flags = flags(&read);
    let bytes = b"0123456789abcdefghijklmnopqrstuvwxyz";
    write.write_all(bytes).unwrap();
    drop(write);
    let output = finish(
        f.command(&["observe", "--stdin", "--max-input-bytes", "8"])
            .stdin(Stdio::from(read.try_clone().unwrap()))
            .spawn()
            .unwrap(),
    )
    .await;
    rejected(&output, "byte_limit", Some(9));
    assert_eq!(flags(&read), original_flags);
    let mut remaining = Vec::new();
    read.read_to_end(&mut remaining).unwrap();
    assert_eq!(remaining, bytes[9..]);
    f.empty_store();
}

#[tokio::test]
async fn finite_utf8_invalid_utf8_and_empty_artifacts_keep_their_parser_semantics() {
    for file in [true, false] {
        for (bytes, expected, valid) in [
            (b"caf\xc3\xa9".as_slice(), "café", true),
            (b"a\xffb".as_slice(), "a�b", false),
        ] {
            let f = Fixture::new();
            let output = finite_input(&f, bytes, file, bytes.len() as u64, "text/plain").await;
            assert!(output.status.success());
            let value = json(&output);
            assert_eq!(value["data_preview"]["utf8_valid"], valid);
            let expanded = finish(
                f.command(&[
                    "expand",
                    value["cursor"].as_str().unwrap(),
                    "--path",
                    "/lines/0/text",
                ])
                .spawn()
                .unwrap(),
            )
            .await;
            assert_eq!(json(&expanded)["data_preview"], expected);
        }
        let f = Fixture::new();
        assert!(
            finite_input(&f, b"", file, 0, "text/plain")
                .await
                .status
                .success()
        );
        rejected(
            &finite_input(&Fixture::new(), b"x", file, 0, "text/plain").await,
            "byte_limit",
            Some(1),
        );
    }
}

#[tokio::test]
async fn stalled_stdin_and_exact_cap_without_eof_time_out_without_persistence() {
    for cap in [4, 128] {
        let f = Fixture::new();
        let (read, mut producer) = pipe();
        producer.write_all(b"test").unwrap();
        let original_flags = flags(&read);
        let output = finish(
            f.command(&[
                "observe",
                "--stdin",
                "--max-input-bytes",
                &cap.to_string(),
                "--timeout-ms",
                "250",
            ])
            .stdin(Stdio::from(read.try_clone().unwrap()))
            .spawn()
            .unwrap(),
        )
        .await;
        let value = rejected(&output, "timeout", Some(4));
        assert_eq!(value["capture_budget"]["limits"][0]["max_duration_ms"], 250);
        assert_eq!(flags(&read), original_flags);
        f.empty_store();
        drop(producer); // Still open when observe returned: EOF did not end capture.
    }
}

#[tokio::test]
async fn sigint_and_sigterm_cancel_stalled_stdin_and_restore_flags() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new();
        let (read, producer) = pipe();
        let original_flags = flags(&read);
        let mut child = f
            .command(&["observe", "--stdin", "--timeout-ms", "20000"])
            .stdin(Stdio::from(read.try_clone().unwrap()))
            .spawn()
            .unwrap();
        // The shared open-file description proves the reader installed its
        // signal handlers and entered acquisition; no startup timing guess.
        let ready = tokio::time::timeout(Duration::from_secs(4), async {
            while flags(&read) & libc::O_NONBLOCK == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        if ready.is_err() {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            panic!("reader never became ready");
        }
        assert_eq!(unsafe { libc::kill(child.id().unwrap() as i32, signal) }, 0);
        let output = finish(child).await;
        assert_eq!(
            rejected(&output, "cancelled", Some(0))["error"]["capture"]["signal"],
            signal
        );
        assert_eq!(flags(&read), original_flags);
        f.empty_store();
        drop(producer);
    }
}

#[tokio::test]
async fn file_fifos_are_rejected_without_waiting_for_a_writer() {
    let f = Fixture::new();
    let path = f.0.path().join("fifo");
    let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let output = finish(
        f.command(&[
            "observe",
            "--file",
            path.to_str().unwrap(),
            "--timeout-ms",
            "100",
        ])
        .spawn()
        .unwrap(),
    )
    .await;
    assert!(!output.status.success());
    assert_eq!(json(&output)["error"]["kind"], "bad_args");
    f.empty_store();
}

#[tokio::test]
async fn always_readable_stdin_device_stops_at_the_cap() {
    let f = Fixture::new();
    let output = finish(
        f.command(&["observe", "--stdin", "--max-input-bytes", "32"])
            .stdin(Stdio::from(File::open("/dev/zero").unwrap()))
            .spawn()
            .unwrap(),
    )
    .await;
    rejected(&output, "byte_limit", Some(33));
    f.empty_store();
}

#[tokio::test]
async fn help_and_public_error_schema_describe_the_capture_limits() {
    let f = Fixture::new();
    let help = finish(f.command(&["observe", "--help"]).spawn().unwrap()).await;
    let help = String::from_utf8(help.stdout).unwrap();
    for term in [
        "--max-input-bytes",
        "16777216",
        "--timeout-ms",
        "30000",
        "EOF",
    ] {
        assert!(help.contains(term), "missing {term}");
    }
    let schemas = prog_core::public_contract_schemas().unwrap();
    assert!(schemas["ErrorBody"]["properties"].get("capture").is_some());
    let meta = finish(f.command(&["meta", "ErrorBody"]).spawn().unwrap()).await;
    assert!(meta.status.success());
    assert!(String::from_utf8(meta.stdout).unwrap().contains("capture"));
    let invalid = finish(
        f.command(&["observe", "--stdin", "--timeout-ms", "0"])
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    )
    .await;
    assert!(!invalid.status.success());
    assert_eq!(json(&invalid)["error"]["kind"], "cli_usage");
}

#[tokio::test]
async fn redirected_regular_file_stdin_preserves_flags_and_redacted_evidence() {
    let f = Fixture::new();
    let path = f.0.path().join("redirected.json");
    let bytes = br#"{"value":"kept","password":"never-store-this-secret"}"#;
    fs::write(&path, bytes).unwrap();
    let input = File::open(path).unwrap();
    let original_flags = flags(&input);
    let output = finish(
        f.command(&[
            "observe",
            "--stdin",
            "--mime",
            "application/json",
            "--max-input-bytes",
            &bytes.len().to_string(),
        ])
        .stdin(Stdio::from(input.try_clone().unwrap()))
        .spawn()
        .unwrap(),
    )
    .await;
    assert!(output.status.success());
    assert_eq!(flags(&input), original_flags);
    let value = json(&output);
    assert_eq!(
        value["observation"]["capture"]["captured_bytes"],
        bytes.len()
    );
    assert_eq!(value["observation"]["capture"]["can_prove_absence"], false);
    let expanded = finish(
        f.command(&[
            "expand",
            value["cursor"].as_str().unwrap(),
            "--path",
            "/value",
        ])
        .spawn()
        .unwrap(),
    )
    .await;
    assert_eq!(json(&expanded)["data_preview"], "kept");
    f.no_persisted_secret(b"never-store-this-secret");
}

#[tokio::test]
async fn disclosure_budgets_do_not_change_the_input_cap() {
    for budget in [4096, 32768] {
        let f = Fixture::new();
        let path = f.0.path().join("input");
        fs::write(&path, b"0123456789").unwrap();
        let output = finish(
            f.command(&[
                "--budget-bytes",
                &budget.to_string(),
                "observe",
                "--file",
                path.to_str().unwrap(),
                "--max-input-bytes",
                "4",
            ])
            .spawn()
            .unwrap(),
        )
        .await;
        rejected(&output, "byte_limit", Some(5));
        assert!(output.stdout.len() <= budget);
        f.empty_store();
    }
}

#[tokio::test]
async fn file_and_report_recipes_apply_explicit_acquisition_limits() {
    let lens = support::first_party_lens_dir();
    let f = Fixture::new();
    let path = f.0.path().join("input.log");
    fs::write(&path, b"ERROR something failed\n").unwrap();
    let output = finish(
        f.command(&[
            "--lens-dir",
            lens.to_str().unwrap(),
            "recipe",
            "logs-root-cause",
            "--file",
            path.to_str().unwrap(),
            "--max-input-bytes",
            "8",
            "--timeout-ms",
            "5000",
        ])
        .spawn()
        .unwrap(),
    )
    .await;
    rejected(&output, "byte_limit", Some(9));
    f.empty_store();

    let f = Fixture::new();
    let reporter = support::repo_root().join("fixtures/cli/modern_reporter.py");
    let output = finish(
        f.command(&[
            "--lens-dir",
            lens.to_str().unwrap(),
            "recipe",
            "vitest",
            "--max-input-bytes",
            "8",
            "--timeout-ms",
            "5000",
            "--",
            "python3",
            reporter.to_str().unwrap(),
            "vitest",
        ])
        .spawn()
        .unwrap(),
    )
    .await;
    rejected(&output, "byte_limit", Some(9));
    let store = Store::open(f.store()).unwrap();
    let observations = store.list_observations(100).unwrap().observations;
    assert_eq!(
        observations.len(),
        1,
        "only the preceding command was captured"
    );
    assert_eq!(observations[0].source_id, "run");
    // The command's persisted argv records the generated path even though
    // observe rejected it; use that evidence to verify temporary-file cleanup.
    let payload = store
        .get_payload(&observations[0].payload_hash)
        .unwrap()
        .unwrap();
    let argv = payload.as_value()["command"]["argv"].as_array().unwrap();
    let report = argv
        .iter()
        .filter_map(Value::as_str)
        .find_map(|arg| arg.strip_prefix("--outputFile="))
        .unwrap();
    assert!(!Path::new(report).exists());
    assert!(!Path::new(report).parent().unwrap().exists());
}

#[tokio::test]
async fn default_file_cap_is_enforced_before_normalization() {
    let f = Fixture::new();
    let path = f.0.path().join("sparse-input");
    let file = File::create(&path).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    let output = finish(
        f.command(&["observe", "--file", path.to_str().unwrap()])
            .spawn()
            .unwrap(),
    )
    .await;
    let value = rejected(&output, "byte_limit", Some(16 * 1024 * 1024 + 1));
    assert_eq!(value["capture_budget"]["source"], "default");
    f.empty_store();
}
