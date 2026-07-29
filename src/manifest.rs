use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub const MANIFEST_FILE: &str = "ai.json";

/// The user-authored, VCS-committed declaration file (`ai.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Target vendors, e.g. `["claude", "copilot"]`.
    pub targets: Vec<String>,
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

/// Reject a skill name that could escape the vendor `skills/` directory when it
/// is used verbatim as a path component (e.g. `skills_dir.join(name)`). Names
/// come straight from `ai.json` keys, so a hostile manifest could otherwise
/// write outside the intended directory via `..`, `/`, or an absolute path.
pub fn validate_skill_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0');
    if bad {
        bail!(
            "invalid skill name `{name}`: names must be non-empty and must not contain \
             path separators, `.`, `..`, or NUL"
        );
    }
    Ok(())
}

/// Reject a skill `path` that could escape the fetched repo root. The subdir is
/// joined onto the store checkout (`repo_dir.join(path)`); without this a hostile
/// manifest could use an absolute path or `..` to read arbitrary files off the
/// host (e.g. `/etc`, or a sibling repo's checkout in the shared store).
pub fn validate_subpath(path: &str) -> Result<()> {
    let p = Path::new(path);
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                bail!("invalid path `{path}`: must be relative to the repo root")
            }
            Component::ParentDir => {
                bail!("invalid path `{path}`: `..` components are not allowed")
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    if path.contains('\0') {
        bail!("invalid path `{path}`: must not contain NUL");
    }
    Ok(())
}

impl Manifest {
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(MANIFEST_FILE)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let p = Self::path_in(dir);
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("no {MANIFEST_FILE} found at {}", p.display()))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        crate::schema::validate(&value).with_context(|| format!("in {}", p.display()))?;
        let manifest: Self =
            serde_json::from_value(value).with_context(|| format!("parsing {}", p.display()))?;
        for (name, spec) in &manifest.skills {
            validate_skill_name(name).with_context(|| format!("in {}", p.display()))?;
            if let Some(sub) = &spec.path {
                validate_subpath(sub)
                    .with_context(|| format!("skill `{name}` in {}", p.display()))?;
            }
        }
        Ok(manifest)
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
