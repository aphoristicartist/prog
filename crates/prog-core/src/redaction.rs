use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Extra;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RedactionPolicy {
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<RedactionRule>,
    /// Normalized field names that are never redacted even when they would
    /// match a keyword (e.g. `max_tokens`, `session_timeout`). Wins over any
    /// rule match, so benign fields whose names happen to contain a secret
    /// keyword can be exempted without narrowing the matcher.
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// When true (default), string values are scanned for high-confidence
    /// embedded secret shapes (Bearer tokens, PEM blocks, JWTs, sensitive URL
    /// query params, `name=secret` / `name:secret` pairs), so a secret living
    /// under a benign key is still redacted before persistence.
    #[serde(default = "default_true")]
    pub scan_values: bool,
    /// When true, low-confidence value-secret shapes (ambiguous long
    /// base64/JWT-like blobs that are not clearly a known secret shape) are
    /// ALSO redacted. Defaults to false: such shapes are preserved verbatim
    /// and only *flagged* via observation metadata (`value_scan.lossy`), so a
    /// value the scanner is unsure about is never silently mutated. High-
    /// confidence shapes are always redacted regardless of this flag.
    #[serde(default)]
    pub redact_low_confidence_values: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RedactionRule {
    pub name: String,
    pub class: RedactionClass,
    #[serde(default)]
    pub field_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RedactionClass {
    Persistence,
    Display,
    Expansion,
}

/// Per-source redaction tuning, attached to a `SourceProfile`. Allows callers
/// to widen redaction (extra keywords), narrow it (allowlist), or replace the
/// default keyword set entirely — without code changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RedactionConfig {
    /// Extra sensitive field names added on top of the defaults. Ignored when
    /// `keywords` is `Some`.
    #[serde(default)]
    pub extra_keywords: Vec<String>,
    /// Field names never redacted, added to the built-in allowlist.
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// Replace the default keyword set entirely when set; `None` keeps the
    /// defaults and merges `extra_keywords`.
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    /// Scan string values for embedded secret shapes (Bearer, PEM, JWT,
    /// sensitive URL params). Defaults to true.
    #[serde(default = "default_true")]
    pub scan_values: bool,
    /// Also redact low-confidence value-secret shapes (ambiguous long blobs).
    /// Defaults to false (preserve-and-flag via observation metadata).
    #[serde(default)]
    pub redact_low_confidence_values: bool,
    #[serde(default)]
    pub extra: Extra,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            extra_keywords: Vec::new(),
            allowlist: Vec::new(),
            keywords: None,
            scan_values: true,
            redact_low_confidence_values: false,
            extra: Extra::new(),
        }
    }
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            rules: vec![RedactionRule {
                name: "secret_field".to_string(),
                class: RedactionClass::Persistence,
                field_names: DEFAULT_SECRET_FIELDS
                    .iter()
                    .map(|field| field.to_string())
                    .collect(),
            }],
            allowlist: DEFAULT_ALLOWLIST
                .iter()
                .map(|field| normalize_field(field))
                .collect(),
            scan_values: true,
            redact_low_confidence_values: false,
        }
    }
}

impl RedactionPolicy {
    pub fn apply_persistence(&self, value: &Value) -> (Value, Vec<String>) {
        let detail = self.apply_persistence_detailed(value);
        (detail.value, detail.redacted_paths)
    }

    /// Like [`apply_persistence`](Self::apply_persistence) but also reports the
    /// value-scan outcome: how many high-confidence value-secrets were redacted
    /// and how many low-confidence secret-like shapes were observed (and, unless
    /// `redact_low_confidence_values` is set, preserved verbatim). Pure: no I/O,
    /// no global state (stays a valid Kani target alongside `apply_persistence`).
    pub fn apply_persistence_detailed(&self, value: &Value) -> RedactionDetail {
        let mut redacted_paths = Vec::new();
        let mut low_confidence_paths = Vec::new();
        let mut report = ValueScanReport::ZERO;
        let value = self.apply_persistence_at(
            value,
            "",
            &mut redacted_paths,
            &mut low_confidence_paths,
            &mut report,
        );
        RedactionDetail {
            value,
            redacted_paths,
            low_confidence_paths,
            value_scan: report,
        }
    }

    /// Default persistence policy extended with one extra Persistence-class
    /// rule covering `extra` field names (for example, an operation's declared
    /// `sensitive_args`). The default keyword rule is retained, so both
    /// keyword-substring matches and explicitly declared names are redacted.
    pub fn with_extra_persistence_names(extra: &[String]) -> Self {
        let mut policy = Self::default();
        if !extra.is_empty() {
            policy.rules.push(RedactionRule {
                name: "declared_sensitive".to_string(),
                class: RedactionClass::Persistence,
                field_names: extra.to_vec(),
            });
        }
        policy
    }

    /// Build a persistence policy from a per-source `RedactionConfig`. The
    /// built-in allowlist is always applied on top of any caller allowlist so
    /// benign token/session fields stay visible by default.
    pub fn from_config(config: &RedactionConfig) -> Self {
        let field_names: Vec<String> = match &config.keywords {
            Some(keywords) => keywords.clone(),
            None => DEFAULT_SECRET_FIELDS
                .iter()
                .map(|field| field.to_string())
                .chain(config.extra_keywords.iter().cloned())
                .collect(),
        };
        let mut allowlist: Vec<String> = DEFAULT_ALLOWLIST
            .iter()
            .map(|field| normalize_field(field))
            .collect();
        allowlist.extend(config.allowlist.iter().map(|field| normalize_field(field)));
        Self {
            version: 1,
            rules: vec![RedactionRule {
                name: "secret_field".to_string(),
                class: RedactionClass::Persistence,
                field_names,
            }],
            allowlist,
            scan_values: config.scan_values,
            redact_low_confidence_values: config.redact_low_confidence_values,
        }
    }

    fn apply_persistence_at(
        &self,
        value: &Value,
        path: &str,
        redacted_paths: &mut Vec<String>,
        low_confidence_paths: &mut Vec<String>,
        report: &mut ValueScanReport,
    ) -> Value {
        match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        self.apply_persistence_at(
                            item,
                            &push_path(path, &index.to_string()),
                            redacted_paths,
                            low_confidence_paths,
                            report,
                        )
                    })
                    .collect(),
            ),
            Value::Object(map) => {
                let mut output = Map::new();
                for (key, child) in map {
                    let child_path = push_path(path, key);
                    if let Some(rule) = self.persistence_rule_for_field(key) {
                        redacted_paths.push(child_path);
                        output.insert(
                            key.clone(),
                            Value::String(format!("[REDACTED:{}]", rule.name)),
                        );
                    } else {
                        output.insert(
                            key.clone(),
                            self.apply_persistence_at(
                                child,
                                &child_path,
                                redacted_paths,
                                low_confidence_paths,
                                report,
                            ),
                        );
                    }
                }
                Value::Object(output)
            }
            Value::String(text) if self.scan_values => match classify_value_secret(text) {
                ValueSecretClass::High => {
                    redacted_paths.push(path.to_string());
                    report.high_confidence_redactions += 1;
                    Value::String("[REDACTED:value_secret]".to_string())
                }
                ValueSecretClass::Low => {
                    report.low_confidence_observations += 1;
                    low_confidence_paths.push(path.to_string());
                    if self.redact_low_confidence_values {
                        redacted_paths.push(path.to_string());
                        Value::String("[REDACTED:value_secret_low_confidence]".to_string())
                    } else {
                        // Preserve verbatim: only observed, never silently mutated
                        // without the explicit `redact_low_confidence_values` flag.
                        Value::String(text.clone())
                    }
                }
                ValueSecretClass::None => Value::String(text.clone()),
            },
            scalar => scalar.clone(),
        }
    }

    fn persistence_rule_for_field(&self, field: &str) -> Option<&RedactionRule> {
        let normalized = normalize_field(field);
        if self.is_allowlisted(&normalized) {
            return None;
        }
        self.rules.iter().find(|rule| {
            rule.class == RedactionClass::Persistence
                && rule
                    .field_names
                    .iter()
                    .any(|candidate| field_matches_keyword(&normalized, candidate))
        })
    }

    fn is_allowlisted(&self, normalized_field: &str) -> bool {
        self.allowlist
            .iter()
            .any(|allowed| normalize_field(allowed) == normalized_field)
    }
}

const DEFAULT_SECRET_FIELDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "credential",
    "private_key",
    "session",
    "cookie",
    "bearer",
    "pwd",
    "access_key",
    "signing_key",
];

/// Benign field names that would otherwise match a default keyword. Compared
/// in normalized (separator-stripped, lowercased) form, so a single entry such
/// as `max_tokens` also exempts `maxTokens`, `MAX_TOKENS`, and `max-tokens`.
const DEFAULT_ALLOWLIST: &[&str] = &[
    "max_tokens",
    "total_tokens",
    "token_count",
    "tokenizer",
    "tokenization",
    "session_timeout",
    "session_count",
    "cookie_consent",
    "secretary",
    "secretary_email",
];

fn normalize_field(field: &str) -> String {
    field
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A field is sensitive when a configured keyword appears as a substring of
/// its normalized form. Substring matching is intentionally conservative:
/// over-redaction is safe, under-redaction leaks. Compound names like
/// `access_token`, `client_secret`, `refresh_token`, and `x_api_key` are
/// therefore caught by the default keywords `token`, `secret`, and `apikey`.
/// Benign collisions (e.g. `max_tokens`, `session_timeout`) are handled by the
/// allowlist rather than by narrowing the matcher, so a compound-within-a-
/// segment name like `mytoken_field` is still redacted.
fn field_matches_keyword(normalized_field: &str, candidate: &str) -> bool {
    let candidate = normalize_field(candidate);
    !candidate.is_empty() && normalized_field.contains(&candidate)
}

/// True when `name` looks secret-bearing: any default secret keyword appears
/// as a substring of the normalized name, unless the name is in the built-in
/// allowlist. Adapters use this to decide which argv elements and flag values
/// to redact. Conservative by design.
pub fn is_sensitive_name(name: &str) -> bool {
    let normalized = normalize_field(name);
    if DEFAULT_ALLOWLIST
        .iter()
        .any(|allowed| normalize_field(allowed) == normalized)
    {
        return false;
    }
    DEFAULT_SECRET_FIELDS
        .iter()
        .any(|keyword| field_matches_keyword(&normalized, keyword))
}

pub fn redact_sensitive_text(input: &str) -> (String, usize) {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut search = 0usize;
    let mut redactions = 0usize;

    while let Some(range) = next_sensitive_text_value(input, search) {
        output.push_str(&input[cursor..range.value_start]);
        output.push_str("[REDACTED:observed_text_secret]");
        cursor = range.value_end;
        search = range.value_end;
        redactions += 1;
    }

    output.push_str(&input[cursor..]);
    (output, redactions)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextSecretRange {
    value_start: usize,
    value_end: usize,
}

fn next_sensitive_text_value(input: &str, search: usize) -> Option<TextSecretRange> {
    let mut index = search.min(input.len());
    while index < input.len() {
        let (key_start, ch) = next_char(input, index)?;
        index = key_start + ch.len_utf8();
        if !is_text_key_char(ch) || previous_char(input, key_start).is_some_and(is_text_key_char) {
            continue;
        }

        let key_end = consume_while(input, index, is_text_key_char);
        let key = &input[key_start..key_end];
        if !is_sensitive_text_key(key) {
            index = key_end;
            continue;
        }

        let (separator, after_separator) = match text_secret_separator(input, key_end) {
            Some(separator) => separator,
            None => {
                index = key_end;
                continue;
            }
        };
        let value_start = skip_horizontal_whitespace(input, after_separator);
        if value_start >= input.len() || matches!(input.as_bytes()[value_start], b'\n' | b'\r') {
            index = key_end;
            continue;
        }
        let value_end = if redacts_to_line_end(key, separator) {
            line_end(input, value_start)
        } else {
            token_end(input, value_start)
        };
        if value_end > value_start {
            return Some(TextSecretRange {
                value_start,
                value_end,
            });
        }
        index = key_end;
    }
    None
}

fn text_secret_separator(input: &str, key_end: usize) -> Option<(char, usize)> {
    let after_spaces = skip_horizontal_whitespace(input, key_end);
    let (separator_index, separator) = next_char(input, after_spaces)?;
    match separator {
        '=' | ':' => Some((separator, separator_index + separator.len_utf8())),
        ch if after_spaces > key_end && !matches!(ch, '\n' | '\r') => Some((' ', after_spaces)),
        _ => None,
    }
}

fn is_sensitive_text_key(key: &str) -> bool {
    let normalized = normalize_field(key);
    is_sensitive_name(key)
        || matches!(
            normalized.as_str(),
            "setcookie" | "proxyauthorization" | "accesstoken" | "refreshtoken" | "sessionid"
        )
}

fn redacts_to_line_end(key: &str, separator: char) -> bool {
    let normalized = normalize_field(key);
    separator == ':'
        && (normalized.contains("authorization")
            || normalized.contains("cookie")
            || normalized == "bearer")
}

fn skip_horizontal_whitespace(input: &str, mut index: usize) -> usize {
    while let Some((next, ch)) = next_char(input, index) {
        if !matches!(ch, ' ' | '\t') {
            break;
        }
        index = next + ch.len_utf8();
    }
    index
}

fn consume_while(input: &str, mut index: usize, predicate: fn(char) -> bool) -> usize {
    while let Some((next, ch)) = next_char(input, index) {
        if !predicate(ch) {
            break;
        }
        index = next + ch.len_utf8();
    }
    index
}

fn token_end(input: &str, start: usize) -> usize {
    consume_while(input, start, |ch| !ch.is_whitespace())
}

fn line_end(input: &str, start: usize) -> usize {
    consume_while(input, start, |ch| !matches!(ch, '\n' | '\r'))
}

fn is_text_key_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn next_char(input: &str, index: usize) -> Option<(usize, char)> {
    input[index..]
        .char_indices()
        .next()
        .map(|(offset, ch)| (index + offset, ch))
}

fn previous_char(input: &str, index: usize) -> Option<char> {
    input[..index].chars().next_back()
}

fn push_path(base: &str, segment: &str) -> String {
    if base.is_empty() {
        format!("/{}", escape(segment))
    } else {
        format!("{base}/{}", escape(segment))
    }
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn default_true() -> bool {
    true
}

/// Outcome of scanning string values for embedded secret shapes during a
/// persistence redaction pass. Engine-internal: it is carried out of the pure
/// redaction layer via [`RedactionDetail`] and surfaced (additively) in
/// observation metadata, but it is NOT a public JSON contract type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueScanReport {
    /// Distinctive secret shapes redacted from string values (Bearer, PEM,
    /// JWT, sensitive URL param, sensitive `name=secret` / `name:secret` pair).
    pub high_confidence_redactions: u32,
    /// Ambiguous long base64/JWT-like blobs observed. Redacted only when
    /// `redact_low_confidence_values` is set; otherwise preserved verbatim.
    pub low_confidence_observations: u32,
}

impl ValueScanReport {
    /// Empty report (nothing observed).
    pub const ZERO: Self = Self {
        high_confidence_redactions: 0,
        low_confidence_observations: 0,
    };

    /// True when at least one low-confidence secret-like shape was observed.
    /// This is the lossy signal surfaced to observation metadata
    /// (`parser.lossy` / `confidence`).
    pub fn lossy(&self) -> bool {
        self.low_confidence_observations > 0
    }
}

/// Result of [`RedactionPolicy::apply_persistence_detailed`]: the redacted
/// value, the paths that were actually mutated (key-name + high-confidence
/// value-secret + optional low-confidence value-secret), the low-confidence
/// paths observed, and the value-scan report.
#[derive(Debug, Clone, PartialEq)]
pub struct RedactionDetail {
    pub value: Value,
    pub redacted_paths: Vec<String>,
    pub low_confidence_paths: Vec<String>,
    pub value_scan: ValueScanReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueSecretClass {
    /// Distinctive secret shape. Always redacted.
    High,
    /// Ambiguous long blob. Observed-and-flagged; redacted only when
    /// `redact_low_confidence_values` is set.
    Low,
    /// No secret shape detected.
    None,
}

/// Classify a string value's embedded-secret content. High-confidence shapes
/// (Bearer, PEM, JWT, sensitive URL param, sensitive name=secret/name:secret
/// pair) are always redacted; a long ambiguous blob is low-confidence and, by
/// default, only flagged. Hand-rolled (no regex) so this module stays
/// dependency-free for Kani.
pub(crate) fn classify_value_secret(text: &str) -> ValueSecretClass {
    if contains_bearer_token(text)
        || contains_pem_block(text)
        || contains_jwt(text)
        || contains_sensitive_url_param(text)
        || contains_sensitive_name_value_pair(text)
    {
        ValueSecretClass::High
    } else if contains_ambiguous_long_blob(text) {
        ValueSecretClass::Low
    } else {
        ValueSecretClass::None
    }
}

/// High-confidence: a token whose normalized form is a sensitive name
/// ([`is_sensitive_name`]), followed by an `=` or `:` separator, followed by a
/// value of length >= 8 that is not a bare path (no `/` in its first segment).
/// Mirrors the `?`/`&`-anchored URL-parameter matcher but for free-form
/// `name=secret` / `name: secret` pairs embedded in a value (e.g. a `command`
/// value quoting `Authorization: Bearer …`, or a `config` blob
/// `accessToken=…`). Reuses [`is_sensitive_name`] so this stays in lockstep
/// with key-name matching.
fn contains_sensitive_name_value_pair(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !is_value_name_byte(bytes[index]) || index > 0 && is_value_name_byte(bytes[index - 1]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_value_name_byte(bytes[index]) {
            index += 1;
        }
        // ASCII-only run => slicing on byte boundaries is char-safe.
        let name = &text[start..index];
        if !is_sensitive_name(name) {
            continue;
        }
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            index += 1;
        }
        if index >= bytes.len() || !matches!(bytes[index], b'=' | b':') {
            continue;
        }
        index += 1;
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            index += 1;
        }
        let value_start = index;
        while index < bytes.len()
            && !matches!(
                bytes[index],
                b' ' | b'\t'
                    | b'\n'
                    | b'\r'
                    | b'"'
                    | b'\''
                    | b'<'
                    | b'>'
                    | b','
                    | b'}'
                    | b']'
                    | b')'
            )
        {
            index += 1;
        }
        // Idempotency: a value that is itself an already-redacted marker (e.g.
        // a text-redacted `name=[REDACTED:observed_text_secret]`) is not a
        // secret and must not be reclassified or re-redacted.
        if text[value_start..].starts_with("[REDACTED") {
            continue;
        }
        // Value must be at least 8 bytes and not look like a bare path.
        if index < value_start + 8 {
            continue;
        }
        if text.as_bytes()[value_start..value_start + 8].contains(&b'/') {
            continue;
        }
        return true;
    }
    false
}

fn is_value_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Low-confidence: a contiguous run of >= 40 base64/JWT-like characters
/// (`[A-Za-z0-9+/=_-]`) that is not itself a JWT. Genuinely ambiguous (could
/// be a hash, an image payload, or a secret), so it is observed-and-flagged
/// rather than silently redacted by default. Bearer/PEM/JWT shapes are caught
/// as [`ValueSecretClass::High`] first, so they never reach here.
fn contains_ambiguous_long_blob(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !is_blob_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_blob_byte(bytes[index]) {
            index += 1;
        }
        if index - start >= 40 {
            let run = &text[start..index];
            if !contains_jwt(run) {
                return true;
            }
        }
    }
    false
}

fn is_blob_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn contains_bearer_token(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for pos in lower.match_indices("bearer ").map(|(index, _)| index) {
        let rest = &lower[pos + "bearer ".len()..];
        let token_len = rest
            .bytes()
            .take_while(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+' | b'=')
            })
            .count();
        if token_len >= 16 {
            return true;
        }
    }
    false
}

fn contains_pem_block(text: &str) -> bool {
    text.contains("-----BEGIN ")
        && (text.contains("KEY-----")
            || text.contains("CERTIFICATE-----")
            || text.contains("PARAMS-----"))
}

fn contains_jwt(text: &str) -> bool {
    for pos in text.match_indices("eyJ").map(|(index, _)| index) {
        let rest = &text[pos..];
        let Some(seg1_end) = rest.bytes().position(|byte| byte == b'.') else {
            continue;
        };
        let after1 = &rest[seg1_end + 1..];
        if !after1.starts_with("eyJ") {
            continue;
        }
        let Some(seg2_end) = after1.bytes().position(|byte| byte == b'.') else {
            continue;
        };
        let after2 = &after1[seg2_end + 1..];
        let sig_len = after2
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'='))
            .count();
        if sig_len >= 8 {
            return true;
        }
    }
    false
}

fn contains_sensitive_url_param(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for sep in [b'?', b'&'] {
        for pos in lower
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == sep)
            .map(|(index, _)| index)
        {
            let rest = &lower[pos + 1..];
            let Some(eq_pos) = rest.bytes().position(|byte| byte == b'=') else {
                continue;
            };
            let name = &rest[..eq_pos];
            if name.is_empty()
                || name.len() > 64
                || name
                    .bytes()
                    .any(|byte| matches!(byte, b'&' | b'?' | b'/' | b' ' | b'#'))
            {
                continue;
            }
            if !is_sensitive_name(name) {
                continue;
            }
            let value = &rest[eq_pos + 1..];
            let value_len = value
                .bytes()
                .take_while(|byte| !matches!(*byte, b'&' | b' ' | b'"' | b'\'' | b'<' | b'>'))
                .count();
            if value_len >= 8 {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn name_value_pair_detects_equals_form() {
        assert!(contains_sensitive_name_value_pair(
            "accessToken=topsecretvalue1234"
        ));
    }

    #[test]
    fn name_value_pair_detects_colon_form() {
        assert!(contains_sensitive_name_value_pair(
            "password: hunter2xxxxxxx"
        ));
    }

    #[test]
    fn name_value_pair_rejects_benign_name() {
        assert!(!contains_sensitive_name_value_pair("username=johndoe12345"));
    }

    #[test]
    fn name_value_pair_rejects_short_value() {
        assert!(!contains_sensitive_name_value_pair("token=abc"));
    }

    #[test]
    fn name_value_pair_rejects_path_value() {
        // Bare path under a sensitive name is not a secret (mirrors bearer-path
        // exclusion): no `/` in the value's first segment.
        assert!(!contains_sensitive_name_value_pair(
            "key=/usr/local/bin/tool"
        ));
    }

    #[test]
    fn classify_high_for_known_shapes() {
        assert_eq!(
            classify_value_secret("Authorization: Bearer mF_9.B5f-4.1Zxoqw"),
            ValueSecretClass::High
        );
        assert_eq!(
            classify_value_secret("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----"),
            ValueSecretClass::High
        );
        assert_eq!(
            classify_value_secret("eyJhbGci.eyJzdWI.signatureabcd"),
            ValueSecretClass::High
        );
        assert_eq!(
            classify_value_secret("https://api.example.com/data?api_key=sk-live-1234"),
            ValueSecretClass::High
        );
        assert_eq!(
            classify_value_secret("config: accessToken=topsecretvalue1234"),
            ValueSecretClass::High
        );
    }

    #[test]
    fn classify_low_for_ambiguous_long_blob() {
        // 46 base64-like chars, no jwt/bearer/pem shape.
        let blob = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(classify_value_secret(blob), ValueSecretClass::Low);
    }

    #[test]
    fn classify_none_for_benign_long_sentence() {
        assert_eq!(
            classify_value_secret("the quick brown fox jumps over the lazy dog again and again"),
            ValueSecretClass::None
        );
    }

    #[test]
    fn redaction_markers_are_not_reclassified() {
        // I4 extension: an already-redacted marker is never itself reclassified
        // as a secret, so redaction is idempotent.
        assert_eq!(
            classify_value_secret("[REDACTED:value_secret]"),
            ValueSecretClass::None
        );
        assert_eq!(
            classify_value_secret("[REDACTED:value_secret_low_confidence]"),
            ValueSecretClass::None
        );
        assert_eq!(
            classify_value_secret("[REDACTED:secret_field]"),
            ValueSecretClass::None
        );
        // A text-redacted `name=[REDACTED:observed_text_secret]` line must not
        // be re-caught by the name=secret value matcher (idempotency).
        assert_eq!(
            classify_value_secret("token=[REDACTED:observed_text_secret]"),
            ValueSecretClass::None
        );
        assert!(!contains_sensitive_name_value_pair(
            "api-key: [REDACTED:observed_text_secret]"
        ));
    }

    #[test]
    fn apply_persistence_detailed_counts_high_and_low_separately() {
        let policy = RedactionPolicy::default();
        let value = json!({
            "note": "accessToken=topsecretvalue1234",                          // high: name pair
            "blob": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",          // low: ambiguous blob
            "benign": "the quick brown fox jumps over"
        });
        let detail = policy.apply_persistence_detailed(&value);
        assert_eq!(detail.value_scan.high_confidence_redactions, 1);
        assert_eq!(detail.value_scan.low_confidence_observations, 1);
        assert!(detail.value_scan.lossy());
        assert_eq!(detail.redacted_paths, vec!["/note".to_string()]);
        assert_eq!(detail.low_confidence_paths, vec!["/blob".to_string()]);
        // Low-confidence is preserved verbatim by default; high is redacted.
        assert_eq!(
            detail.value["blob"],
            json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
        assert_eq!(detail.value["note"], json!("[REDACTED:value_secret]"));
    }

    #[test]
    fn low_confidence_blob_redacted_when_flag_set() {
        let policy = RedactionPolicy {
            redact_low_confidence_values: true,
            ..RedactionPolicy::default()
        };
        let value = json!({ "blob": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" });
        let detail = policy.apply_persistence_detailed(&value);
        assert_eq!(detail.value_scan.low_confidence_observations, 1);
        assert!(detail.redacted_paths.contains(&"/blob".to_string()));
        assert_eq!(
            detail.value["blob"],
            json!("[REDACTED:value_secret_low_confidence]")
        );
    }

    #[test]
    fn lossy_flag_false_when_only_high_confidence_hits() {
        let policy = RedactionPolicy::default();
        let detail = policy.apply_persistence_detailed(&json!({
            "note": "accessToken=topsecretvalue1234"
        }));
        assert!(!detail.value_scan.lossy());
        assert_eq!(detail.value_scan.high_confidence_redactions, 1);
    }

    #[test]
    fn default_config_keeps_low_confidence_flag_false() {
        assert!(!RedactionConfig::default().redact_low_confidence_values);
        assert!(!RedactionPolicy::default().redact_low_confidence_values);
    }

    #[test]
    fn config_round_trips_low_confidence_flag() {
        let config = RedactionConfig {
            extra_keywords: Vec::new(),
            allowlist: Vec::new(),
            keywords: None,
            scan_values: true,
            redact_low_confidence_values: true,
            extra: Extra::new(),
        };
        let serialized = serde_json::to_string(&config).unwrap();
        let back: RedactionConfig = serde_json::from_str(&serialized).unwrap();
        assert!(back.redact_low_confidence_values);
        // Backward compat: `{}` deserializes with the flag defaulted to false.
        let empty: RedactionConfig = serde_json::from_str("{}").unwrap();
        assert!(!empty.redact_low_confidence_values);
    }

    #[test]
    fn name_value_pair_composes_with_key_name_redaction() {
        let policy = RedactionPolicy::default();
        let value = json!({
            "password": "x",                       // key-name redaction
            "note": "password: hunter2xxxxxxx"     // value name-pair redaction
        });
        let (redacted, paths) = policy.apply_persistence(&value);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"/password".to_string()));
        assert!(paths.contains(&"/note".to_string()));
        assert_eq!(redacted["password"], json!("[REDACTED:secret_field]"));
        assert_eq!(redacted["note"], json!("[REDACTED:value_secret]"));
    }

    #[test]
    fn apply_persistence_detailed_is_pure_and_idempotent() {
        let policy = RedactionPolicy::default();
        let value = json!({ "note": "accessToken=topsecretvalue1234" });
        let first = policy.apply_persistence_detailed(&value);
        let second = policy.apply_persistence_detailed(&first.value);
        // Re-applying to the already-redacted value changes nothing and observes
        // no further secrets (markers are not reclassified).
        assert_eq!(second.value, first.value);
        assert_eq!(second.value_scan, ValueScanReport::ZERO);
    }
}
