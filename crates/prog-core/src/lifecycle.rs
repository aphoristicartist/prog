use std::{marker::PhantomData, ops::Deref};

use serde_json::Value;

use crate::{
    CoreError, CursorRecord, RedactionPolicy, Result, SliceRequest, ValueScanReport,
    pointer::{self, is_within},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raw;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Redacted;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Persisted;

#[derive(Debug, Clone, PartialEq)]
pub struct Payload<State> {
    value: Value,
    _state: PhantomData<State>,
}

pub type RawPayload = Payload<Raw>;
pub type RedactedPayload = Payload<Redacted>;
pub type PersistedPayload = Payload<Persisted>;

#[derive(Debug, Clone, PartialEq)]
pub struct RedactionOutcome {
    pub payload: RedactedPayload,
    pub redacted_paths: Vec<String>,
    /// Value-scan outcome: high-confidence redactions and low-confidence
    /// observations (the lossy signal surfaced to observation metadata).
    pub value_scan: ValueScanReport,
}

impl RawPayload {
    pub fn new(value: Value) -> Self {
        Self {
            value,
            _state: PhantomData,
        }
    }

    pub fn redact(self, policy: &RedactionPolicy) -> RedactionOutcome {
        let detail = policy.apply_persistence_detailed(&self.value);
        RedactionOutcome {
            payload: RedactedPayload::from_redacted_value(detail.value),
            redacted_paths: detail.redacted_paths,
            value_scan: detail.value_scan,
        }
    }
}

impl RedactedPayload {
    pub(crate) fn from_redacted_value(value: Value) -> Self {
        Self {
            value,
            _state: PhantomData,
        }
    }
}

impl PersistedPayload {
    pub(crate) fn from_store(value: Value) -> Self {
        Self {
            value,
            _state: PhantomData,
        }
    }

    pub fn into_redacted(self) -> RedactedPayload {
        RedactedPayload::from_redacted_value(self.value)
    }
}

impl<State> Payload<State> {
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }
}

pub trait ExpandablePayload {
    fn expansion_value(&self) -> &Value;
}

impl ExpandablePayload for RedactedPayload {
    fn expansion_value(&self) -> &Value {
        self.as_value()
    }
}

impl ExpandablePayload for PersistedPayload {
    fn expansion_value(&self) -> &Value {
        self.as_value()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPointer(String);

impl JsonPointer {
    pub fn root() -> Self {
        Self(String::new())
    }

    pub fn parse(value: &str) -> Result<Self> {
        validate_json_pointer(value)?;
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionScope {
    root: JsonPointer,
}

impl ExpansionScope {
    pub fn root() -> Self {
        Self {
            root: JsonPointer::root(),
        }
    }

    pub fn new(root_path: &str) -> Result<Self> {
        Ok(Self {
            root: JsonPointer::parse(root_path)?,
        })
    }

    pub fn from_cursor(cursor: &ValidatedCursor) -> Result<Self> {
        Self::new(&cursor.record.root_path)
    }

    pub fn root_path(&self) -> &JsonPointer {
        &self.root
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopedSlice {
    scope: ExpansionScope,
    request: SliceRequest,
    target_path: JsonPointer,
}

impl ScopedSlice {
    pub fn new(scope: ExpansionScope, request: SliceRequest) -> Result<Self> {
        let target_raw = request.path.as_deref().unwrap_or(scope.root.as_str());
        let target_path = JsonPointer::parse(target_raw)?;
        if !is_within(scope.root.as_str(), target_path.as_str())? {
            return Err(CoreError::PathOutsideBoundary {
                path: target_path.as_str().to_string(),
                boundary: scope.root.as_str().to_string(),
            });
        }
        Ok(Self {
            scope,
            request,
            target_path,
        })
    }

    pub fn root(request: SliceRequest) -> Result<Self> {
        Self::new(ExpansionScope::root(), request)
    }

    pub fn request(&self) -> &SliceRequest {
        &self.request
    }

    pub fn root_path(&self) -> &JsonPointer {
        self.scope.root_path()
    }

    pub fn target_path(&self) -> &JsonPointer {
        &self.target_path
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedCursor {
    token: String,
    record: CursorRecord,
}

impl ValidatedCursor {
    pub(crate) fn new(token: String, record: CursorRecord) -> Self {
        Self { token, record }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn record(&self) -> &CursorRecord {
        &self.record
    }
}

impl Deref for ValidatedCursor {
    type Target = CursorRecord;

    fn deref(&self) -> &Self::Target {
        &self.record
    }
}

fn validate_json_pointer(value: &str) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let Some(rest) = value.strip_prefix('/') else {
        return Err(CoreError::BadPointer(value.to_string()));
    };
    for segment in rest.split('/') {
        let bytes = segment.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] == b'~' {
                let Some(next) = bytes.get(index + 1) else {
                    return Err(CoreError::BadPointer(value.to_string()));
                };
                if !matches!(next, b'0' | b'1') {
                    return Err(CoreError::BadPointer(value.to_string()));
                }
                index += 2;
            } else {
                index += 1;
            }
        }
    }
    pointer::parse(value).map(|_| ())
}
