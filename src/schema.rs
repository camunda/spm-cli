//! JSON Schema validation for `ai.json`. The schema is the single source of
//! truth for the manifest's shape; it's embedded in the binary and also usable
//! by editors via a `$schema` reference.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// The canonical `ai.json` schema, compiled into the binary.
pub const SOURCE: &str = include_str!("../schema/ai.schema.json");

fn document() -> Result<Value> {
    serde_json::from_str(SOURCE).context("embedded ai.json schema is not valid JSON")
}

/// The vendors `targets` accepts: the schema's `targets` enum, which
/// `vendor::ALL_TARGETS` is pinned to.
pub fn targets() -> Result<Vec<String>> {
    document()?["properties"]["targets"]["items"]["enum"]
        .as_array()
        .and_then(|vals| {
            vals.iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| anyhow!("embedded ai.json schema has no `targets` enum"))
}

/// Validate a parsed `ai.json` document against the embedded schema, collecting
/// every violation into one readable error.
pub fn validate(instance: &Value) -> Result<()> {
    let schema = document()?;
    // The compile/validate error types borrow `schema`, so render them to owned
    // strings before propagating (anyhow errors must be 'static).
    let compiled = jsonschema::JSONSchema::compile(&schema)
        .map_err(|e| anyhow!("embedded ai.json schema is invalid: {e}"))?;

    if let Err(errors) = compiled.validate(instance) {
        let details: Vec<String> = errors
            .map(|e| {
                let at = e.instance_path.to_string();
                let at = if at.is_empty() { "(root)".into() } else { at };
                format!("  at {at}: {e}")
            })
            .collect();
        bail!("ai.json does not match schema:\n{}", details.join("\n"));
    }
    Ok(())
}
