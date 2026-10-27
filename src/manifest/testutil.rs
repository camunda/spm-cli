#![cfg(test)]
//! Property-test strategies shared by the tests of the `ai.json` and `ai.lock`
//! loaders: valid dependency names, and arbitrary edits of a valid document's
//! JSON or text, which turn it into the untrusted input a hand-edited or
//! hostile file can be.

use crate::ids::DepName;
use proptest::prelude::*;
use serde_json::Value;
use std::path::PathBuf;

/// A fresh scratch directory, removed however the case ends.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spm-prop-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The keys spm's JSON files use, so arbitrary objects often hit real fields.
const KEYS: &[&str] = &[
    "$schema",
    "targets",
    "resolveTransitive",
    "allowedSources",
    "skills",
    "plugins",
    "git",
    "tag",
    "branch",
    "commit",
    "version",
    "path",
    "id",
    "reference",
    "resolved_tag",
    "store",
    "bundled_skills",
    "requested_by",
    "also_satisfies",
];

/// A valid dependency name (`DepName`'s own properties cover invalid ones).
pub(crate) fn dep_name() -> impl Strategy<Value = DepName> {
    "[a-zA-Z0-9][a-zA-Z0-9 ._-]{0,8}"
        .prop_filter_map("an invalid name", |s| DepName::try_from(s).ok())
}

/// A string, often shaped like a real field value.
fn string() -> impl Strategy<Value = String> {
    prop_oneof![
        ".{0,8}",
        "[0-9a-fA-F]{40}",
        "(tag|branch|commit|version):.{0,4}",
        "(claude|copilot|gemini|\\^1\\.2|v1\\.4\\.1|github\\.com/\\*|file://\\*)",
    ]
}

/// A small arbitrary JSON value whose object keys are mostly real field names.
fn json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        string().prop_map(Value::from),
    ];
    let key = prop_oneof![prop::sample::select(KEYS).prop_map(String::from), ".{0,4}"];
    leaf.prop_recursive(3, 24, 4, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((key.clone(), inner), 0..4)
                .prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

/// The JSON pointer of every node of `value` (its own included).
fn pointers(value: &Value, at: String, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                pointers(item, format!("{at}/{i}"), out);
            }
        }
        Value::Object(map) => {
            for (k, item) in map {
                let k = k.replace('~', "~0").replace('/', "~1");
                pointers(item, format!("{at}/{k}"), out);
            }
        }
        _ => {}
    }
    out.push(at);
}

/// One arbitrary edit of `text`, a valid document: a node of its JSON removed
/// or replaced by a string or an arbitrary value, its text cut short and
/// followed by anything, or text unrelated to it.
pub(crate) fn edited(text: String) -> impl Strategy<Value = String> {
    let value: Value = serde_json::from_str(&text).unwrap();
    let mut nodes = Vec::new();
    pointers(&value, String::new(), &mut nodes);
    let len = text.len();
    let replace = {
        let value = value.clone();
        move |(at, with): (String, Value)| {
            let mut value = value.clone();
            *value.pointer_mut(&at).unwrap() = with;
            value.to_string()
        }
    };
    let remove = {
        let value = value.clone();
        move |at: String| {
            let mut value = value.clone();
            if let Some((parent, last)) = at.rsplit_once('/') {
                match value.pointer_mut(parent) {
                    Some(Value::Object(map)) => {
                        map.remove(&last.replace("~1", "/").replace("~0", "~"));
                    }
                    Some(Value::Array(items)) => {
                        items.remove(last.parse::<usize>().unwrap());
                    }
                    _ => {}
                }
            }
            value.to_string()
        }
    };
    prop_oneof![
        prop::sample::select(nodes.clone()).prop_map(remove),
        (
            prop::sample::select(nodes.clone()),
            string().prop_map(Value::from)
        )
            .prop_map(replace.clone()),
        (prop::sample::select(nodes), json()).prop_map(replace),
        (0..=len, ".{0,4}").prop_map(move |(cut, tail)| {
            let cut = (0..=cut)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(0);
            format!("{}{tail}", &text[..cut])
        }),
        any::<String>(),
    ]
}
