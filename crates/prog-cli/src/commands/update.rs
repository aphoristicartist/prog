use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use prog_core::{CoreError, Result};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::cli_args::UpdateArgs;

const REPOSITORY: &str = "aphoristicartist/prog";
const OWNER: &str = "aphoristicartist";
const MANAGED_INSTALL_MARKER: &str = ".prog-install";

#[derive(Debug, Serialize)]
pub(crate) struct UpdateReport {
    status: &'static str,
    previous_version: &'static str,
    installed_version: String,
    installed_to: PathBuf,
    checksum_verified: bool,
    installer_attestation_verified: bool,
    binary_attestation_verified: bool,
    update_mode: &'static str,
}

pub(crate) fn update_command(args: &UpdateArgs) -> Result<UpdateReport> {
    if !args.yes {
        return Err(CoreError::RequiresConfirmation {
            operation: "prog self-update".to_string(),
            class: "a binary replacement".to_string(),
            effects: "mutating=true, network=true, requires_confirmation=true".to_string(),
        });
    }
    if let Some(version) = &args.target_version {
        validate_version(version)?;
    }

    let install_dir = resolve_install_dir(args.install_dir.as_deref())?;
    let installer_url = installer_url(args.target_version.as_deref())?;
    let temp = UpdateTemp::create()?;
    let installer_path = temp.path.join("install.sh");
    let allowed_protocol = if installer_url.starts_with("file://") {
        "=file"
    } else {
        "=https"
    };

    let mut curl = Command::new("curl");
    curl.args([
        "--proto",
        allowed_protocol,
        "--proto-redir",
        allowed_protocol,
        "--tlsv1.2",
        "-fsSL",
        "--output",
    ])
    .arg(&installer_path)
    .arg(&installer_url);
    checked_output("download verified installer", &mut curl)?;

    let mut attest = Command::new("gh");
    attest
        .args(["attestation", "verify"])
        .arg(&installer_path)
        .args(["--owner", OWNER]);
    checked_output("verify installer provenance", &mut attest)?;

    let mut install = Command::new("sh");
    install
        .arg(&installer_path)
        .env("PROG_INSTALL_DIR", &install_dir);
    if let Some(version) = &args.target_version {
        install.env("PROG_VERSION", version);
    }
    checked_output("install verified prog update", &mut install)?;

    let installed_binary = install_dir.join("prog");
    let mut version_command = Command::new(&installed_binary);
    version_command.arg("--version");
    let output = checked_output("read installed prog version", &mut version_command)?;
    let installed_version = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next_back()
        .ok_or_else(|| update_error("installed prog returned no version"))?
        .to_string();

    Ok(UpdateReport {
        status: "updated",
        previous_version: env!("CARGO_PKG_VERSION"),
        installed_version,
        installed_to: installed_binary,
        checksum_verified: true,
        installer_attestation_verified: true,
        binary_attestation_verified: true,
        update_mode: "explicit_confirmed_self_update",
    })
}

fn resolve_install_dir(requested: Option<&Path>) -> Result<PathBuf> {
    if let Some(requested) = requested {
        return Ok(requested.to_path_buf());
    }
    let current = std::env::current_exe()?;
    let parent = current.parent().ok_or_else(|| {
        update_error("current executable has no parent directory; pass --install-dir")
    })?;
    let marker = parent.join(MANAGED_INSTALL_MARKER);
    let marker_text = fs::read_to_string(&marker).map_err(|_| {
        CoreError::BadArgs {
            operation: "prog self-update".to_string(),
            reason: format!(
                "{} is not a curl-managed prog installation; pass --install-dir to choose an explicit destination",
                current.display()
            ),
        }
    })?;
    if !marker_text
        .lines()
        .any(|line| line == format!("repository={REPOSITORY}"))
    {
        return Err(update_error(format!(
            "managed-install marker {} does not name repository {REPOSITORY}",
            marker.display()
        )));
    }
    Ok(parent.to_path_buf())
}

fn installer_url(version: Option<&str>) -> Result<String> {
    if let Some(override_url) = std::env::var_os("PROG_UPDATE_INSTALLER_URL") {
        let override_url = override_url.into_string().map_err(|_| CoreError::BadArgs {
            operation: "prog self-update".to_string(),
            reason: "PROG_UPDATE_INSTALLER_URL must be valid UTF-8".to_string(),
        })?;
        match override_url.as_str() {
            value if value.starts_with("https://") => return Ok(value.to_string()),
            value
                if value.starts_with("file://")
                    && std::env::var_os("PROG_ALLOW_FILE_URL").as_deref()
                        == Some(std::ffi::OsStr::new("1")) =>
            {
                return Ok(value.to_string());
            }
            _ => {
                return Err(CoreError::BadArgs {
                    operation: "prog self-update".to_string(),
                    reason: "installer URL overrides must use HTTPS (file URLs are test-only)"
                        .to_string(),
                });
            }
        }
    }
    Ok(match version {
        Some(version) => {
            format!("https://github.com/{REPOSITORY}/releases/download/{version}/install.sh")
        }
        None => format!("https://github.com/{REPOSITORY}/releases/latest/download/install.sh"),
    })
}

fn validate_version(version: &str) -> Result<()> {
    let body = version.strip_prefix('v');
    let (core, prerelease) = body.map_or((None, None), |value| {
        value
            .split_once('-')
            .map_or((Some(value), None), |(core, prerelease)| {
                (Some(core), Some(prerelease))
            })
    });
    let valid_core = core.is_some_and(|value| {
        let mut parts = value.split('.');
        parts.next().is_some_and(all_ascii_digits)
            && parts.next().is_some_and(all_ascii_digits)
            && parts.next().is_some_and(all_ascii_digits)
            && parts.next().is_none()
    });
    let valid_prerelease = prerelease.is_none_or(|value| {
        !value.is_empty()
            && value.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    });
    if body.is_some() && valid_core && valid_prerelease {
        Ok(())
    } else {
        Err(CoreError::BadArgs {
            operation: "prog self-update".to_string(),
            reason: format!("invalid target version '{version}'; expected vMAJOR.MINOR.PATCH"),
        })
    }
}

fn all_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn checked_output(operation: &str, command: &mut Command) -> Result<Output> {
    let output = command.output().map_err(|error| CoreError::CliTransport {
        operation: operation.to_string(),
        message: error.to_string(),
    })?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.chars().take(4_096).collect::<String>();
    Err(CoreError::CliExit {
        operation: operation.to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        stderr_preview: json!({"text": stderr}),
    })
}

fn update_error(message: impl Into<String>) -> CoreError {
    CoreError::BadArgs {
        operation: "prog self-update".to_string(),
        reason: message.into(),
    }
}

struct UpdateTemp {
    path: PathBuf,
}

impl UpdateTemp {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "prog-update-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for UpdateTemp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_versions_are_strict_and_path_safe() {
        for valid in ["v0.1.0", "v1.2.3-rc.1"] {
            assert!(validate_version(valid).is_ok(), "{valid}");
        }
        for invalid in ["0.1.0", "v1.2", "v1.2.3/asset", "v1.2.three", "v1.2.3?x=1"] {
            assert!(validate_version(invalid).is_err(), "{invalid}");
        }
    }
}
