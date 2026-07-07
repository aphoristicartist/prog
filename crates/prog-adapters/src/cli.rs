use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use prog_core::{
    CoreError, RedactionPolicy, Result, TrustSettings, is_sensitive_name, redact_sensitive_text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    task::JoinHandle,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliSource {
    pub id: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_stdout_bytes: usize,
    #[serde(default = "default_max_output_bytes")]
    pub max_stderr_bytes: usize,
    #[serde(default)]
    pub trust: TrustSettings,
    #[serde(default)]
    pub operations: Vec<CliOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliOperation {
    pub id: String,
    #[serde(default)]
    pub input_schema: Value,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub shell: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_stdout_bytes: Option<usize>,
    #[serde(default)]
    pub max_stderr_bytes: Option<usize>,
    #[serde(default)]
    pub sensitive_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliCallResult {
    pub data: Value,
    pub provenance: CliProvenance,
    pub diagnostics: CliDiagnostics,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliProvenance {
    pub source_id: String,
    pub operation: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliDiagnostics {
    pub stderr: Value,
}

impl CliSource {
    pub async fn execute(&self, operation_id: &str, args: &Value) -> Result<CliCallResult> {
        let operation = self
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .ok_or_else(|| CoreError::UnknownOperation {
                source_id: self.id.clone(),
                operation: operation_id.to_string(),
            })?;
        if operation.shell && !self.trust.allow_shell {
            return Err(CoreError::ShellNotTrusted {
                operation: operation.id.clone(),
            });
        }
        let args_object = args_object(operation, args)?;
        validate_args(operation, args_object)?;

        let rendered_command = substitute_template(&operation.command, args_object)?;
        let rendered_args = operation
            .args
            .iter()
            .map(|template| substitute_template(template, args_object))
            .collect::<Result<Vec<_>>>()?;
        let rendered_env = operation
            .env
            .iter()
            .map(|(name, template)| Ok((name.clone(), substitute_template(template, args_object)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let timeout_ms = operation.timeout_ms.unwrap_or(self.timeout_ms);
        let max_stdout_bytes = operation.max_stdout_bytes.unwrap_or(self.max_stdout_bytes);
        let max_stderr_bytes = operation.max_stderr_bytes.unwrap_or(self.max_stderr_bytes);

        let mut command = Command::new(&rendered_command);
        command
            .args(&rendered_args)
            .envs(rendered_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut command);
        if let Some(working_dir) = &operation.working_dir {
            command.current_dir(working_dir);
        }

        let started = Instant::now();
        let mut child = command.spawn().map_err(|error| CoreError::CliTransport {
            operation: operation.id.clone(),
            message: error.to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| CoreError::CliTransport {
            operation: operation.id.clone(),
            message: "failed to capture stdout".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| CoreError::CliTransport {
            operation: operation.id.clone(),
            message: "failed to capture stderr".to_string(),
        })?;

        let stdout_task = tokio::spawn(read_bounded(stdout, max_stdout_bytes));
        let stderr_task = tokio::spawn(read_bounded(stderr, max_stderr_bytes));
        let wait = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;

        let status = match wait {
            Ok(result) => result.map_err(|error| CoreError::CliTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?,
            Err(_) => {
                kill_child_process_group(&mut child).await;
                let _ = tokio::join!(
                    finish_reader_or_abort(stdout_task),
                    finish_reader_or_abort(stderr_task)
                );
                return Err(CoreError::CliTimeout {
                    operation: operation.id.clone(),
                    timeout_ms,
                });
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| CoreError::CliTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?
            .map_err(|error| CoreError::CliTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?;
        let stderr = stderr_task
            .await
            .map_err(|error| CoreError::CliTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?
            .map_err(|error| CoreError::CliTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?;
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let exit_code = status.code().unwrap_or(-1);
        let stderr_preview = normalize_text(&stderr.bytes, stderr.truncated);
        let provenance = CliProvenance {
            source_id: self.id.clone(),
            operation: operation.id.clone(),
            argv: redact_argv(
                std::iter::once(rendered_command.clone())
                    .chain(rendered_args.clone())
                    .collect(),
                args_object,
                operation,
            ),
            exit_code: Some(exit_code),
            duration_ms,
            stdout_bytes: stdout.total_bytes,
            stderr_bytes: stderr.total_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            args: redacted_args(args_object, &operation.sensitive_args),
        };
        let diagnostics = CliDiagnostics {
            stderr: stderr_preview.clone(),
        };
        let mut warnings = Vec::new();
        if stdout.truncated {
            warnings.push(format!(
                "stdout exceeded max_stdout_bytes ({max_stdout_bytes}); output was truncated"
            ));
        }
        if stderr.truncated {
            warnings.push(format!(
                "stderr exceeded max_stderr_bytes ({max_stderr_bytes}); diagnostics were truncated"
            ));
        }

        if !status.success() {
            return Err(CoreError::CliExit {
                operation: operation.id.clone(),
                exit_code,
                stderr_preview,
            });
        }

        Ok(CliCallResult {
            data: normalize_stdout(&stdout.bytes, stdout.truncated),
            provenance,
            diagnostics,
            warnings,
        })
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

async fn kill_child_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
            // process_group(0) makes the process group id equal to the child pid.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_millis(100), child.wait()).await;
}

async fn finish_reader_or_abort(mut task: JoinHandle<std::io::Result<Capture>>) {
    tokio::select! {
        _ = &mut task => {}
        _ = tokio::time::sleep(Duration::from_millis(25)) => {
            task.abort();
            let _ = task.await;
        }
    }
}

fn args_object<'a>(operation: &CliOperation, args: &'a Value) -> Result<&'a Map<String, Value>> {
    args.as_object().ok_or_else(|| CoreError::BadArgs {
        operation: operation.id.clone(),
        reason: "args must be a JSON object".to_string(),
    })
}

fn validate_args(operation: &CliOperation, args: &Map<String, Value>) -> Result<()> {
    let constraints = arg_constraints(operation)?;
    let missing: Vec<&String> = constraints
        .required
        .iter()
        .filter(|name| !args.contains_key(*name))
        .collect();
    let unknown: Vec<&String> = if constraints.allow_unknown {
        Vec::new()
    } else {
        args.keys()
            .filter(|name| !constraints.allowed.contains(*name))
            .collect()
    };

    if missing.is_empty() && unknown.is_empty() {
        return Ok(());
    }

    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!(
            "missing parameters: {}",
            missing
                .into_iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !unknown.is_empty() {
        parts.push(format!(
            "unknown parameters: {}",
            unknown
                .into_iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Err(CoreError::BadArgs {
        operation: operation.id.clone(),
        reason: parts.join("; "),
    })
}

struct ArgConstraints {
    required: BTreeSet<String>,
    allowed: BTreeSet<String>,
    allow_unknown: bool,
}

fn arg_constraints(operation: &CliOperation) -> Result<ArgConstraints> {
    let referenced = referenced_args(operation);
    if operation.input_schema.is_null() {
        return Ok(ArgConstraints {
            required: referenced.clone(),
            allowed: referenced,
            allow_unknown: false,
        });
    }

    let schema = operation
        .input_schema
        .as_object()
        .ok_or_else(|| CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: "input_schema must be a JSON object".to_string(),
        })?;
    if let Some(schema_type) = schema.get("type").and_then(Value::as_str)
        && schema_type != "object"
    {
        return Err(CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: "input_schema.type must be 'object'".to_string(),
        });
    }

    let mut required = referenced.clone();
    if let Some(required_values) = schema.get("required") {
        let required_values = required_values
            .as_array()
            .ok_or_else(|| CoreError::BadArgs {
                operation: operation.id.clone(),
                reason: "input_schema.required must be an array".to_string(),
            })?;
        for value in required_values {
            let name = value.as_str().ok_or_else(|| CoreError::BadArgs {
                operation: operation.id.clone(),
                reason: "input_schema.required entries must be strings".to_string(),
            })?;
            required.insert(name.to_string());
        }
    }

    let mut allowed = BTreeSet::new();
    if let Some(properties) = schema.get("properties") {
        let properties = properties.as_object().ok_or_else(|| CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: "input_schema.properties must be an object".to_string(),
        })?;
        allowed.extend(properties.keys().cloned());
    }
    allowed.extend(required.iter().cloned());

    Ok(ArgConstraints {
        required,
        allowed,
        allow_unknown: schema
            .get("additional_properties")
            .or_else(|| schema.get("additionalProperties"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn referenced_args(operation: &CliOperation) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_placeholders(&operation.command, &mut names);
    for value in operation.args.iter().chain(operation.env.values()) {
        collect_placeholders(value, &mut names);
    }
    names
}

fn collect_placeholders(template: &str, names: &mut BTreeSet<String>) {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = &after[..end];
        if is_placeholder_name(name) {
            names.insert(name.to_string());
        }
        rest = &after[end + 1..];
    }
}

fn substitute_template(template: &str, args: &Map<String, Value>) -> Result<String> {
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            output.push_str(&rest[start..]);
            return Ok(output);
        };
        let name = &after[..end];
        if is_placeholder_name(name) {
            output.push_str(&arg_to_string(
                name,
                args.get(name).ok_or_else(|| CoreError::BadArgs {
                    operation: "cli template".to_string(),
                    reason: format!("missing parameter '{name}'"),
                })?,
            )?);
        } else {
            output.push('{');
            output.push_str(name);
            output.push('}');
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn is_placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn arg_to_string(name: &str, value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null => Err(CoreError::BadArgs {
            operation: "cli template".to_string(),
            reason: format!("parameter '{name}' cannot be null"),
        }),
        Value::Array(_) | Value::Object(_) => Err(CoreError::BadArgs {
            operation: "cli template".to_string(),
            reason: format!("parameter '{name}' must be scalar"),
        }),
    }
}

#[derive(Debug)]
struct Capture {
    bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
}

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> std::io::Result<Capture> {
    let mut output = Vec::new();
    let mut total_bytes = 0usize;
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read);
        let remaining = cap.saturating_sub(output.len());
        if remaining > 0 {
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        if read > remaining || total_bytes > cap {
            truncated = true;
        }
    }
    Ok(Capture {
        bytes: output,
        total_bytes,
        truncated,
    })
}

fn normalize_stdout(bytes: &[u8], truncated: bool) -> Value {
    if !truncated && let Ok(value) = serde_json::from_slice(bytes) {
        return value;
    }
    normalize_text(bytes, truncated)
}

fn normalize_text(bytes: &[u8], truncated: bool) -> Value {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<String> = text
        .lines()
        .map(|line| redact_sensitive_text(line).0)
        .collect();
    let head: Vec<Value> = lines.iter().take(10).map(|line| json!(line)).collect();
    let tail_start = lines.len().saturating_sub(10).max(head.len());
    let tail: Vec<Value> = lines
        .iter()
        .skip(tail_start)
        .map(|line| json!(line))
        .collect();
    json!({
        "format": "text",
        "head": head,
        "tail": tail,
        "line_count": lines.len(),
        "byte_count": bytes.len(),
        "truncated": truncated
    })
}

fn redacted_args(args: &Map<String, Value>, sensitive: &[String]) -> Value {
    RedactionPolicy::with_extra_persistence_names(sensitive)
        .apply_persistence(&Value::Object(args.clone()))
        .0
}

fn redact_argv(
    argv: Vec<String>,
    args: &Map<String, Value>,
    operation: &CliOperation,
) -> Vec<String> {
    let sensitive_values: Vec<String> = args
        .iter()
        .filter(|(name, _)| {
            operation.sensitive_args.iter().any(|arg| arg == *name) || is_sensitive_name(name)
        })
        .filter_map(|(_, value)| redaction_value(value))
        .collect();

    argv.into_iter()
        .map(|part| {
            sensitive_values.iter().fold(part, |redacted, secret| {
                redacted.replace(secret, "[REDACTED]")
            })
        })
        .collect()
}

fn redaction_value(value: &Value) -> Option<String> {
    let value = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    (!value.is_empty()).then_some(value)
}

fn default_timeout_ms() -> u64 {
    30_000
}

fn default_max_output_bytes() -> usize {
    1024 * 1024
}
