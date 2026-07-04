use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RedactionPolicy {
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<RedactionRule>,
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
        }
    }
}

impl RedactionPolicy {
    pub fn apply_persistence(&self, value: &Value) -> (Value, Vec<String>) {
        let mut paths = Vec::new();
        let redacted = self.apply_persistence_at(value, "", &mut paths);
        (redacted, paths)
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
        self.rules.iter().find(|rule| {
            rule.class == RedactionClass::Persistence
                && rule
                    .field_names
                    .iter()
                    .any(|candidate| normalize_field(candidate) == normalized)
        })
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
];

fn normalize_field(field: &str) -> String {
    field
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect()
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
