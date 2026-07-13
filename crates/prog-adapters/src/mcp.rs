use std::{
    collections::BTreeMap,
    future::Future,
    process::Stdio,
    time::{Duration, Instant},
};

use prog_core::{
    CachePolicy, CoreError, EffectSet, EvidenceGrade, Extra, OperationProfile, PreviewPolicy,
    Result, SOURCE_PROFILE_SCHEMA, SourceKind, SourceProfile, TrustSettings, mcp_read_effects,
    mcp_tool_annotation_effects, project, redact_sensitive_text, stamp_evidence_grade,
};
use rmcp::{
    RoleClient, ServiceError,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientInfo, ContentBlock,
        Implementation, Prompt, ReadResourceRequestParams, ReadResourceResult, Resource,
        ResourceContents, ServerInfo, Tool, ToolAnnotations,
    },
    serve_client,
    service::RunningService,
    transport::TokioChildProcess,
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
pub struct McpSource {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_content_bytes")]
    pub max_content_bytes: usize,
    #[serde(default = "default_max_stderr_bytes")]
    pub max_stderr_bytes: usize,
    #[serde(default = "default_max_schema_depth")]
    pub max_schema_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpDiscoveryResult {
    pub profile: SourceProfile,
    pub provenance: McpProvenance,
    pub diagnostics: McpDiagnostics,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpCallResult {
    pub data: Value,
    pub provenance: McpProvenance,
    pub diagnostics: McpDiagnostics,
    pub received_error: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpProvenance {
    pub source_id: String,
    pub operation: String,
    pub server_command: Vec<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
    pub duration_ms: u64,
    pub response_bytes: usize,
    pub truncated: bool,
    pub structured_content: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpDiagnostics {
    pub stderr: Value,
}

impl McpSource {
    pub async fn discover(&self) -> Result<McpDiscoveryResult> {
        let started = Instant::now();
        let mut session = self.connect("discover").await?;
        let protocol_version = session.protocol_version();
        let server_info = session.server_info();
        let mut operations = Vec::new();
        let mut warnings = Vec::new();

        if server_info
            .as_ref()
            .is_none_or(|info| info.capabilities.tools.is_some())
        {
            let tools = self
                .request("tools/list", session.client.peer().list_all_tools())
                .await?;
            for tool in tools {
                operations.push(self.tool_operation(tool, &mut warnings));
            }
        }

        if server_info
            .as_ref()
            .is_none_or(|info| info.capabilities.resources.is_some())
        {
            let resources = self
                .request("resources/list", session.client.peer().list_all_resources())
                .await?;
            for resource in resources {
                operations.push(resource_operation(resource));
            }
        }

        if server_info
            .as_ref()
            .is_none_or(|info| info.capabilities.prompts.is_some())
        {
            let prompts = self
                .request("prompts/list", session.client.peer().list_all_prompts())
                .await?;
            for prompt in prompts {
                operations.push(prompt_operation(prompt));
            }
        }

        let diagnostics = session.shutdown(self.timeout_ms).await;
        let duration_ms = elapsed_ms(started);
        let mut extra = Extra::new();
        extra.insert(
            "seed".to_string(),
            json!({
                "kind": "mcp",
                "command": self.command,
                "args": self.args,
                "env": self.env.keys().cloned().collect::<Vec<_>>()
            }),
        );
        if let Some(info) = &server_info {
            extra.insert(
                "mcp_server".to_string(),
                json!({
                    "name": info.server_info.name,
                    "version": info.server_info.version,
                    "protocol_version": info.protocol_version.as_str()
                }),
            );
        }

        Ok(McpDiscoveryResult {
            profile: SourceProfile {
                schema: SOURCE_PROFILE_SCHEMA.to_string(),
                id: self.id.clone(),
                kind: SourceKind::Mcp,
                revision: 1,
                description: server_info
                    .as_ref()
                    .and_then(|info| info.instructions.clone())
                    .or_else(|| Some(format!("MCP stdio source: {}", self.command))),
                operations,
                auth: Vec::new(),
                cache: CachePolicy::default(),
                trust: TrustSettings::default(),
                effect_defaults: EffectSet::default(),
                redaction: prog_core::RedactionConfig::default(),
                extra,
            },
            provenance: self.provenance("discover", protocol_version, duration_ms, 0, false, false),
            diagnostics,
            warnings,
        })
    }

    pub async fn call_tool(&self, tool_name: &str, args: &Value) -> Result<McpCallResult> {
        let args = args.as_object().ok_or_else(|| CoreError::BadArgs {
            operation: tool_name.to_string(),
            reason: "MCP tool arguments must be a JSON object".to_string(),
        })?;
        let operation = format!("tools/call:{tool_name}");
        let started = Instant::now();
        let mut session = self.connect(&operation).await?;
        let protocol_version = session.protocol_version();
        let result = self
            .request(
                &operation,
                session.client.peer().call_tool(
                    CallToolRequestParams::new(tool_name.to_string()).with_arguments(args.clone()),
                ),
            )
            .await?;
        let normalized = self.normalize_tool_result(tool_name, result)?;
        let diagnostics = session.shutdown(self.timeout_ms).await;

        Ok(McpCallResult {
            data: normalized.data,
            provenance: self.provenance(
                &operation,
                protocol_version,
                elapsed_ms(started),
                normalized.response_bytes,
                normalized.truncated,
                normalized.structured_content,
            ),
            diagnostics,
            received_error: normalized.received_error,
            warnings: normalized.warnings,
        })
    }

    pub async fn read_resource(&self, uri: &str) -> Result<McpCallResult> {
        let operation = format!("resources/read:{uri}");
        let started = Instant::now();
        let mut session = self.connect(&operation).await?;
        let protocol_version = session.protocol_version();
        let result = self
            .request(
                &operation,
                session
                    .client
                    .peer()
                    .read_resource(ReadResourceRequestParams::new(uri.to_string())),
            )
            .await?;
        let normalized = self.normalize_resource_result(result)?;
        let diagnostics = session.shutdown(self.timeout_ms).await;

        Ok(McpCallResult {
            data: normalized.data,
            provenance: self.provenance(
                &operation,
                protocol_version,
                elapsed_ms(started),
                normalized.response_bytes,
                normalized.truncated,
                normalized.structured_content,
            ),
            diagnostics,
            received_error: false,
            warnings: normalized.warnings,
        })
    }

    async fn connect(&self, operation: &str) -> Result<McpSession> {
        let mut command = Command::new(&self.command);
        command.args(&self.args).envs(&self.env).kill_on_drop(true);
        configure_process_group(&mut command);

        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| CoreError::McpTransport {
                operation: operation.to_string(),
                message: error.to_string(),
            })?;
        let stderr_task =
            stderr.map(|stderr| tokio::spawn(read_bounded(stderr, self.max_stderr_bytes)));
        let client_info = ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("prog", env!("CARGO_PKG_VERSION")),
        );

        let client = match tokio::time::timeout(
            Duration::from_millis(self.timeout_ms),
            serve_client(client_info, transport),
        )
        .await
        {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => {
                wait_for_stderr(stderr_task).await;
                return Err(CoreError::McpTransport {
                    operation: operation.to_string(),
                    message: error.to_string(),
                });
            }
            Err(_) => {
                wait_for_stderr(stderr_task).await;
                return Err(CoreError::McpTimeout {
                    operation: operation.to_string(),
                    timeout_ms: self.timeout_ms,
                });
            }
        };

        Ok(McpSession {
            client,
            stderr_task,
        })
    }

    async fn request<T, F>(&self, operation: &str, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, ServiceError>>,
    {
        tokio::time::timeout(Duration::from_millis(self.timeout_ms), future)
            .await
            .map_err(|_| CoreError::McpTimeout {
                operation: operation.to_string(),
                timeout_ms: self.timeout_ms,
            })?
            .map_err(|error| map_service_error(operation, error))
    }

    fn tool_operation(&self, tool: Tool, warnings: &mut Vec<String>) -> OperationProfile {
        let mut input_flags = SchemaImportFlags::default();
        let input_schema = import_schema(
            Value::Object(tool.input_schema.as_ref().clone()),
            self.max_schema_depth,
            &mut input_flags,
        );
        push_schema_warnings(tool.name.as_ref(), "inputSchema", &input_flags, warnings);

        let mut output_flags = SchemaImportFlags::default();
        let declared_output_schema = tool.output_schema.as_ref().map(|schema| {
            import_schema(
                Value::Object(schema.as_ref().clone()),
                self.max_schema_depth,
                &mut output_flags,
            )
        });
        push_schema_warnings(tool.name.as_ref(), "outputSchema", &output_flags, warnings);

        let mut extra = Extra::new();
        extra.insert(
            "invocation".to_string(),
            json!({"mcp": {"kind": "tool", "name": tool.name.as_ref()}}),
        );
        if let Some(title) = tool.title {
            extra.insert("title".to_string(), json!(title));
        }

        OperationProfile {
            id: tool.name.to_string(),
            description: tool.description.map(|description| description.to_string()),
            input_schema,
            output_shape: None,
            declared_output_schema,
            effects: tool_effects(tool.annotations.as_ref()),
            cache: CachePolicy::default(),
            pagination: None,
            extra,
        }
    }

    fn normalize_tool_result(
        &self,
        tool_name: &str,
        result: CallToolResult,
    ) -> Result<NormalizedMcpData> {
        let received_error = result.is_error.unwrap_or(false);
        let mut normalized = if let Some(data) = result.structured_content {
            let response_bytes = serde_json::to_vec(&data).map_or(0, |bytes| bytes.len());
            if response_bytes > self.max_content_bytes {
                let projection = project(
                    &data,
                    &PreviewPolicy {
                        max_envelope_bytes: self.max_content_bytes,
                        ..PreviewPolicy::default()
                    },
                    "",
                );
                NormalizedMcpData {
                    data: json!({
                        "format": "structured_content",
                        "preview": projection.preview,
                        "omitted": projection.omitted,
                        "byte_count": response_bytes,
                        "truncated": true
                    }),
                    response_bytes,
                    truncated: true,
                    structured_content: true,
                    warnings: vec![format!(
                        "structuredContent exceeded max_content_bytes ({}); content was truncated",
                        self.max_content_bytes
                    )],
                    received_error: false,
                }
            } else {
                NormalizedMcpData {
                    data,
                    response_bytes,
                    truncated: false,
                    structured_content: true,
                    warnings: Vec::new(),
                    received_error: false,
                }
            }
        } else {
            self.normalize_content_blocks(&result.content)
        };
        if received_error {
            normalized.warnings.push(format!(
                "MCP tool '{tool_name}' returned isError content; captured as error evidence"
            ));
            normalized.received_error = true;
        }
        Ok(normalized)
    }

    fn normalize_resource_result(&self, result: ReadResourceResult) -> Result<NormalizedMcpData> {
        let text = resource_text(&result.contents);
        if !text.is_empty() {
            return Ok(self.normalize_text(text));
        }
        let data = serde_json::to_value(&result.contents)?;
        let response_bytes = serde_json::to_vec(&data).map_or(0, |bytes| bytes.len());
        Ok(NormalizedMcpData {
            data,
            response_bytes,
            truncated: false,
            structured_content: false,
            warnings: Vec::new(),
            received_error: false,
        })
    }

    fn normalize_content_blocks(&self, content: &[ContentBlock]) -> NormalizedMcpData {
        let text = content_text(content);
        if !text.is_empty() {
            return self.normalize_text(text);
        }
        let data = serde_json::to_value(content).unwrap_or_else(|_| json!([]));
        let response_bytes = serde_json::to_vec(&data).map_or(0, |bytes| bytes.len());
        NormalizedMcpData {
            data,
            response_bytes,
            truncated: false,
            structured_content: false,
            warnings: Vec::new(),
            received_error: false,
        }
    }

    fn normalize_text(&self, text: String) -> NormalizedMcpData {
        let response_bytes = text.len();
        let truncated = response_bytes > self.max_content_bytes;
        let bounded = if truncated {
            &text[..safe_prefix_len(&text, self.max_content_bytes)]
        } else {
            text.as_str()
        };
        if !truncated && let Ok(data) = serde_json::from_str(bounded) {
            return NormalizedMcpData {
                data,
                response_bytes,
                truncated,
                structured_content: false,
                warnings: Vec::new(),
                received_error: false,
            };
        }
        let lines: Vec<String> = bounded
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
        let mut warnings = Vec::new();
        if truncated {
            warnings.push(format!(
                "content exceeded max_content_bytes ({}); content was truncated",
                self.max_content_bytes
            ));
        }
        NormalizedMcpData {
            data: json!({
                "format": "text",
                "head": head,
                "tail": tail,
                "line_count": lines.len(),
                "byte_count": response_bytes,
                "truncated": truncated
            }),
            response_bytes,
            truncated,
            structured_content: false,
            warnings,
            received_error: false,
        }
    }

    fn provenance(
        &self,
        operation: &str,
        protocol_version: Option<String>,
        duration_ms: u64,
        response_bytes: usize,
        truncated: bool,
        structured_content: bool,
    ) -> McpProvenance {
        McpProvenance {
            source_id: self.id.clone(),
            operation: operation.to_string(),
            server_command: std::iter::once(self.command.clone())
                .chain(self.args.clone())
                .collect(),
            protocol_version,
            duration_ms,
            response_bytes,
            truncated,
            structured_content,
        }
    }
}

struct McpSession {
    client: RunningService<RoleClient, ClientInfo>,
    stderr_task: Option<JoinHandle<std::io::Result<Capture>>>,
}

impl McpSession {
    fn protocol_version(&self) -> Option<String> {
        self.client
            .peer()
            .peer_info()
            .map(|info| info.protocol_version.as_str().to_string())
    }

    fn server_info(&self) -> Option<ServerInfo> {
        self.client.peer().peer_info().map(|info| (*info).clone())
    }

    async fn shutdown(&mut self, timeout_ms: u64) -> McpDiagnostics {
        let _ = self
            .client
            .close_with_timeout(Duration::from_millis(timeout_ms))
            .await;
        let stderr = match self.stderr_task.take() {
            Some(task) => tokio::time::timeout(Duration::from_millis(timeout_ms), task)
                .await
                .ok()
                .and_then(|joined| joined.ok())
                .and_then(|capture| capture.ok()),
            None => None,
        };
        McpDiagnostics {
            stderr: stderr
                .map(|capture| {
                    normalize_text_capture(&capture.bytes, capture.total_bytes, capture.truncated)
                })
                .unwrap_or_else(|| normalize_text_capture(&[], 0, false)),
        }
    }
}

#[derive(Debug)]
struct NormalizedMcpData {
    data: Value,
    response_bytes: usize,
    truncated: bool,
    structured_content: bool,
    warnings: Vec<String>,
    received_error: bool,
}

#[derive(Default)]
struct SchemaImportFlags {
    external_refs: usize,
    truncated: bool,
}

fn tool_effects(annotations: Option<&ToolAnnotations>) -> EffectSet {
    let read_only_hint = annotations.and_then(|annotation| annotation.read_only_hint);
    let destructive_hint = annotations.and_then(|annotation| annotation.destructive_hint);
    mcp_tool_annotation_effects(read_only_hint, destructive_hint)
}

fn resource_operation(resource: Resource) -> OperationProfile {
    let mut extra = Extra::new();
    extra.insert(
        "invocation".to_string(),
        json!({"mcp": {"kind": "resource", "uri": resource.uri}}),
    );
    if let Some(mime_type) = resource.mime_type {
        extra.insert("mime_type".to_string(), json!(mime_type));
    }
    if let Some(size) = resource.size {
        extra.insert("size".to_string(), json!(size));
    }
    OperationProfile {
        id: format!("resource:{}", resource.name),
        description: resource.description,
        input_schema: json!({
            "type": "object",
            "required": ["uri"],
            "properties": {
                "uri": { "type": "string", "const": resource.uri }
            }
        }),
        output_shape: None,
        declared_output_schema: None,
        effects: {
            // An MCP resource is spec-defined read-only: graded Proven and
            // stored gated so trust.auto_upgrade is a live runtime knob.
            let mut effects = mcp_read_effects();
            effects.requires_confirmation = true;
            stamp_evidence_grade(&mut effects, EvidenceGrade::Proven);
            effects
        },
        cache: CachePolicy::default(),
        pagination: None,
        extra,
    }
}

fn prompt_operation(prompt: Prompt) -> OperationProfile {
    let mut properties = Map::new();
    let mut required = Vec::new();
    if let Some(arguments) = &prompt.arguments {
        for argument in arguments {
            properties.insert(
                argument.name.clone(),
                json!({
                    "type": "string",
                    "description": argument.description
                }),
            );
            if argument.required.unwrap_or(false) {
                required.push(argument.name.clone());
            }
        }
    }
    let mut extra = Extra::new();
    extra.insert(
        "invocation".to_string(),
        json!({"mcp": {"kind": "prompt", "name": prompt.name}}),
    );
    if let Some(title) = prompt.title {
        extra.insert("title".to_string(), json!(title));
    }
    OperationProfile {
        id: format!("prompt:{}", prompt.name),
        description: prompt.description,
        input_schema: json!({
            "type": "object",
            "required": required,
            "properties": properties
        }),
        output_shape: None,
        declared_output_schema: None,
        effects: EffectSet {
            read_only: false,
            mutating: false,
            network: false,
            shell: false,
            sensitive: false,
            cacheable: false,
            requires_confirmation: true,
            extra: Extra::new(),
        },
        cache: CachePolicy::default(),
        pagination: None,
        extra,
    }
}

fn import_schema(value: Value, max_depth: usize, flags: &mut SchemaImportFlags) -> Value {
    if max_depth == 0 {
        flags.truncated = true;
        return json!({"description": "schema omitted after max_schema_depth"});
    }
    match value {
        Value::Object(map) => {
            let mut output = Map::new();
            for (key, value) in map {
                if key == "$ref"
                    && let Some(reference) = value.as_str()
                    && is_external_ref(reference)
                {
                    flags.external_refs += 1;
                }
                output.insert(key, import_schema(value, max_depth - 1, flags));
            }
            Value::Object(output)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| import_schema(value, max_depth - 1, flags))
                .collect(),
        ),
        scalar => scalar,
    }
}

fn push_schema_warnings(
    operation: &str,
    field: &str,
    flags: &SchemaImportFlags,
    warnings: &mut Vec<String>,
) {
    if flags.external_refs > 0 {
        warnings.push(format!(
            "{operation} {field} contained {} external $ref value(s); refs were preserved without dereferencing",
            flags.external_refs
        ));
    }
    if flags.truncated {
        warnings.push(format!(
            "{operation} {field} exceeded max_schema_depth; nested schema content was truncated"
        ));
    }
}

fn is_external_ref(reference: &str) -> bool {
    reference.starts_with("http://")
        || reference.starts_with("https://")
        || reference.starts_with("urn:")
}

fn map_service_error(operation: &str, error: ServiceError) -> CoreError {
    match error {
        ServiceError::McpError(error) => CoreError::McpProtocol {
            operation: operation.to_string(),
            message: error.to_string(),
            preview: serde_json::to_value(&error).unwrap_or_else(|_| json!({})),
        },
        ServiceError::Timeout { timeout } => CoreError::McpTimeout {
            operation: operation.to_string(),
            timeout_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
        },
        other => CoreError::McpTransport {
            operation: operation.to_string(),
            message: other.to_string(),
        },
    }
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            ContentBlock::Resource(resource) => match &resource.resource {
                ResourceContents::TextResourceContents { text, .. } => Some(text.as_str()),
                ResourceContents::BlobResourceContents { .. } => None,
                _ => None,
            },
            ContentBlock::Image(_) | ContentBlock::Audio(_) | ContentBlock::ResourceLink(_) => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resource_text(contents: &[ResourceContents]) -> String {
    contents
        .iter()
        .filter_map(|content| match content {
            ResourceContents::TextResourceContents { text, .. } => Some(text.as_str()),
            ResourceContents::BlobResourceContents { .. } => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn safe_prefix_len(text: &str, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return text.len();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

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

async fn wait_for_stderr(task: Option<JoinHandle<std::io::Result<Capture>>>) {
    if let Some(task) = task {
        let _ = tokio::time::timeout(Duration::from_millis(250), task).await;
    }
}

fn normalize_text_capture(bytes: &[u8], total_bytes: usize, truncated: bool) -> Value {
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
        "byte_count": total_bytes,
        "truncated": truncated
    })
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn default_timeout_ms() -> u64 {
    30_000
}

fn default_max_content_bytes() -> usize {
    1024 * 1024
}

fn default_max_stderr_bytes() -> usize {
    64 * 1024
}

fn default_max_schema_depth() -> usize {
    32
}
