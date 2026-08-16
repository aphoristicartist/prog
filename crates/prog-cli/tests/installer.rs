#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

mod support;

use support::{repo_root, stderr, stdout};

const FIXTURE_VERSION: &str = "9.9.9";

struct ReleaseFixture {
    _root: tempfile::TempDir,
    release_dir: PathBuf,
    fake_bin: PathBuf,
    home_dir: PathBuf,
    install_dir: PathBuf,
    target: &'static str,
    archive: PathBuf,
}

impl ReleaseFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let release_dir = root.path().join("release");
        let staging = release_dir.join(format!("prog-{}", target()));
        let fake_bin = root.path().join("fake-bin");
        let home_dir = root.path().join("home");
        let install_dir = root.path().join("installed");
        fs::create_dir_all(&staging).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::create_dir_all(&home_dir).unwrap();

        let fake_prog = staging.join("prog");
        fs::write(
            &fake_prog,
            format!(
                "#!/bin/sh\nif [ \"${{1:-}}\" = \"--version\" ]; then printf 'prog {FIXTURE_VERSION}\\n'; else printf '{{\"fixture\":true}}\\n'; fi\n"
            ),
        )
        .unwrap();
        executable(&fake_prog);
        fs::write(staging.join("VERSION"), format!("{FIXTURE_VERSION}\n")).unwrap();
        fs::write(staging.join("TARGET"), format!("{}\n", target())).unwrap();

        let archive = release_dir.join(format!("prog-{}.tar.gz", target()));
        let output = Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&release_dir)
            .arg(format!("prog-{}", target()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));

        let hash = hex_sha256(&fs::read(&archive).unwrap());
        fs::write(
            release_dir.join("SHA256SUMS"),
            format!(
                "{hash}  {}\n",
                archive.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();

        let fake_gh = fake_bin.join("gh");
        fs::write(
            &fake_gh,
            "#!/bin/sh\n[ \"${PROG_TEST_ATTEST_FAIL:-0}\" = 0 ] || exit 9\n[ \"${1:-}\" = attestation ]\n[ \"${2:-}\" = verify ]\n[ -f \"${3:-}\" ]\n",
        )
        .unwrap();
        executable(&fake_gh);

        Self {
            _root: root,
            release_dir,
            fake_bin,
            home_dir,
            install_dir,
            target: target(),
            archive,
        }
    }

    fn installer(&self) -> Command {
        let mut command = Command::new("sh");
        command
            .arg(repo_root().join("install.sh"))
            .env("PROG_RELEASE_URL", file_url(&self.release_dir))
            .env("PROG_ALLOW_FILE_URL", "1")
            .env("PROG_TARGET", self.target)
            .env("PROG_INSTALL_DIR", &self.install_dir)
            .env("HOME", &self.home_dir)
            .env("SHELL", "/bin/sh")
            .env("PATH", path_with(&self.fake_bin));
        command
    }

    fn updater(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .args([
                "update",
                "--yes",
                "--target-version",
                "v9.9.9",
                "--install-dir",
            ])
            .arg(&self.install_dir)
            .env(
                "PROG_UPDATE_INSTALLER_URL",
                file_url(&repo_root().join("install.sh")),
            )
            .env("PROG_RELEASE_URL", file_url(&self.release_dir))
            .env("PROG_ALLOW_FILE_URL", "1")
            .env("PROG_TARGET", self.target)
            .env("HOME", &self.home_dir)
            .env("SHELL", "/bin/sh")
            .env("PATH", path_with(&self.fake_bin));
        command
    }
}

#[test]
fn curl_installer_verifies_and_installs_a_supported_release() {
    let fixture = ReleaseFixture::new();
    let output = fixture.installer().output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(fixture.install_dir.join("prog").is_file());
    assert_eq!(
        fs::read_to_string(fixture.install_dir.join(".prog-install")).unwrap(),
        format!(
            "repository=aphoristicartist/prog\nversion={FIXTURE_VERSION}\ntarget={}\n",
            fixture.target
        )
    );
    assert!(stderr(&output).contains("checksum and GitHub attestation verified"));
    assert_eq!(
        fs::read_to_string(fixture.home_dir.join(".profile")).unwrap(),
        format!(
            "# Added by the prog installer.\nexport PATH='{}':\"$PATH\"\n",
            fixture.install_dir.display()
        )
    );
    assert!(stderr(&output).contains("Open a new terminal to use prog by name"));
}

#[test]
fn curl_installer_adds_quoted_path_to_shell_profile_only_once() {
    let fixture = ReleaseFixture::new();
    let install_dir = fixture._root.path().join("installed dir's bin");
    let run = || {
        fixture
            .installer()
            .env("PROG_INSTALL_DIR", &install_dir)
            .env("SHELL", "/bin/zsh")
            .output()
            .unwrap()
    };

    let first = run();
    assert!(first.status.success(), "{}", stderr(&first));
    let second = run();
    assert!(second.status.success(), "{}", stderr(&second));

    let profile = fs::read_to_string(fixture.home_dir.join(".zshrc")).unwrap();
    let quoted = install_dir.to_string_lossy().replace('\'', "'\\''");
    let path_line = format!("export PATH='{quoted}':\"$PATH\"");
    assert_eq!(
        profile.lines().filter(|line| *line == path_line).count(),
        1,
        "the PATH entry must be idempotent: {profile}"
    );
    assert!(stderr(&second).contains("already configured"));
}

#[test]
fn curl_installer_path_setup_can_be_disabled() {
    let fixture = ReleaseFixture::new();
    let output = fixture
        .installer()
        .env("PROG_MODIFY_PATH", "0")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!fixture.home_dir.join(".profile").exists());
    assert!(stderr(&output).contains("PROG_MODIFY_PATH=0"));
}

#[test]
fn curl_installer_does_not_edit_a_profile_when_install_dir_is_already_on_path() {
    let fixture = ReleaseFixture::new();
    let path = format!(
        "{}:{}",
        fixture.install_dir.display(),
        path_with(&fixture.fake_bin)
    );
    let output = fixture.installer().env("PATH", path).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!fixture.home_dir.join(".profile").exists());
    assert!(stderr(&output).contains("already on PATH"));
}

#[test]
fn curl_installer_keeps_the_install_when_login_shell_is_unsupported() {
    let fixture = ReleaseFixture::new();
    let output = fixture
        .installer()
        .env("SHELL", "/bin/fish")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(fixture.install_dir.join("prog").is_file());
    assert!(!fixture.home_dir.join(".profile").exists());
    assert!(stderr(&output).contains("unsupported login shell /bin/fish"));
}

#[test]
fn curl_installer_selects_the_platform_bash_profile() {
    let fixture = ReleaseFixture::new();
    let output = fixture
        .installer()
        .env("SHELL", "/bin/bash")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let profile = if cfg!(target_os = "macos") {
        ".bash_profile"
    } else {
        ".bashrc"
    };
    assert!(fixture.home_dir.join(profile).is_file());
    assert!(stderr(&output).contains(profile));
}

#[test]
fn curl_installer_rejects_invalid_path_setup_mode_before_installing() {
    let fixture = ReleaseFixture::new();
    let output = fixture
        .installer()
        .env("PROG_MODIFY_PATH", "sometimes")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.install_dir.join("prog").exists());
    assert!(stderr(&output).contains("invalid PROG_MODIFY_PATH"));
}

#[test]
fn curl_installer_refuses_checksum_mismatch_without_replacing_binary() {
    let fixture = ReleaseFixture::new();
    fs::create_dir_all(&fixture.install_dir).unwrap();
    let installed = fixture.install_dir.join("prog");
    fs::write(&installed, "old binary\n").unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&fixture.archive)
        .unwrap()
        .write_all(b"corrupt")
        .unwrap();

    let output = fixture.installer().output().unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("checksum verification failed"));
    assert_eq!(fs::read_to_string(installed).unwrap(), "old binary\n");
}

#[test]
fn curl_installer_refuses_failed_attestation_without_replacing_binary() {
    let fixture = ReleaseFixture::new();
    fs::create_dir_all(&fixture.install_dir).unwrap();
    let installed = fixture.install_dir.join("prog");
    fs::write(&installed, "old binary\n").unwrap();

    let output = fixture
        .installer()
        .env("PROG_TEST_ATTEST_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("build-provenance verification failed"));
    assert_eq!(fs::read_to_string(installed).unwrap(), "old binary\n");
}

#[test]
fn curl_installer_rejects_malformed_exact_version_before_download() {
    let fixture = ReleaseFixture::new();
    let output = fixture
        .installer()
        .env_remove("PROG_RELEASE_URL")
        .env("PROG_VERSION", "v9.9.9/asset")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("invalid PROG_VERSION"));
    assert!(!fixture.install_dir.join("prog").exists());
}

#[test]
fn self_update_requires_confirmation_before_network_or_mutation() {
    let output = Command::new(env!("CARGO_BIN_EXE_prog"))
        .arg("update")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "requires_confirmation");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--yes")
    );
}

#[test]
fn self_update_rejects_malformed_exact_version_before_network() {
    let fixture = ReleaseFixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_prog"))
        .args([
            "update",
            "--yes",
            "--target-version",
            "v9.9.9/asset",
            "--install-dir",
        ])
        .arg(&fixture.install_dir)
        .env(
            "PROG_UPDATE_INSTALLER_URL",
            "https://invalid.example.test/install.sh",
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "bad_args");
    assert!(!fixture.install_dir.join("prog").exists());
}

#[test]
fn self_update_does_not_overwrite_an_unmanaged_binary_by_default() {
    let output = Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(["update", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "bad_args");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is not a curl-managed prog installation")
    );
}

#[test]
fn self_update_verifies_installer_then_installs_verified_binary() {
    let fixture = ReleaseFixture::new();
    let output = fixture.updater().output().unwrap();
    assert!(output.status.success(), "{}", stdout(&output));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["status"], "updated");
    assert_eq!(response["installed_version"], FIXTURE_VERSION);
    assert_eq!(response["checksum_verified"], true);
    assert_eq!(response["installer_attestation_verified"], true);
    assert_eq!(response["binary_attestation_verified"], true);
    assert!(fixture.install_dir.join("prog").is_file());
}

#[test]
fn self_update_refuses_unverified_installer_before_running_it() {
    let fixture = ReleaseFixture::new();
    let output = fixture
        .updater()
        .env("PROG_TEST_ATTEST_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["kind"], "cli_exit");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("verify installer provenance")
    );
    assert!(!fixture.install_dir.join("prog").exists());
}

fn target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else {
        panic!("installer tests run only on supported release targets")
    }
}

fn executable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn path_with(prefix: &Path) -> String {
    format!(
        "{}:{}",
        prefix.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

use std::io::Write as _;
