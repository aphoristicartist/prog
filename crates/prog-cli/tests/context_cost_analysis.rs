//! Offline analysis stays executable under the workspace test gate (#280).

use std::{path::Path, process::Command};

#[test]
fn offline_context_cost_analysis_preserves_accounting_and_privacy() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("python3")
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "fixtures/harness",
            "-p",
            "test_context_cost_analysis.py",
        ])
        .current_dir(root)
        .output()
        .expect("offline analyzer tests should run");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
