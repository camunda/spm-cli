use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const LOCK_FILE: &str = "ai.lock";

/// Generated, VCS-committed pin file (`ai.lock`). Makes installs reproducible:
/// every version selector is resolved down to an immutable commit SHA + store key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lockfile {
    #[serde(default)]
    pub skills: BTreeMap<String, LockedSkill>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSkill {
    pub git: String,
    /// Human-readable ref that was requested (e.g. "tag:v1.2.0", "branch:main").
    pub reference: String,
    /// Resolved immutable commit.
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    /// Store directory key: `<sanitized-url>@<sha>`.
    pub store: String,
}

impl Lockfile {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(LOCK_FILE)
    }

    pub fn load_or_default(dir: &Path) -> Result<Self> {
        let p = Self::path_in(dir);
        if !p.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&p)?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = Self::path_in(dir);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text + "\n").with_context(|| format!("writing {}", p.display()))
    }
}

/// Derive a filesystem-safe, stable store key from a repo URL and commit SHA.
pub fn store_key(url: &str, sha: &str) -> String {
    let sanitized: String = url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    // Trim runs of underscores for readability, keep full sha for collision safety.
    let sanitized = sanitized.trim_matches('_').to_string();
    format!("{sanitized}@{sha}")
}
