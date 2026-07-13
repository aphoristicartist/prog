use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use prog_core::{
    AuthRef, CoreError, RedactionPolicy, Result, is_sensitive_name, redact_sensitive_text,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

const DEFAULT_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpSource {
    pub id: String,
    pub base_url: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
    #[serde(default)]
    pub default_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub response_header_allowlist: Vec<String>,
    #[serde(default)]
    pub auth: Vec<AuthRef>,
    #[serde(default)]
    pub operations: Vec<HttpOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpOperation {
    pub id: String,
    #[serde(default = "default_method")]
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub json_body: Option<Value>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
    #[serde(default)]
    pub sensitive_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpCallResult {
    pub data: Value,
    pub provenance: HttpProvenance,
    pub received_error: bool,
    #[serde(default)]
    pub pagination: Option<Value>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpProvenance {
    pub source_id: String,
    pub operation: String,
    pub method: String,
    pub final_url: String,
    pub status: u16,
    #[serde(default)]
    pub selected_headers: BTreeMap<String, String>,
    pub duration_ms: u64,
    pub response_bytes: usize,
    pub truncated: bool,
    #[serde(default)]
    pub args: Value,
}

impl HttpSource {
    pub async fn execute(&self, operation_id: &str, args: &Value) -> Result<HttpCallResult> {
        self.execute_with_env(operation_id, args, &|name| std::env::var(name).ok())
            .await
    }

    pub async fn execute_with_env(
        &self,
        operation_id: &str,
        args: &Value,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<HttpCallResult> {
        let operation = self
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .ok_or_else(|| CoreError::UnknownOperation {
                source_id: self.id.clone(),
                operation: operation_id.to_string(),
            })?;
        let args = args_object(operation, args)?;
        validate_args(operation, args)?;

        let timeout_ms = operation.timeout_ms.unwrap_or(self.timeout_ms);
        let max_response_bytes = operation
            .max_response_bytes
            .unwrap_or(self.max_response_bytes);
        let sensitive_names = sensitive_arg_names(operation, args);
        let url = build_url(self, operation, args)?;
        let redacted_url = redact_url(url.as_str(), args, &sensitive_names);
        let client = http_client(&operation.id)?;
        let method = Method::from_bytes(operation.method.as_bytes()).map_err(|error| {
            CoreError::BadArgs {
                operation: operation.id.clone(),
                reason: format!("invalid http method: {error}"),
            }
        })?;

        let mut request = client.request(method.clone(), url);
        for (name, value) in self.default_headers.iter().chain(operation.headers.iter()) {
            request = request.header(name, substitute_template(value, args, false)?);
        }
        for auth in &self.auth {
            if let (Some(header), Some(secret)) = (&auth.header, env(&auth.env)) {
                let value = auth
                    .format
                    .as_deref()
                    .unwrap_or("{value}")
                    .replace("{value}", &secret);
                request = request.header(header, value);
            }
        }
        if let Some(body) = &operation.json_body {
            request = request.json(&substitute_json(body, args)?);
        }

        let started = Instant::now();
        let response = tokio::time::timeout(Duration::from_millis(timeout_ms), request.send())
            .await
            .map_err(|_| CoreError::HttpTimeout {
                operation: operation.id.clone(),
                timeout_ms,
            })?
            .map_err(|error| CoreError::HttpTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?;

        let status = response.status();
        let selected_headers = selected_headers(response.headers(), self);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let link_header = response
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let (bytes, truncated) =
            read_bounded_body(response, max_response_bytes, timeout_ms, &operation.id).await?;
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let data = normalize_body(&bytes, content_type.as_deref(), truncated)?;
        // Pagination-shape detection lives in core (`extract_pagination_hints`)
        // so it is unit-testable independent of any transport. The adapter is a
        // one-line caller: body + Link header in, canonical hint shape out.
        let pagination = prog_core::extract_pagination_hints(&data, link_header.as_deref());
        let mut warnings = Vec::new();
        if truncated {
            warnings.push(format!(
                "response exceeded max_response_bytes ({max_response_bytes}); body was truncated"
            ));
        }

        let provenance = HttpProvenance {
            source_id: self.id.clone(),
            operation: operation.id.clone(),
            method: operation.method.to_uppercase(),
            final_url: redacted_url,
            status: status.as_u16(),
            selected_headers,
            duration_ms,
            response_bytes: bytes.len(),
            truncated,
            args: redacted_args(args, &operation.sensitive_args),
        };

        if !status.is_success() {
            warnings.push(format!(
                "upstream returned HTTP status {}; response captured as error evidence",
                status.as_u16()
            ));
        }

        Ok(HttpCallResult {
            data,
            provenance,
            received_error: !status.is_success(),
            pagination,
            warnings,
        })
    }

    /// Follow a literal next-page URL (RFC 5988 `Link: rel="next"` or a URL
    /// field) reusing the base operation's auth headers, timeout,
    /// `max_response_bytes`, and reqwest client, but forcing method `GET`.
    ///
    /// SSRF guard (I10): the target URL's scheme + host + port MUST match the
    /// source's `base_url`. An attacker-controlled `Link: rel="next"` header
    /// cannot redirect page chasing to an internal or third-party host. The
    /// redirect policy stays reqwest's `limited(10)`, so a same-origin
    /// redirect chain still resolves but a cross-origin one is refused here
    /// before any connection.
    pub async fn execute_url(
        &self,
        base_operation_id: &str,
        url: &str,
        args: &Value,
    ) -> Result<HttpCallResult> {
        let operation = self
            .operations
            .iter()
            .find(|operation| operation.id == base_operation_id)
            .ok_or_else(|| CoreError::UnknownOperation {
                source_id: self.id.clone(),
                operation: base_operation_id.to_string(),
            })?;

        let args = match args {
            Value::Object(map) => map.clone(),
            _ => Map::new(),
        };
        let timeout_ms = operation.timeout_ms.unwrap_or(self.timeout_ms);
        let max_response_bytes = operation
            .max_response_bytes
            .unwrap_or(self.max_response_bytes);

        let target = reqwest::Url::parse(url).map_err(|error| CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: format!("invalid pagination url: {error}"),
        })?;
        let base = reqwest::Url::parse(&self.base_url).map_err(|error| CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: format!("invalid base_url: {url} {error}"),
        })?;
        // Same-origin guard: scheme + host + port must all match the base.
        let same_origin = target.scheme() == base.scheme()
            && target.host_str() == base.host_str()
            && target.port_or_known_default() == base.port_or_known_default();
        if !same_origin {
            return Err(CoreError::BadArgs {
                operation: operation.id.clone(),
                reason:
                    "cross-host pagination continuation refused: Link rel=\"next\" target must match the source base scheme+host"
                        .to_string(),
            });
        }

        let sensitive_names = sensitive_arg_names(operation, &args);
        let client = http_client(&operation.id)?;

        // Forced GET: URL continuation never carries the base operation's
        // request body, even if the base operation was POST.
        let mut request = client.request(Method::GET, target.clone());
        for (name, value) in self.default_headers.iter().chain(operation.headers.iter()) {
            request = request.header(name, substitute_template(value, &args, false)?);
        }
        for auth in &self.auth {
            if let (Some(header), Some(secret)) = (&auth.header, std::env::var(&auth.env).ok()) {
                let value = auth
                    .format
                    .as_deref()
                    .unwrap_or("{value}")
                    .replace("{value}", &secret);
                request = request.header(header, value);
            }
        }

        let started = Instant::now();
        let response = tokio::time::timeout(Duration::from_millis(timeout_ms), request.send())
            .await
            .map_err(|_| CoreError::HttpTimeout {
                operation: operation.id.clone(),
                timeout_ms,
            })?
            .map_err(|error| CoreError::HttpTransport {
                operation: operation.id.clone(),
                message: error.to_string(),
            })?;

        let status = response.status();
        let selected_headers = selected_headers(response.headers(), self);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let link_header = response
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let (bytes, truncated) =
            read_bounded_body(response, max_response_bytes, timeout_ms, &operation.id).await?;
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let data = normalize_body(&bytes, content_type.as_deref(), truncated)?;
        let pagination = prog_core::extract_pagination_hints(&data, link_header.as_deref());
        let mut warnings = Vec::new();
        if truncated {
            warnings.push(format!(
                "response exceeded max_response_bytes ({max_response_bytes}); body was truncated"
            ));
        }

        let redacted_target = redact_url(url, &args, &sensitive_names);
        let provenance = HttpProvenance {
            source_id: self.id.clone(),
            operation: operation.id.clone(),
            method: "GET".to_string(),
            final_url: redacted_target,
            status: status.as_u16(),
            selected_headers,
            duration_ms,
            response_bytes: bytes.len(),
            truncated,
            args: redacted_args(&args, &operation.sensitive_args),
        };

        if !status.is_success() {
            warnings.push(format!(
                "upstream returned HTTP status {}; response captured as error evidence",
                status.as_u16()
            ));
        }

        Ok(HttpCallResult {
            data,
            provenance,
            received_error: !status.is_success(),
            pagination,
            warnings,
        })
    }
}

fn http_client(operation: &str) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(DEFAULT_USER_AGENT)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|error| CoreError::HttpTransport {
            operation: operation.to_string(),
            message: error.to_string(),
        })
}

fn args_object<'a>(operation: &HttpOperation, args: &'a Value) -> Result<&'a Map<String, Value>> {
    args.as_object().ok_or_else(|| CoreError::BadArgs {
        operation: operation.id.clone(),
        reason: "args must be a JSON object".to_string(),
    })
}

fn validate_args(operation: &HttpOperation, args: &Map<String, Value>) -> Result<()> {
    let referenced = referenced_args(operation);
    let missing: Vec<&String> = referenced
        .iter()
        .filter(|name| !args.contains_key(*name))
        .collect();
    let unknown: Vec<&String> = args
        .keys()
        .filter(|name| !referenced.contains(*name))
        .collect();

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

fn referenced_args(operation: &HttpOperation) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_placeholders(&operation.path, &mut names);
    for value in operation.query.values().chain(operation.headers.values()) {
        collect_placeholders(value, &mut names);
    }
    if let Some(body) = &operation.json_body {
        collect_json_placeholders(body, &mut names);
    }
    names
}

fn collect_json_placeholders(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::String(value) => collect_placeholders(value, names),
        Value::Array(values) => {
            for value in values {
                collect_json_placeholders(value, names);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_json_placeholders(value, names);
            }
        }
        _ => {}
    }
}

fn collect_placeholders(template: &str, names: &mut BTreeSet<String>) {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = &after[..end];
        if !name.is_empty() {
            names.insert(name.to_string());
        }
        rest = &after[end + 1..];
    }
}

fn build_url(
    source: &HttpSource,
    operation: &HttpOperation,
    args: &Map<String, Value>,
) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(&source.base_url).map_err(|error| CoreError::BadArgs {
        operation: operation.id.clone(),
        reason: format!("invalid base_url: {error}"),
    })?;

    let path = substitute_template(&operation.path, args, true)?;
    let base_path = url.path().trim_end_matches('/');
    let operation_path = path.trim_start_matches('/');
    let full_path = if base_path.is_empty() || base_path == "/" {
        format!("/{operation_path}")
    } else if operation_path.is_empty() {
        base_path.to_string()
    } else {
        format!("{base_path}/{operation_path}")
    };
    url.set_path(&full_path);
    {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in &operation.query {
            let value = substitute_template(value, args, false)?;
            pairs.append_pair(name, &value);
        }
    }
    Ok(url)
}

fn substitute_json(value: &Value, args: &Map<String, Value>) -> Result<Value> {
    match value {
        Value::String(template) => {
            if let Some(name) = exact_placeholder(template) {
                return Ok(args.get(name).cloned().unwrap_or(Value::Null));
            }
            Ok(Value::String(substitute_template(template, args, false)?))
        }
        Value::Array(values) => Ok(Value::Array(
            values
                .iter()
                .map(|value| substitute_json(value, args))
                .collect::<Result<Vec<_>>>()?,
        )),
        Value::Object(map) => {
            let mut output = Map::new();
            for (key, value) in map {
                output.insert(key.clone(), substitute_json(value, args)?);
            }
            Ok(Value::Object(output))
        }
        scalar => Ok(scalar.clone()),
    }
}

fn substitute_template(template: &str, args: &Map<String, Value>, encode: bool) -> Result<String> {
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
        let value = args.get(name).ok_or_else(|| CoreError::BadArgs {
            operation: "http template".to_string(),
            reason: format!("missing parameter '{name}'"),
        })?;
        let value = arg_to_string(name, value)?;
        if encode {
            output.push_str(&percent_encode(&value));
        } else {
            output.push_str(&value);
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn exact_placeholder(template: &str) -> Option<&str> {
    template
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .filter(|name| !name.is_empty() && !name.contains('{') && !name.contains('}'))
}

fn arg_to_string(name: &str, value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null => Err(CoreError::BadArgs {
            operation: "http template".to_string(),
            reason: format!("parameter '{name}' cannot be null"),
        }),
        Value::Array(_) | Value::Object(_) => Err(CoreError::BadArgs {
            operation: "http template".to_string(),
            reason: format!("parameter '{name}' must be scalar"),
        }),
    }
}

async fn read_bounded_body(
    mut response: reqwest::Response,
    max_response_bytes: usize,
    timeout_ms: u64,
    operation: &str,
) -> Result<(Vec<u8>, bool)> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let mut bytes = Vec::new();
        let mut truncated = false;
        while let Some(chunk) =
            response
                .chunk()
                .await
                .map_err(|error| CoreError::HttpTransport {
                    operation: operation.to_string(),
                    message: error.to_string(),
                })?
        {
            let remaining = max_response_bytes.saturating_sub(bytes.len());
            if remaining == 0 {
                truncated = true;
                break;
            }
            let take = chunk.len().min(remaining);
            bytes.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                truncated = true;
                break;
            }
        }
        Ok((bytes, truncated))
    })
    .await
    .map_err(|_| CoreError::HttpTimeout {
        operation: operation.to_string(),
        timeout_ms,
    })?
}

fn normalize_body(bytes: &[u8], content_type: Option<&str>, truncated: bool) -> Result<Value> {
    let is_json_content_type = content_type
        .map(|value| value.to_ascii_lowercase().contains("json"))
        .unwrap_or(false);
    let sniffed_json = bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'{' || byte == b'[');

    if (is_json_content_type || sniffed_json)
        && let Ok(value) = serde_json::from_slice(bytes)
    {
        return Ok(value);
    }

    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<Value> = text
        .lines()
        .take(20)
        .map(|line| json!(redact_sensitive_text(line).0))
        .collect();
    Ok(json!({
        "format": "text",
        "lines": lines,
        "line_count": text.lines().count(),
        "byte_count": bytes.len(),
        "truncated": truncated
    }))
}

fn selected_headers(
    headers: &reqwest::header::HeaderMap,
    source: &HttpSource,
) -> BTreeMap<String, String> {
    let allowlist: BTreeSet<String> = if source.response_header_allowlist.is_empty() {
        [
            "content-type",
            "etag",
            "last-modified",
            "x-ratelimit-remaining",
            "x-ratelimit-reset",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    } else {
        source
            .response_header_allowlist
            .iter()
            .map(|header| header.to_ascii_lowercase())
            .collect()
    };

    let mut selected = BTreeMap::new();
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if !allowlist.contains(&name) || is_sensitive_header(&name) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            selected.insert(name, value.to_string());
        }
    }
    selected
}

fn redacted_args(args: &Map<String, Value>, sensitive: &[String]) -> Value {
    let (redacted, _) = RedactionPolicy::with_extra_persistence_names(sensitive)
        .apply_persistence(&Value::Object(args.clone()));
    redacted
}

fn sensitive_arg_names(operation: &HttpOperation, args: &Map<String, Value>) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = operation.sensitive_args.iter().cloned().collect();
    for name in args.keys() {
        if is_sensitive_name(name) {
            names.insert(name.clone());
        }
    }
    names
}

fn redact_url(url: &str, args: &Map<String, Value>, sensitive_names: &BTreeSet<String>) -> String {
    let mut output = url.to_string();
    for name in sensitive_names {
        if let Some(value) = redaction_value(args.get(name)) {
            output = output.replace(&value, "[REDACTED]");
            output = output.replace(&percent_encode(&value), "[REDACTED]");
        }
    }
    output
}

fn redaction_value(value: Option<&Value>) -> Option<String> {
    let value = match value? {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    (!value.is_empty()).then_some(value)
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name,
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "x-api-key"
    )
}

fn percent_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                output.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(output, "%{byte:02X}");
            }
        }
    }
    output
}

fn default_timeout_ms() -> u64 {
    30_000
}

fn default_max_response_bytes() -> usize {
    2 * 1024 * 1024
}

fn default_method() -> String {
    "GET".to_string()
}
