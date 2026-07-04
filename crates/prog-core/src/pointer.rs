//! JSON Pointer (RFC 6901) utilities.
//!
//! Paths are the address space of the disclosure lens: previews, omitted
//! regions, cursors, and expansion requests all speak JSON Pointer. Hint
//! paths may additionally use `*` as an array-wildcard segment; wildcards are
//! for display only and are never resolved against payloads.

use crate::error::{CoreError, Result};
use serde_json::Value;

/// Escape a single reference token per RFC 6901 (`~` -> `~0`, `/` -> `~1`).
pub fn escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Split a pointer into unescaped segments. Empty pointer -> no segments.
pub fn parse(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(CoreError::BadPointer(pointer.to_string()));
    };
    Ok(rest.split('/').map(unescape).collect())
}

/// Join segments back into a pointer string.
pub fn join(segments: &[String]) -> String {
    if segments.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for s in segments {
        out.push('/');
        out.push_str(&escape(s));
    }
    out
}

/// Append one segment to a pointer string.
pub fn push(pointer: &str, segment: &str) -> String {
    format!("{pointer}/{}", escape(segment))
}

/// Resolve a pointer against a value.
pub fn get<'v>(value: &'v Value, pointer: &str) -> Result<Option<&'v Value>> {
    let segments = parse(pointer)?;
    let mut cur = value;
    for seg in &segments {
        match cur {
            Value::Object(map) => match map.get(seg) {
                Some(v) => cur = v,
                None => return Ok(None),
            },
            Value::Array(items) => match seg.parse::<usize>().ok().and_then(|i| items.get(i)) {
                Some(v) => cur = v,
                None => return Ok(None),
            },
            _ => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// True when `path` is `boundary` itself or a descendant of it.
/// Compares parsed segments, so escaping differences cannot smuggle a path
/// past the check (e.g. `/a~1b` is NOT inside `/a`).
pub fn is_within(boundary: &str, path: &str) -> Result<bool> {
    let b = parse(boundary)?;
    let p = parse(path)?;
    Ok(p.len() >= b.len() && p[..b.len()] == b[..])
}

/// Nearby keys for actionable "path not found" errors.
pub fn siblings_hint(value: &Value, pointer: &str) -> String {
    let Ok(segments) = parse(pointer) else {
        return String::new();
    };
    let mut cur = value;
    let mut depth = 0usize;
    for seg in &segments {
        let next = match cur {
            Value::Object(map) => map.get(seg),
            Value::Array(items) => seg.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        };
        match next {
            Some(v) => {
                cur = v;
                depth += 1;
            }
            None => break,
        }
    }
    let reached = join(&segments[..depth]);
    match cur {
        Value::Object(map) => {
            let keys: Vec<&str> = map.keys().take(8).map(String::as_str).collect();
            format!(
                "; deepest existing ancestor is '{reached}' with keys [{}]",
                keys.join(", ")
            )
        }
        Value::Array(items) => {
            format!(
                "; deepest existing ancestor is '{reached}', an array of {} items",
                items.len()
            )
        }
        _ => format!("; deepest existing ancestor is '{reached}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_escaping() {
        let segs = vec!["a/b".to_string(), "c~d".to_string(), "plain".to_string()];
        assert_eq!(parse(&join(&segs)).unwrap(), segs);
    }

    #[test]
    fn get_resolves_nested() {
        let v = json!({"items": [{"a": 1}, {"a": 2}]});
        assert_eq!(get(&v, "/items/1/a").unwrap(), Some(&json!(2)));
        assert_eq!(get(&v, "").unwrap(), Some(&v));
        assert_eq!(get(&v, "/missing").unwrap(), None);
    }

    #[test]
    fn boundary_containment_respects_segments() {
        assert!(is_within("/a", "/a/b").unwrap());
        assert!(is_within("", "/anything").unwrap());
        assert!(is_within("/a", "/a").unwrap());
        assert!(!is_within("/a", "/ab").unwrap());
        assert!(!is_within("/a/b", "/a").unwrap());
        // '/a~1b' is the single key "a/b", not a child of "/a"
        assert!(!is_within("/a", "/a~1b").unwrap());
    }
}
