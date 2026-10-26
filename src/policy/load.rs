//! Loading the policy layers: the machine policy file and the project's
//! `allowedSources`.

use super::layer::Layer;
use super::Policy;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// The machine policy file: `{ "allowedSources": [...] }`. Unknown keys are an
/// error so a typo (say `allowedSource`) cannot silently disable the policy.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    #[serde(rename = "allowedSources")]
    allowed_sources: Vec<String>,
}

impl Policy {
    /// The policy for a command operating on the `ai.json` at `manifest_path`:
    /// the machine layer, if configured, plus the manifest's `allowedSources`
    /// (`allowed_sources`), if set.
    pub fn load(allowed_sources: Option<&[String]>, manifest_path: &Path) -> Result<Policy> {
        let mut policy = Policy::machine()?;
        if let Some(raw) = allowed_sources {
            policy.layers.push(Layer::new(
                format!(
                    "project policy `allowedSources` in {}",
                    manifest_path.display()
                ),
                raw,
            )?);
        }
        Ok(policy)
    }

    /// Only the machine layer (`SPM_POLICY`, else `$SPM_HOME/policy.json`). A
    /// missing default file means no machine policy; a missing file that
    /// `SPM_POLICY` explicitly names is an error, as is any unreadable or
    /// malformed policy — a broken policy must never fail open.
    pub fn machine() -> Result<Policy> {
        let loc = crate::paths::policy_file()?;
        let origin = if loc.explicit {
            format!("machine policy {} (from SPM_POLICY)", loc.path.display())
        } else {
            format!("machine policy {}", loc.path.display())
        };
        // Only a default location with *no directory entry at all* means "no
        // machine policy". `symlink_metadata` doesn't follow links, so a
        // dangling `policy.json` symlink (say, to a managed file that went
        // missing) still counts as configured and its read error surfaces.
        if !loc.explicit {
            match std::fs::symlink_metadata(&loc.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::default()),
                Err(e) => return Err(anyhow!(e).context(format!("reading the {origin}"))),
                Ok(_) => {}
            }
        }
        let text =
            std::fs::read_to_string(&loc.path).with_context(|| format!("reading the {origin}"))?;
        let file: PolicyFile =
            serde_json::from_str(&text).with_context(|| format!("parsing the {origin}"))?;
        Ok(Policy {
            layers: vec![Layer::new(origin, &file.allowed_sources)?],
        })
    }
}
