use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("command '{command}' is not implemented yet")]
    NotImplemented { command: &'static str },

    #[error("invalid command line: {0}")]
    CliUsage(String),

    #[error(
        "unknown source '{0}'; run `prog discover {0} --kind <http|cli|mcp> --seed <seed>` first"
    )]
    UnknownSource(String),

    #[error(
        "unknown operation '{operation}' on source '{source_id}'; run `prog hints {source_id}` to list operations"
    )]
    UnknownOperation {
        source_id: String,
        operation: String,
    },

    #[error(
        "invalid cursor '{0}': not found in the local store (it may belong to another .prog directory)"
    )]
    CursorNotFound(String),

    #[error("cursor '{0}' expired at {1}; re-run the original call to refresh the cache")]
    CursorExpired(String, String),

    #[error(
        "cursor '{cursor}' was created under redaction policy v{cursor_version} but the store now uses v{store_version}; failing closed — re-run the original call"
    )]
    RedactionVersionMismatch {
        cursor: String,
        cursor_version: u32,
        store_version: u32,
    },

    #[error("path '{path}' escapes the cursor's provenance boundary '{boundary}'")]
    PathOutsideBoundary { path: String, boundary: String },

    #[error("path '{path}' does not exist in the cached payload{hint}")]
    PathNotFound { path: String, hint: String },

    #[error("path '{0}' is blocked by expansion redaction rule '{1}'")]
    ExpansionRedacted(String, String),

    #[error("cache entry '{0}' not found or expired")]
    CacheMiss(String),

    #[error("operation '{operation}' is {class}; pass --yes to confirm (effects: {effects})")]
    RequiresConfirmation {
        operation: String,
        class: String,
        effects: String,
    },

    #[error(
        "operation '{operation}' is shell-backed and the source profile does not set trust.allow_shell — edit the profile to allow it explicitly"
    )]
    ShellNotTrusted { operation: String },

    #[error("discovery may only invoke read-only operations; '{0}' is not marked read-only")]
    DiscoveryNotReadOnly(String),

    #[error("invalid JSON pointer '{0}': must be empty or start with '/'")]
    BadPointer(String),

    #[error("invalid arguments for '{operation}': {reason}")]
    BadArgs { operation: String, reason: String },

    #[error("http operation '{operation}' timed out after {timeout_ms} ms")]
    HttpTimeout { operation: String, timeout_ms: u64 },

    #[error("http transport error for '{operation}': {message}")]
    HttpTransport { operation: String, message: String },

    #[error("http operation '{operation}' returned status {status}: {body_preview}")]
    HttpStatus {
        operation: String,
        status: u16,
        body_preview: serde_json::Value,
    },

    #[error("storage error: {0}")]
    Storage(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl CoreError {
    pub fn storage(e: impl std::fmt::Display) -> Self {
        CoreError::Storage(e.to_string())
    }

    pub fn kind(&self) -> &'static str {
        match self {
            CoreError::NotImplemented { .. } => "not_implemented",
            CoreError::CliUsage(_) => "cli_usage",
            CoreError::UnknownSource(_) => "unknown_source",
            CoreError::UnknownOperation { .. } => "unknown_operation",
            CoreError::CursorNotFound(_) => "cursor_not_found",
            CoreError::CursorExpired(_, _) => "cursor_expired",
            CoreError::RedactionVersionMismatch { .. } => "redaction_version_mismatch",
            CoreError::PathOutsideBoundary { .. } => "path_outside_boundary",
            CoreError::PathNotFound { .. } => "path_not_found",
            CoreError::ExpansionRedacted(_, _) => "expansion_redacted",
            CoreError::CacheMiss(_) => "cache_miss",
            CoreError::RequiresConfirmation { .. } => "requires_confirmation",
            CoreError::ShellNotTrusted { .. } => "shell_not_trusted",
            CoreError::DiscoveryNotReadOnly(_) => "discovery_not_read_only",
            CoreError::BadPointer(_) => "bad_pointer",
            CoreError::BadArgs { .. } => "bad_args",
            CoreError::HttpTimeout { .. } => "http_timeout",
            CoreError::HttpTransport { .. } => "http_transport",
            CoreError::HttpStatus { .. } => "http_status",
            CoreError::Storage(_) => "storage",
            CoreError::Json(_) => "json",
            CoreError::Io(_) => "io",
        }
    }

    pub fn hint(&self) -> String {
        match self {
            CoreError::NotImplemented { command } => {
                format!(
                    "The '{command}' command is scaffolded by issue #1; implement its roadmap issue before using it."
                )
            }
            CoreError::CliUsage(_) => {
                "Run `prog --help` to see the supported command tree.".to_string()
            }
            CoreError::UnknownSource(source) => {
                format!("Run `prog discover {source} --kind <http|cli|mcp> --seed <seed>` first.")
            }
            CoreError::UnknownOperation { source_id, .. } => {
                format!("Run `prog hints {source_id}` to list operations.")
            }
            CoreError::CursorNotFound(_) => {
                "Check --dir/PROG_DIR or re-run the original call to create a cursor.".to_string()
            }
            CoreError::CursorExpired(_, _) => {
                "Re-run the original call to refresh the cache.".to_string()
            }
            CoreError::RedactionVersionMismatch { .. } => {
                "Re-run the original call under the current redaction policy.".to_string()
            }
            CoreError::PathOutsideBoundary { .. } => {
                "Choose a path inside the cursor's root path.".to_string()
            }
            CoreError::PathNotFound { .. } => {
                "Use the reported ancestor keys to choose an existing JSON Pointer.".to_string()
            }
            CoreError::ExpansionRedacted(_, rule) => {
                format!("The expansion redaction rule '{rule}' blocks this path.")
            }
            CoreError::CacheMiss(_) => {
                "Re-run the original call or choose an unexpired cache key.".to_string()
            }
            CoreError::RequiresConfirmation { .. } => {
                "Pass --yes only after confirming the mutation is intended.".to_string()
            }
            CoreError::ShellNotTrusted { .. } => {
                "Set trust.allow_shell in the source profile only if this command is trusted."
                    .to_string()
            }
            CoreError::DiscoveryNotReadOnly(_) => {
                "Mark the operation read_only only when probing it cannot mutate upstream state."
                    .to_string()
            }
            CoreError::BadPointer(_) => {
                "Use an RFC 6901 JSON Pointer such as /items/0/body.".to_string()
            }
            CoreError::BadArgs { .. } => "Fix the named missing or unknown arguments.".to_string(),
            CoreError::HttpTimeout { .. } => {
                "Increase the operation timeout or retry when the upstream is healthy.".to_string()
            }
            CoreError::HttpTransport { .. } => {
                "Check the upstream URL, network access, and TLS settings.".to_string()
            }
            CoreError::HttpStatus { .. } => {
                "Inspect the bounded response preview and adjust the request or credentials."
                    .to_string()
            }
            CoreError::Storage(_) => {
                "Check the local .prog store and filesystem permissions.".to_string()
            }
            CoreError::Json(_) => "Provide valid JSON for the requested argument.".to_string(),
            CoreError::Io(_) => "Check the referenced path and filesystem permissions.".to_string(),
        }
    }

    pub fn envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ErrorBody {
                kind: self.kind().to_string(),
                message: self.to_string(),
                hint: self.hint(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorBody {
    pub kind: String,
    pub message: String,
    pub hint: String,
}

pub type Result<T> = std::result::Result<T, CoreError>;
