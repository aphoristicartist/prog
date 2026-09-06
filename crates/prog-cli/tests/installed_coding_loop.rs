//! A real installed skill/CLI failure-to-verification workflow outside checkout.

use std::{path::Path, process::Command};

use serde_json::Value;

#[test]
fn installed_coding_loop_preserves_evidence_and_refuses_unearned_readiness() {
    let ambient = tempfile::tempdir().unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/harness/installed_coding_loop.py");
    let output = Command::new("python3")
        .arg(script)
        .args(["--prog", env!("CARGO_BIN_EXE_prog")])
        .env("GIT_DIR", ambient.path().join("redirected.git"))
        .env("PROG_DIR", ambient.path().join("redirected-store"))
        .env("PROG_LENS_DIR", ambient.path().join("absent-lenses"))
        .output()
        .expect("installed-loop driver should run");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], "prog.installed_coding_loop_smoke");
    assert_eq!(report["passed"], true);
    assert_eq!(report["temporary_store_retained"], false);
    assert_ne!(
        report["baseline_observation_id"],
        report["verification_observation_id"]
    );
    assert_eq!(report["verified_status"]["readiness"]["ready"], true);
    for name in ["narrow", "stale", "incomplete", "evicted"] {
        assert_eq!(
            report["negative_controls"][name]["readiness"]["ready"],
            false
        );
    }
    assert_eq!(report["test_executions"].as_array().unwrap().len(), 7);
    assert_eq!(std::fs::read_dir(ambient.path()).unwrap().count(), 0);
}
