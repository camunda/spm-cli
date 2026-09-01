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

/// Remove every key ending in `suffix` from the object at `outer`, dropping
/// `outer` if it becomes empty. Used to purge all of spm's enabled-plugin
/// entries (keyed `<plugin>@<marketplace>`) for a given marketplace without
/// having to know each plugin's name — spm owns its whole marketplace, so any
/// `*@<marketplace>` key is spm's to remove.
pub fn remove_nested_by_suffix(root: &mut Value, outer: &str, suffix: &str) {
    let Some(map) = root.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(m)) = map.get_mut(outer) {
        m.retain(|k, _| !k.ends_with(suffix));
        if m.is_empty() {
            map.remove(outer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "spm-jsonutil-test-{name}-{}-{nanos}",
            std::process::id(),
        ))
    }

    #[test]
    fn read_object_missing_file_is_empty_object() {
        let dir = scratch("missing");
        let path = dir.join("nope.json");
        assert_eq!(read_object(&path).unwrap(), Value::Object(Map::new()));
    }

    #[test]
    fn read_object_blank_file_is_empty_object() {
        let dir = scratch("blank");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blank.json");
        std::fs::write(&path, "   \n\t \n").unwrap();
        assert_eq!(read_object(&path).unwrap(), Value::Object(Map::new()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_object_parses_valid_json() {
        let dir = scratch("valid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v.json");
        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        let v = read_object(&path).unwrap();
        assert_eq!(v["a"], 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_object_surfaces_parse_error_for_invalid_json() {
        let dir = scratch("invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "{not json").unwrap();
        let err = read_object(&path).unwrap_err();
        assert!(format!("{err:#}").contains("parsing"), "{err:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_creates_parent_dirs_and_pretty_json() {
        let dir = scratch("write");
        let path = dir.join("nested").join("out.json");
        write(&path, &json!({"k": "v"})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"k\": \"v\""), "{text}");
        assert!(text.ends_with('\n'));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn object_mut_creates_then_reuses_nested_object() {
        let mut root = Value::Object(Map::new());
        object_mut(&mut root, "outer").insert("a".into(), Value::Bool(true));
        // Second call must reuse the same nested object, not clobber it.
        object_mut(&mut root, "outer").insert("b".into(), Value::Bool(false));
        let outer = root["outer"].as_object().unwrap();
        assert_eq!(outer.len(), 2);
    }

    #[test]
    fn remove_nested_drops_empty_outer_but_keeps_nonempty() {
        let mut root = json!({
            "outer": {"a": 1, "b": 2},
            "solo": {"x": 1}
        });
        // Removing one of two keys leaves `outer` present (non-empty).
        remove_nested(&mut root, "outer", "a");
        assert_eq!(root["outer"].as_object().unwrap().len(), 1);
        assert!(root.get("outer").is_some());

        // Removing the last key drops `outer` entirely.
        remove_nested(&mut root, "solo", "x");
        assert!(root.get("solo").is_none(), "{root}");
    }

    #[test]
    fn remove_nested_is_a_noop_on_missing_outer_or_non_object_root() {
        let mut root = json!({"other": {"z": 1}});
        // `outer` key doesn't exist at all: no-op, no panic.
        remove_nested(&mut root, "outer", "inner");
        assert_eq!(root, json!({"other": {"z": 1}}));

        // Root is not even an object: still a no-op.
        let mut not_object = Value::String("hi".into());
        remove_nested(&mut not_object, "outer", "inner");
        assert_eq!(not_object, Value::String("hi".into()));
    }
}
