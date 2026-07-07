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
    /// query params), so a secret living under a benign key is still redacted
    /// before persistence.
    #[serde(default = "default_true")]
    pub scan_values: bool,
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
        }
    }
}

impl RedactionPolicy {
    pub fn apply_persistence(&self, value: &Value) -> (Value, Vec<String>) {
        let mut paths = Vec::new();
        let redacted = self.apply_persistence_at(value, "", &mut paths);
        (redacted, paths)
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
        }
    }

    fn apply_persistence_at(&self, value: &Value, path: &str, paths: &mut Vec<String>) -> Value {
        match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        self.apply_persistence_at(item, &push_path(path, &index.to_string()), paths)
                    })
                    .collect(),
            ),
            Value::Object(map) => {
                let mut output = Map::new();
                for (key, child) in map {
                    let child_path = push_path(path, key);
                    if let Some(rule) = self.persistence_rule_for_field(key) {
                        paths.push(child_path);
                        output.insert(
                            key.clone(),
                            Value::String(format!("[REDACTED:{}]", rule.name)),
                        );
                    } else {
                        output.insert(
                            key.clone(),
                            self.apply_persistence_at(child, &child_path, paths),
                        );
                    }
                }
                Value::Object(output)
            }
            Value::String(text) if self.scan_values && contains_value_secret(text) => {
                paths.push(path.to_string());
                Value::String("[REDACTED:value_secret]".to_string())
            }
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

/// True when `text` contains a high-confidence embedded secret shape. Catches
/// secrets that live in a string *value* under a benign key, which the
/// key-based matcher cannot reach. Hand-rolled (no regex) so this module stays
/// dependency-free for Kani. Intentionally high-precision: a value is only
/// flagged when it matches a distinctive shape (Bearer token, PEM block, JWT,
/// or a sensitive URL query parameter with a non-trivial value).
///
/// Note: the URL-parameter shape uses the built-in `is_sensitive_name` keyword
/// set (the defaults), independent of a source's `RedactionConfig` tuning; the
/// other shapes are keyword-free. Configurable value-scan keywords are future
/// work.
pub(crate) fn contains_value_secret(text: &str) -> bool {
    contains_bearer_token(text)
        || contains_pem_block(text)
        || contains_jwt(text)
        || contains_sensitive_url_param(text)
}

fn contains_bearer_token(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for pos in lower.match_indices("bearer ").map(|(index, _)| index) {
        let rest = &lower[pos + "bearer ".len()..];
        let token_len = rest
            .bytes()
            .take_while(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'-' | b'_' | b'+' | b'=')
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
