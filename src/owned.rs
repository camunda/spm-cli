//! What spm last materialized into a scope's targets, recorded locally and
//! independently of `ai.lock`.
//!
//! Shared-dir and global vendors write into directories that also hold the
//! user's own skills, so they may only remove entries spm owns. `ai.lock`
//! cannot be the sole record of that: it describes what *should* be installed
//! and can change without an install — `spm update --lock-only`, or a `git
//! pull` bringing a teammate's lock. A skill dropped by such a change would
//! otherwise be orphaned in the target, as neither the next `install` nor
//! `clean` would know spm put it there. Install and clean therefore treat
//! `ai.lock` ∪ this record as owned.
//!
//! The record is machine-local state, never committed: in a project it lives
//! in `.spm/owned.json` (the `.spm/` dir ignores itself), globally in
//! `$SPM_HOME/owned-global.json`.

use crate::lockfile::Lockfile;
use crate::manifest::validate_skill_name;
use crate::paths;
use crate::scope::Scope;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

const PROJECT_DIR: &str = ".spm";
const PROJECT_FILE: &str = "owned.json";
const GLOBAL_FILE: &str = "owned-global.json";

/// Skill and plugin names spm materialized and has not cleaned up since.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owned {
    #[serde(default)]
    pub skills: BTreeSet<String>,
    #[serde(default)]
    pub plugins: BTreeSet<String>,
}

impl Owned {
    /// Every name `lock` installs: its skills, plugins' bundled skills (which
    /// vendors flatten into the same skills dirs) and its plugins.
    pub fn from_lock(lock: &Lockfile) -> Self {
        Self {
            skills: lock
                .skills
                .keys()
                .cloned()
                .chain(
                    lock.plugins
                        .values()
                        .flat_map(|l| l.bundled_skills.iter().cloned()),
                )
                .collect(),
            plugins: lock.plugins.keys().cloned().collect(),
        }
    }

    pub fn extend(&mut self, other: Owned) {
        self.skills.extend(other.skills);
        self.plugins.extend(other.plugins);
    }

    fn path(scope: &Scope) -> Result<PathBuf> {
        Ok(match scope {
            Scope::Project { root } => root.join(PROJECT_DIR).join(PROJECT_FILE),
            Scope::Global => paths::spm_home()?.join(GLOBAL_FILE),
        })
    }

    /// The recorded set, or empty when nothing was recorded yet.
    pub fn load(scope: &Scope) -> Result<Self> {
        let p = Self::path(scope)?;
        if !p.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let owned: Self =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        // The names are joined onto target dirs that get deleted, so a
        // tampered record must not be able to point outside them.
        for name in owned.skills.iter().chain(&owned.plugins) {
            validate_skill_name(name).with_context(|| format!("in {}", p.display()))?;
        }
        Ok(owned)
    }

    /// Persist the set; an empty set removes the record instead.
    pub fn save(&self, scope: &Scope) -> Result<()> {
        if self.skills.is_empty() && self.plugins.is_empty() {
            return Self::remove(scope);
        }
        let p = Self::path(scope)?;
        let dir = p.parent().expect("record path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        if let Scope::Project { .. } = scope {
            let ignore = dir.join(".gitignore");
            if !ignore.exists() {
                std::fs::write(&ignore, "# machine-local spm state — never commit\n*\n")
                    .with_context(|| format!("writing {}", ignore.display()))?;
            }
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, text + "\n").with_context(|| format!("writing {}", p.display()))
    }

    /// Forget the record (after a `clean`, nothing is owned any more).
    pub fn remove(scope: &Scope) -> Result<()> {
        let p = Self::path(scope)?;
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", p.display()))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("spm-owned-{name}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn round_trips_and_ignores_itself_in_a_project() {
        let root = scratch("roundtrip");
        let scope = Scope::Project { root: root.clone() };
        assert_eq!(Owned::load(&scope).unwrap(), Owned::default());
        let owned = Owned {
            skills: set(&["a", "b"]),
            plugins: set(&["kit"]),
        };
        owned.save(&scope).unwrap();
        assert_eq!(Owned::load(&scope).unwrap(), owned);
        let ignore = std::fs::read_to_string(root.join(".spm").join(".gitignore")).unwrap();
        assert!(ignore.lines().any(|l| l == "*"), "{ignore}");
        Owned::default().save(&scope).unwrap();
        assert!(!root.join(".spm").join("owned.json").exists());
        Owned::remove(&scope).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_names_that_escape_the_target_dir() {
        let root = scratch("hostile");
        let scope = Scope::Project { root: root.clone() };
        std::fs::create_dir_all(root.join(".spm")).unwrap();
        for bad in ["..", "../x", "a\\\\b", ""] {
            std::fs::write(
                root.join(".spm").join("owned.json"),
                format!(r#"{{"skills":["{bad}"]}}"#),
            )
            .unwrap();
            let err = format!("{:#}", Owned::load(&scope).unwrap_err());
            assert!(err.contains("invalid skill name"), "{bad}: {err}");
        }
        std::fs::write(
            root.join(".spm").join("owned.json"),
            r#"{"plugins":["../kit"]}"#,
        )
        .unwrap();
        assert!(Owned::load(&scope).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
