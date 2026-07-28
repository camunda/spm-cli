use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "ai.json";

/// The user-authored, VCS-committed declaration file (`ai.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Target vendor: "claude" or "copilot".
    pub target: String,
    #[serde(default)]
    pub skills: BTreeMap<String, SkillSpec>,
}

/// A single skill dependency. Exactly one of tag/branch/commit selects the version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSpec {
    pub git: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub commit: Option<String>,
    /// Optional subdirectory inside the repo (monorepo of skills).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
}

/// The version selector, normalized.
pub enum Version {
    Tag(String),
    Branch(String),
    Commit(String),
}

impl Version {
    /// Stable human label stored in ai.lock and used to detect request changes.
    pub fn label(&self) -> String {
        match self {
            Version::Tag(t) => format!("tag:{t}"),
            Version::Branch(b) => format!("branch:{b}"),
            Version::Commit(c) => format!("commit:{c}"),
        }
    }
}

impl SkillSpec {
    /// Validate that exactly one version selector is set and return it.
    pub fn version(&self) -> Result<Version> {
        match (&self.tag, &self.branch, &self.commit) {
            (Some(t), None, None) => Ok(Version::Tag(t.clone())),
            (None, Some(b), None) => Ok(Version::Branch(b.clone())),
            (None, None, Some(c)) => Ok(Version::Commit(c.clone())),
            (None, None, None) => bail!("skill `{}`: set one of tag/branch/commit", self.git),
            _ => bail!("skill `{}`: set only one of tag/branch/commit", self.git),
        }
    }
}

impl Manifest {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(MANIFEST_FILE)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let p = Self::path_in(dir);
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("no {MANIFEST_FILE} found at {}", p.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = Self::path_in(dir);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text + "\n").with_context(|| format!("writing {}", p.display()))
    }

    pub fn exists(dir: &Path) -> bool {
        Self::path_in(dir).exists()
    }
}
