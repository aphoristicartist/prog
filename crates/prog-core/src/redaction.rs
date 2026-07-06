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
    #[serde(default)]
    pub extra: Extra,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            extra_keywords: Vec::new(),
            allowlist: Vec::new(),
            keywords: None,
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
    "tokens",
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
