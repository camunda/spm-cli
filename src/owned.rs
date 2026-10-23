//! What spm last materialized into each target of a scope, recorded locally
//! and independently of `ai.lock`.
//!
//! Shared-dir and global vendors write into directories that also hold the
//! user's own skills, so they may only remove entries spm put there. `ai.lock`
//! cannot be the record of that: it describes what *should* be installed and
//! can change without an install — `spm update --lock-only`, or a `git pull`
//! bringing a teammate's lock. A skill dropped by such a change would be
//! orphaned in the target; and a name the lock only passed through (or one a
//! newly added target never received) must not be "cleaned up" from a dir
//! where it is the user's own. So install, clean and `target remove` purge
//! exactly what this record says spm installed into each target, and only an
//! install (or clean) rewrites it.
//!
//! The record is machine-local state, never committed: in a project it lives
//! in `.spm/owned.json` (the `.spm/` dir ignores itself), globally in
//! `$SPM_HOME/owned-global.json`. Installs made before it existed have no
//! record; then the lock stands in for it, as it always did.

use crate::lockfile::Lockfile;
use crate::manifest::validate_skill_name;
use crate::scope::Scope;
use crate::vendor::Claim;
use crate::{gitignore, paths};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const PROJECT_DIR: &str = ".spm";
const PROJECT_FILE: &str = "owned.json";
const GLOBAL_FILE: &str = "owned-global.json";

/// Skill and plugin names spm installed into one target.
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

    /// Add every name in `other`.
    pub fn extend(&mut self, other: &Owned) {
        self.skills.extend(other.skills.iter().cloned());
        self.plugins.extend(other.plugins.iter().cloned());
    }

    pub fn skill_names(&self) -> Vec<String> {
        self.skills.iter().cloned().collect()
    }

    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.iter().cloned().collect()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    targets: BTreeMap<String, Owned>,
}

/// What spm owns in each target of a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// Per target, exactly what spm last installed there. A target without an
    /// entry holds nothing of spm's.
    Recorded(BTreeMap<String, Owned>),
    /// No record yet (installed by an spm that kept none): assume every target
    /// holds what this lock installs.
    Legacy(Owned),
}

impl Ownership {
    fn path(scope: &Scope) -> Result<PathBuf> {
        Ok(match scope {
            Scope::Project { root } => root.join(PROJECT_DIR).join(PROJECT_FILE),
            Scope::Global => paths::spm_home()?.join(GLOBAL_FILE),
        })
    }

    /// The recorded ownership, or `lock`'s set when nothing was recorded yet.
    pub fn load(scope: &Scope, lock: &Lockfile) -> Result<Self> {
        let p = Self::path(scope)?;
        // Only a record that is truly absent falls back to the lock: one that
        // can't be inspected must stop the caller before it deletes anything.
        if !p
            .try_exists()
            .with_context(|| format!("inspecting {}", p.display()))?
        {
            return Ok(Self::Legacy(Owned::from_lock(lock)));
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let record: Record =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        // The names are joined onto target dirs that get deleted, so a
        // tampered record must not be able to point outside them.
        for owned in record.targets.values() {
            for name in owned.skills.iter().chain(&owned.plugins) {
                validate_skill_name(name).with_context(|| format!("in {}", p.display()))?;
            }
        }
        Ok(Self::Recorded(record.targets))
    }

    /// What spm owns in `target`.
    pub fn for_target(&self, target: &str) -> Owned {
        match self {
            Self::Recorded(map) => map.get(target).cloned().unwrap_or_default(),
            Self::Legacy(owned) => owned.clone(),
        }
    }

    /// Install `lock`'s set into each of `targets` with `install(index, prior
    /// ownership, claim)`, keeping the record true at every step: a failure
    /// part-way leaves nothing spm touched unrecorded, and claims nothing it
    /// did not touch. A skill name is recorded when the vendor `claim`s it,
    /// right before modifying its entry; plugin output lives only in
    /// spm-owned locations, so the new plugins are recorded up front. Once a
    /// target is written it holds exactly the new set.
    pub fn install(
        &self,
        scope: &Scope,
        targets: &[String],
        lock: &Lockfile,
        mut install: impl FnMut(usize, &Owned, Claim) -> Result<()>,
    ) -> Result<()> {
        let desired = Owned::from_lock(lock);
        let mut record: BTreeMap<String, Owned> = targets
            .iter()
            .map(|t| (t.clone(), self.for_target(t)))
            .collect();
        for (i, target) in targets.iter().enumerate() {
            let prior = self.for_target(target);
            let mut ahead = prior.clone();
            ahead.plugins.extend(desired.plugins.iter().cloned());
            record.insert(target.clone(), ahead);
            Self::Recorded(record.clone()).save(scope)?;
            let mut claim = |name: &str| -> Result<()> {
                let owned = record.get_mut(target).expect("target recorded above");
                if owned.skills.insert(name.to_string()) {
                    Self::Recorded(record.clone()).save(scope)?;
                }
                Ok(())
            };
            install(i, &prior, &mut claim)?;
            record.insert(target.clone(), desired.clone());
        }
        Self::Recorded(record).save(scope)
    }

    /// Before the lock changes without an install, pin down what the targets
    /// hold: a legacy assumption ("whatever the lock installs") is written out
    /// for `targets`, while an existing record is already exact and is kept.
    pub fn freeze(&self, scope: &Scope, targets: &[String]) -> Result<()> {
        match self {
            Self::Recorded(_) => Ok(()),
            Self::Legacy(owned) => {
                Self::Recorded(targets.iter().map(|t| (t.clone(), owned.clone())).collect())
                    .save(scope)
            }
        }
    }

    /// Forget `target` (its spm output was removed).
    pub fn without(self, target: &str) -> Self {
        match self {
            Self::Recorded(mut map) => {
                map.remove(target);
                Self::Recorded(map)
            }
            legacy => legacy,
        }
    }

    /// Add `owned` to what spm owns in `target` (e.g. output another target
    /// wrote into a location `target` shares). `Legacy` already assumes every
    /// target holds the lock's whole set.
    pub fn grant(&mut self, target: &str, owned: &Owned) {
        if let Self::Recorded(map) = self {
            map.entry(target.to_string()).or_default().extend(owned);
        }
    }

    /// Persist a record. `Legacy` is not persisted: there is nothing exact to
    /// write down.
    pub fn save(&self, scope: &Scope) -> Result<()> {
        let Self::Recorded(targets) = self else {
            return Ok(());
        };
        let p = Self::path(scope)?;
        let dir = p.parent().expect("record path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        if let Scope::Project { .. } = scope {
            // `.spm/.gitignore` may pre-exist (even empty): append the rule
            // rather than trust it, so the record can never be committed.
            gitignore::ensure(dir, "# machine-local spm state — never commit", "*")?;
        }
        let record = Record {
            targets: targets.clone(),
        };
        let text = serde_json::to_string_pretty(&record)?;
        std::fs::write(&p, text + "\n").with_context(|| format!("writing {}", p.display()))
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

    fn lock_with(skills: &[&str]) -> Lockfile {
        let mut lock = Lockfile::default();
        for name in skills {
            let commit = "a".repeat(40);
            lock.skills.insert(
                name.to_string(),
                serde_json::from_value(serde_json::json!({
                    "git": "https://example.com/r.git",
                    "reference": "branch:main",
                    "commit": commit,
                    "store": crate::lockfile::store_key("https://example.com/r.git", &commit),
                }))
                .unwrap(),
            );
        }
        lock
    }

    fn installed_err(
        scope: &Scope,
        owned: &Ownership,
        targets: &[String],
        lock: &Lockfile,
        fail_at: usize,
    ) -> String {
        let err = owned
            .install(scope, targets, lock, |i, _, claim| {
                claim("touched")?;
                if i == fail_at {
                    anyhow::bail!("boom")
                }
                Ok(())
            })
            .unwrap_err();
        format!("{err:#}")
    }

    #[test]
    fn grant_extends_a_recorded_target_and_leaves_legacy_alone() {
        let extra = Owned {
            skills: set(&["fresh"]),
            plugins: set(&["kit"]),
        };
        let mut recorded = Ownership::Recorded(BTreeMap::new());
        recorded.grant("amp", &extra);
        recorded.grant(
            "amp",
            &Owned {
                skills: set(&["base"]),
                plugins: BTreeSet::new(),
            },
        );
        let amp = recorded.for_target("amp");
        assert_eq!(amp.skills, set(&["base", "fresh"]));
        assert_eq!(amp.plugins, set(&["kit"]));
        assert_eq!(recorded.for_target("codex"), Owned::default());

        let mut legacy = Ownership::Legacy(Owned::from_lock(&lock_with(&["a"])));
        let before = legacy.clone();
        legacy.grant("amp", &extra);
        assert_eq!(legacy, before);
    }

    #[test]
    fn records_per_target_and_ignores_itself_in_a_project() {
        let root = scratch("roundtrip");
        let scope = Scope::Project { root: root.clone() };
        let legacy = Ownership::load(&scope, &lock_with(&["a"])).unwrap();
        assert_eq!(legacy.for_target("codex").skills, set(&["a"]));
        // A legacy assumption is written out once; afterwards it is exact and
        // no later lock (lock-only writes) changes it.
        legacy.freeze(&scope, &["codex".to_string()]).unwrap();
        let recorded = Ownership::load(&scope, &lock_with(&["b"])).unwrap();
        assert_eq!(recorded.for_target("codex").skills, set(&["a"]));
        assert_eq!(recorded.for_target("cursor"), Owned::default());
        recorded.freeze(&scope, &["cursor".to_string()]).unwrap();
        assert_eq!(Ownership::load(&scope, &lock_with(&[])).unwrap(), recorded);

        let targets = ["codex".to_string(), "cursor".to_string()];
        let err = installed_err(&scope, &recorded, &targets, &lock_with(&["b"]), 1);
        assert!(err.contains("boom"), "{err}");
        // `codex` was written; `cursor` failed part-way, so it holds its prior
        // set plus exactly what it claimed — not the rest of the new set.
        let partial = Ownership::load(&scope, &lock_with(&[])).unwrap();
        assert_eq!(partial.for_target("codex").skills, set(&["b"]));
        assert_eq!(partial.for_target("cursor").skills, set(&["touched"]));
        let mut seen = Vec::new();
        recorded
            .install(&scope, &targets[..1], &lock_with(&["b"]), |i, prior, _| {
                seen.push((i, prior.skill_names()));
                Ok(())
            })
            .unwrap();
        assert_eq!(seen, [(0, vec!["a".to_string()])]);
        let installed = Ownership::load(&scope, &lock_with(&[])).unwrap();
        assert_eq!(installed.for_target("codex").skills, set(&["b"]));
        assert_eq!(installed.for_target("cursor"), Owned::default());
        // An explicitly empty record stays exact rather than falling back to the lock.
        installed.without("codex").save(&scope).unwrap();
        let empty = Ownership::load(&scope, &lock_with(&["b"])).unwrap();
        assert_eq!(empty, Ownership::Recorded(BTreeMap::new()));
        let ignore = std::fs::read_to_string(root.join(".spm").join(".gitignore")).unwrap();
        assert!(ignore.lines().any(|l| l == "*"), "{ignore}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_existing_ignore_file_without_the_rule_gets_it() {
        for existing in ["", "keep-me\n"] {
            let root = scratch("ignore");
            let scope = Scope::Project { root: root.clone() };
            std::fs::create_dir_all(root.join(".spm")).unwrap();
            std::fs::write(root.join(".spm").join(".gitignore"), existing).unwrap();
            Ownership::Recorded(BTreeMap::new()).save(&scope).unwrap();
            let ignore = std::fs::read_to_string(root.join(".spm").join(".gitignore")).unwrap();
            assert!(ignore.starts_with(existing), "{ignore:?}");
            assert!(ignore.lines().any(|l| l == "*"), "{ignore:?}");
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn rejects_names_that_escape_the_target_dir() {
        let root = scratch("hostile");
        let scope = Scope::Project { root: root.clone() };
        std::fs::create_dir_all(root.join(".spm")).unwrap();
        for owned in [
            r#"{"skills":[".."]}"#,
            r#"{"skills":["../x"]}"#,
            r#"{"skills":["a\\b"]}"#,
            r#"{"skills":[""]}"#,
            r#"{"plugins":["../kit"]}"#,
        ] {
            std::fs::write(
                root.join(".spm").join("owned.json"),
                format!(r#"{{"targets":{{"codex":{owned}}}}}"#),
            )
            .unwrap();
            let err = format!(
                "{:#}",
                Ownership::load(&scope, &Lockfile::default()).unwrap_err()
            );
            assert!(err.contains("invalid skill name"), "{owned}: {err}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
