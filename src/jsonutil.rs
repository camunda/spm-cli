//! Small helpers for merge-patching user-owned JSON config files (Claude's
//! settings.local.json, Copilot's .vscode/settings.json) without clobbering keys
//! the user set themselves.

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::Path;

/// Read a JSON object from `path`, returning an empty object if it is missing or blank.
pub fn read_object(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Write `value` as pretty JSON, creating parent dirs as needed.
pub fn write(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
}

/// Get (or create) a nested object field on `root`.
pub fn object_mut<'a>(root: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    let map = root.as_object_mut().expect("json root is an object");
    map.entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("json field is an object")
}

/// Remove `inner` from the object at `outer`, dropping `outer` itself if it becomes empty.
pub fn remove_nested(root: &mut Value, outer: &str, inner: &str) {
    let Some(map) = root.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(m)) = map.get_mut(outer) {
        m.remove(inner);
        if m.is_empty() {
            map.remove(outer);
        }
    }
}
