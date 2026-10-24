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
//! The record is machine-local state, never in the checkout — a repository
//! must not be able to claim the user's own skills for spm to delete: in a
//! project it lives in `$SPM_HOME/owned/<project>-<hash>.json` (named like the
//! links file, see [`crate::links`]), globally in `$SPM_HOME/owned-global.json`. Installs made before it existed have no
//! record; then the lock stands in for it, as it always did.

use crate::lockfile::Lockfile;
use crate::manifest::validate_skill_name;
use crate::pathid::{dir_id, DirId};
use crate::paths;
use crate::scope::Scope;
use crate::vendor::Claim;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `$SPM_HOME` subdirectory holding one record per project.
const PROJECT_DIR: &str = "owned";
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
    /// Per target holding skills, the identity of the skills dir they were
    /// written into (see [`Ownership::load`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    dirs: BTreeMap<String, DirId>,
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
    /// Every ownership record on this machine: the global one and each
    /// project's (whether or not they exist yet), `scope`'s first. Losing any
    /// of them makes cleanup trust a newer lock, so all are kept from harm.
    pub fn all_paths(scope: &Scope) -> Result<Vec<PathBuf>> {
        let own = Self::path(scope)?;
        let home = paths::spm_home()?;
        let mut out = vec![own.clone()];
        let global = home.join(GLOBAL_FILE);
        if global != own {
            out.push(global);
        }
        let dir = home.join(PROJECT_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        };
        for entry in entries {
            let file = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            if file.extension().is_some_and(|e| e == "json") && file != own {
                out.push(file);
            }
        }
        Ok(out)
    }

    pub fn path(scope: &Scope) -> Result<PathBuf> {
        let home = paths::spm_home()?;
        Ok(match scope {
            Scope::Project { root } => crate::links::project_state_path(&home, PROJECT_DIR, root)?,
            Scope::Global => home.join(GLOBAL_FILE),
        })
    }

    /// The recorded ownership, or — when nothing was recorded yet — `lock`'s
    /// set plus `linked`, the skills last materialized from linked plugins
    /// (see [`crate::links::Links::managed`]), which every target got. A
    /// record is reconciled for the configured `targets` (see
    /// [`Self::adopt_shared`]).
    pub fn load(
        scope: &Scope,
        lock: &Lockfile,
        linked: &[String],
        targets: &[String],
    ) -> Result<Self> {
        let p = Self::path(scope)?;
        // Only a record that is truly absent falls back to the lock: one that
        // can't be inspected must stop the caller before it deletes anything.
        if !p
            .try_exists()
            .with_context(|| format!("inspecting {}", p.display()))?
        {
            let mut owned = Owned::from_lock(lock);
            owned.skills.extend(linked.iter().cloned());
            return Ok(Self::Legacy(owned));
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
        // Skill names are bound to the dir spm wrote them into: once a
        // target's skills dir is gone or is a different directory (moved
        // aside, or a symlink re-pointed at the user's own skills), the names
        // recorded for the old one claim nothing there. Only a present,
        // matching identity keeps them: an unknown one (no birth time, or no
        // dir) proves nothing.
        let mut map = record.targets;
        for (target, owned) in map.iter_mut() {
            if owned.skills.is_empty() {
                continue;
            }
            let current = skills_dir_identity(scope, target)?;
            if current.is_none() || record.dirs.get(target) != current.as_ref() {
                owned.skills.clear();
            }
        }
        let mut ownership = Self::Recorded(map);
        ownership.adopt_shared(scope, targets)?;
        Ok(ownership)
    }

    /// Several vendors can write into one physical skills dir (codex and amp
    /// both use `.agents/skills/`), so what spm installed there is owned by
    /// the dir, not by whichever target wrote it. A configured target takes
    /// over the skill names recorded for unconfigured targets whose skills dir
    /// is that very same dir (compared by filesystem identity) — e.g. after a
    /// branch switched from codex to amp — so it removes them once unwanted.
    /// The names move rather than copy: no stale claim stays behind for a dir
    /// entry spm may no longer have written. Targets with separate dirs stay
    /// isolated; a legacy assumption already covers every target.
    fn adopt_shared(&mut self, scope: &Scope, targets: &[String]) -> Result<()> {
        let Self::Recorded(map) = self else {
            return Ok(());
        };
        let dir_of = |target: &str| skills_dir_identity(scope, target);
        let mut unconfigured = Vec::new();
        for t in map.keys().filter(|t| !targets.contains(t)) {
            if let Some(id) = dir_of(t)? {
                unconfigured.push((t.clone(), id));
            }
        }
        for target in targets {
            let Some(ours) = dir_of(target)? else {
                continue;
            };
            for (other, _) in unconfigured.iter().filter(|(_, id)| *id == ours) {
                let skills = map
                    .get_mut(other)
                    .map(|o| std::mem::take(&mut o.skills))
                    .unwrap_or_default();
                map.entry(target.clone()).or_default().skills.extend(skills);
            }
        }
        Ok(())
    }

    /// Drop every recorded skill name whose entry is gone from its target's
    /// skills dir: spm holds nothing there any more, so whatever is created
    /// under that name later is the user's. Called once a command that
    /// deletes entries (install, `clean`, `target remove`) stops part-way, so
    /// the record never outlives what it removed. Plugins live only in
    /// spm-owned locations, so their names stay.
    pub fn settle(&mut self, scope: &Scope) -> Result<()> {
        let Self::Recorded(map) = self else {
            return Ok(());
        };
        for (target, owned) in map.iter_mut() {
            let Ok(vendor) = crate::vendor::for_target(target) else {
                continue;
            };
            let dir = vendor.skills_dir(scope)?;
            owned
                .skills
                .retain(|name| match dir.join(name).symlink_metadata() {
                    Ok(_) => true,
                    Err(e) => !matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ),
                });
        }
        Ok(())
    }

    /// Return `err`, first [settling](Self::settle) and saving the record as
    /// it stands once a command failed part-way. A legacy assumption is
    /// written out for `targets` first (see [`Self::freeze`]): falling back to
    /// the lock again would claim what was just removed.
    pub fn fail(self, scope: &Scope, targets: &[String], err: anyhow::Error) -> anyhow::Error {
        let mut record = self.exact(targets);
        match record.settle(scope).and_then(|()| record.save(scope)) {
            Ok(()) => err,
            Err(e) => err.context(format!("also failed to update the ownership record: {e:#}")),
        }
    }

    /// What spm owns in `target`.
    pub fn for_target(&self, target: &str) -> Owned {
        match self {
            Self::Recorded(map) => map.get(target).cloned().unwrap_or_default(),
            Self::Legacy(owned) => owned.clone(),
        }
    }

    /// Install `desired` — the names actually materialized, which a linked
    /// plugin's working copy may make differ from `ai.lock` — into each of
    /// `targets` with `install(index, prior ownership, claim)`, keeping the
    /// record true at every step: a failure part-way leaves nothing spm
    /// touched unrecorded, and claims nothing it did not touch. A skill name is
    /// recorded when the vendor `claim`s it, right before modifying its entry;
    /// plugin output lives only in spm-owned locations, so the new plugins are
    /// recorded up front. Once a target is written it holds exactly the new
    /// set. Targets not installed now (e.g. ones another branch's `ai.json`
    /// declares) keep their record, minus the names an install removed from a
    /// skills dir they share.
    pub fn install(
        &self,
        scope: &Scope,
        targets: &[String],
        desired: &Owned,
        mut install: impl FnMut(usize, &Owned, Claim) -> Result<()>,
    ) -> Result<()> {
        let mut record = self.recorded();
        for t in targets {
            record.insert(t.clone(), self.for_target(t));
        }
        for (i, target) in targets.iter().enumerate() {
            // As recorded now: a target sharing a dir already written lost
            // the names removed from it.
            let prior = record.get(target).cloned().unwrap_or_default();
            // Targets sharing this skills dir (see `adopt_shared`) hold the
            // same physical entries: a name this install removes from it is
            // gone for them too, so they stop claiming it now — while this
            // target keeps it until written, so it is never left unowned.
            if let Some(ours) = skills_dir_identity(scope, target)? {
                for (other, owned) in record.iter_mut() {
                    if other != target
                        && owned.skills.iter().any(|n| !desired.skills.contains(n))
                        && skills_dir_identity(scope, other)?.as_ref() == Some(&ours)
                    {
                        owned
                            .skills
                            .retain(|n| !prior.skills.contains(n) || desired.skills.contains(n));
                    }
                }
            }
            // Names this install removes are released before it starts: the
            // record must never outlive a removal, even when a later save
            // fails. Should it stop part-way, those still present are
            // reclaimed (see `fail`); at worst one stays unclaimed, which is
            // left alone rather than deleted.
            let mut ahead = prior.clone();
            ahead.skills.retain(|n| desired.skills.contains(n));
            ahead.plugins.extend(desired.plugins.iter().cloned());
            record.insert(target.clone(), ahead);
            Self::Recorded(record.clone()).save(scope)?;
            // The dir the record binds this target's skills to (see `save`).
            // A vendor may create it, or recreate it (Claude rebuilds its
            // marketplace), before claiming even names it already owned, so a
            // claim saves whenever the name or the binding is new.
            let mut bound = skills_dir_identity(scope, target)?;
            let mut claim = |name: &str| -> Result<()> {
                let owned = record.get_mut(target).expect("target recorded above");
                // Claimed only to be removed: nothing to record.
                let added = desired.skills.contains(name) && owned.skills.insert(name.to_string());
                let dir = skills_dir_identity(scope, target)?;
                if added || dir != bound {
                    Self::Recorded(record.clone()).save(scope)?;
                    bound = dir;
                }
                Ok(())
            };
            if let Err(e) = install(i, &prior, &mut claim) {
                let owned = record.get_mut(target).expect("target recorded above");
                owned.skills.extend(prior.skills.iter().cloned());
                return Err(Self::Recorded(record).fail(scope, targets, e));
            }
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
            Self::Legacy(_) => self.clone().exact(targets).save(scope),
        }
    }

    /// A legacy assumption written out as a record for `targets` (each holds
    /// the lock's set); a record as is.
    pub fn exact(self, targets: &[String]) -> Self {
        match self {
            Self::Legacy(owned) => {
                Self::Recorded(targets.iter().map(|t| (t.clone(), owned.clone())).collect())
            }
            recorded => recorded,
        }
    }

    /// The exact per-target record; a legacy assumption covers no particular
    /// target, so it records none.
    fn recorded(&self) -> BTreeMap<String, Owned> {
        match self {
            Self::Recorded(map) => map.clone(),
            Self::Legacy(_) => BTreeMap::new(),
        }
    }

    /// The record once `targets` were cleaned: they hold nothing of spm's —
    /// recorded explicitly, so a later install does not fall back to the
    /// lock — while every other target keeps its record.
    pub fn cleaned(&self, targets: &[String]) -> Self {
        let mut map = self.recorded();
        for t in targets {
            map.remove(t);
        }
        Self::Recorded(map)
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

    /// Persist a record, binding each target's skills to its current skills
    /// dir (every name was claimed in, or checked against, that very dir; the
    /// vendors create it before claiming). `Legacy` is not persisted: there
    /// is nothing exact to write down.
    pub fn save(&self, scope: &Scope) -> Result<()> {
        let Self::Recorded(targets) = self else {
            return Ok(());
        };
        let p = Self::path(scope)?;
        let dir = p.parent().expect("record path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut dirs = BTreeMap::new();
        for (target, owned) in targets {
            if owned.skills.is_empty() {
                continue;
            }
            if let Some(id) = skills_dir_identity(scope, target)? {
                dirs.insert(target.clone(), id);
            }
        }
        let record = Record {
            targets: targets.clone(),
            dirs,
        };
        let text = serde_json::to_string_pretty(&record)?;
        replace_atomically(&p, (text + "\n").as_bytes(), |f, bytes| f.write_all(bytes))
            .with_context(|| format!("writing {}", p.display()))
    }
}

/// The [`DirId`] of the directory `target`'s skills dir in `scope` resolves
/// to, if it exists: the directory itself, not the route to it — moved, or
/// reached through new symlinks, it still holds what spm wrote; a different
/// directory at the same path (even one reusing its inode) does not. A
/// target spm no longer knows, or a filesystem without birth times, has none,
/// so nothing recorded for it is trusted: a skill may be left installed, but
/// never is the user's own deleted.
fn skills_dir_identity(scope: &Scope, target: &str) -> Result<Option<DirId>> {
    let Ok(vendor) = crate::vendor::for_target(target) else {
        return Ok(None);
    };
    dir_id(&vendor.skills_dir(scope)?)
}

/// Replace `path` with `bytes` all at once: they are `write`n to a fresh
/// temporary file beside it, which is renamed over `path` only once complete.
/// The record is the sole history of what spm installed, so a failed write
/// (e.g. a full disk) must leave the previous one intact.
fn replace_atomically(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let dir = path.parent().expect("record path has a parent");
    let name = path.file_name().expect("record path has a file name");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(
        ".{}.{}-{nanos}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    let res = write(&mut file, bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&tmp, path)
        });
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
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

    /// A scratch dir holding an isolated `SPM_HOME` (`home/`) and a project
    /// (`project/`), with `SPM_HOME` pointed at it while the guard lives.
    fn sandbox(name: &str) -> (PathBuf, Scope, paths::SpmHomeGuard) {
        let root = scratch(name);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let guard = paths::SpmHomeGuard::set(&root.join("home"));
        (root, Scope::Project { root: project }, guard)
    }

    /// Create `target`'s skills dir, so a record can bind names to it.
    fn make_skills_dir(scope: &Scope, target: &str) {
        let vendor = crate::vendor::for_target(target).unwrap();
        std::fs::create_dir_all(vendor.skills_dir(scope).unwrap()).unwrap();
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

    /// Install `lock`'s names into `targets`, writing each claimed entry like
    /// a vendor would (but removing none): the one at `fail_at` writes only
    /// `touched`, then fails.
    fn installed_err(
        scope: &Scope,
        owned: &Ownership,
        targets: &[String],
        lock: &Lockfile,
        fail_at: usize,
    ) -> String {
        let write = |target: &str, name: &str| {
            let dir = crate::vendor::for_target(target)
                .unwrap()
                .skills_dir(scope)
                .unwrap();
            std::fs::create_dir_all(dir.join(name)).unwrap();
        };
        let err = owned
            .install(scope, targets, &Owned::from_lock(lock), |i, _, claim| {
                // The target at `fail_at` stops after its first name.
                for (n, name) in lock.skills.keys().enumerate() {
                    if i == fail_at && n == 1 {
                        anyhow::bail!("boom")
                    }
                    claim(name)?;
                    write(&targets[i], name);
                }
                Ok(())
            })
            .unwrap_err();
        format!("{err:#}")
    }

    #[test]
    fn a_failed_write_keeps_the_previous_record() {
        let (root, scope, _home) = sandbox("atomic");
        make_skills_dir(&scope, "codex");
        let mut first = Ownership::Recorded(BTreeMap::new());
        first.grant("codex", &Owned::from_lock(&lock_with(&["a"])));
        first.save(&scope).unwrap();
        let path = Ownership::path(&scope).unwrap();
        let before = std::fs::read(&path).unwrap();

        let err = replace_atomically(&path, b"{\"targets\":{}}\n", |f, bytes| {
            f.write_all(&bytes[..3])?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        assert_eq!(std::fs::read(&path).unwrap(), before, "record unchanged");
        assert_eq!(
            Ownership::load(&scope, &Lockfile::default(), &[], &[]).unwrap(),
            first
        );
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file removed: {leftovers:?}");

        let mut second = first.clone();
        second.grant("codex", &Owned::from_lock(&lock_with(&["b"])));
        second.save(&scope).unwrap();
        assert_eq!(
            Ownership::load(&scope, &Lockfile::default(), &[], &[]).unwrap(),
            second
        );
        std::fs::remove_dir_all(&root).unwrap();
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
    fn records_per_target_outside_the_checkout() {
        let (root, scope, _home) = sandbox("roundtrip");
        make_skills_dir(&scope, "codex");
        make_skills_dir(&scope, "cursor");
        // With no record, every target is assumed to hold the lock's names
        // plus what linked plugins last materialized.
        let linked = Ownership::load(&scope, &lock_with(&["a"]), &["x".to_string()], &[]).unwrap();
        assert_eq!(linked.for_target("codex").skills, set(&["a", "x"]));
        let legacy = Ownership::load(&scope, &lock_with(&["a"]), &[], &[]).unwrap();
        assert_eq!(legacy.for_target("codex").skills, set(&["a"]));
        // A legacy assumption is written out once; afterwards it is exact and
        // no later lock (lock-only writes) changes it.
        legacy.freeze(&scope, &["codex".to_string()]).unwrap();
        let recorded = Ownership::load(&scope, &lock_with(&["b"]), &[], &[]).unwrap();
        assert_eq!(recorded.for_target("codex").skills, set(&["a"]));
        assert_eq!(recorded.for_target("cursor"), Owned::default());
        recorded.freeze(&scope, &["cursor".to_string()]).unwrap();
        assert_eq!(
            Ownership::load(&scope, &lock_with(&[]), &[], &[]).unwrap(),
            recorded
        );

        let targets = ["codex".to_string(), "cursor".to_string()];
        let err = installed_err(&scope, &recorded, &targets, &lock_with(&["b", "c"]), 1);
        assert!(err.contains("boom"), "{err}");
        // `codex` was written; `cursor` failed part-way, so it holds its prior
        // set plus exactly what it claimed — not the rest of the new set.
        let partial = Ownership::load(&scope, &lock_with(&[]), &[], &[]).unwrap();
        assert_eq!(partial.for_target("codex").skills, set(&["b", "c"]));
        assert_eq!(partial.for_target("cursor").skills, set(&["b"]));
        let mut seen = Vec::new();
        recorded
            .install(
                &scope,
                &targets[..1],
                &Owned::from_lock(&lock_with(&["b"])),
                |i, prior, _| {
                    seen.push((i, prior.skill_names()));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(seen, [(0, vec!["a".to_string()])]);
        let installed = Ownership::load(&scope, &lock_with(&[]), &[], &[]).unwrap();
        assert_eq!(installed.for_target("codex").skills, set(&["b"]));
        assert_eq!(installed.for_target("cursor"), Owned::default());
        // A target not installed now (absent from this branch's ai.json) keeps
        // its record, and `cleaned` forgets only the targets it is given.
        partial
            .install(&scope, &targets[..1], &Owned::default(), |_, _, _| Ok(()))
            .unwrap();
        let kept = Ownership::load(&scope, &lock_with(&[]), &[], &[]).unwrap();
        assert_eq!(kept.for_target("codex"), Owned::default());
        assert_eq!(kept.for_target("cursor").skills, set(&["b"]));
        let cleaned = kept.cleaned(&targets[..1]);
        assert_eq!(cleaned.for_target("cursor").skills, set(&["b"]));
        assert_eq!(
            Ownership::Legacy(Owned::from_lock(&lock_with(&["a"]))).cleaned(&[]),
            Ownership::Recorded(BTreeMap::new())
        );
        // An explicitly empty record stays exact rather than falling back to the lock.
        installed.without("codex").save(&scope).unwrap();
        let empty = Ownership::load(&scope, &lock_with(&["b"]), &[], &[]).unwrap();
        assert_eq!(empty, Ownership::Recorded(BTreeMap::new()));
        let path = Ownership::path(&scope).unwrap();
        assert!(
            path.starts_with(root.join("home").join(PROJECT_DIR)),
            "{path:?}"
        );
        assert!(!root.join("project").join(".spm").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_record_in_the_checkout_is_ignored() {
        // A repository must not be able to claim the user's own skills for
        // spm to delete by committing a record.
        let (root, scope, _home) = sandbox("committed");
        let Scope::Project { root: project } = &scope else {
            unreachable!()
        };
        std::fs::create_dir_all(project.join(".spm")).unwrap();
        std::fs::write(
            project.join(".spm").join("owned.json"),
            r#"{"targets":{"codex":{"skills":["users-own"]}}}"#,
        )
        .unwrap();
        let loaded = Ownership::load(&scope, &lock_with(&["a"]), &[], &[]).unwrap();
        assert_eq!(
            loaded,
            Ownership::Legacy(Owned::from_lock(&lock_with(&["a"])))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_name_removed_from_a_shared_dir_is_not_reclaimed_by_an_alias() {
        // codex and amp share `.agents/skills/`: once codex removed `old`
        // there, amp failing part-way must not record it as its own again.
        let (root, scope, _home) = sandbox("alias");
        make_skills_dir(&scope, "codex");
        let mut before = Ownership::Recorded(BTreeMap::new());
        before.grant("codex", &Owned::from_lock(&lock_with(&["old"])));
        before.grant("amp", &Owned::from_lock(&lock_with(&["old"])));
        before.save(&scope).unwrap();
        let targets = ["codex".to_string(), "amp".to_string()];
        let err = installed_err(&scope, &before, &targets, &lock_with(&["fresh", "more"]), 1);
        assert!(err.contains("boom"), "{err}");
        let after = Ownership::load(&scope, &Lockfile::default(), &[], &targets).unwrap();
        assert_eq!(after.for_target("codex").skills, set(&["fresh", "more"]));
        assert_eq!(after.for_target("amp").skills, set(&["fresh"]));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The record never claims a name an install removed, even when the save
    /// after the last target fails: it was released before the removal.
    #[cfg(unix)]
    #[test]
    fn a_removed_name_is_released_before_the_final_save() {
        use std::os::unix::fs::PermissionsExt;
        let (root, scope, _home) = sandbox("final");
        make_skills_dir(&scope, "codex");
        let dir = crate::vendor::for_target("codex")
            .unwrap()
            .skills_dir(&scope)
            .unwrap();
        std::fs::create_dir_all(dir.join("old")).unwrap();
        let mut before = Ownership::Recorded(BTreeMap::new());
        before.grant("codex", &Owned::from_lock(&lock_with(&["old"])));
        before.save(&scope).unwrap();
        let record_dir = Ownership::path(&scope)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let lock = lock_with(&["fresh"]);
        let err = before
            .install(
                &scope,
                &["codex".to_string()],
                &Owned::from_lock(&lock),
                |_, _, claim| {
                    claim("old")?;
                    std::fs::remove_dir_all(dir.join("old"))?;
                    claim("fresh")?;
                    std::fs::create_dir_all(dir.join("fresh"))?;
                    // The record can't be written from here on.
                    std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o555))?;
                    Ok(())
                },
            )
            .unwrap_err();
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(format!("{err:#}").contains("ermission"), "{err:#}");
        let after = Ownership::load(&scope, &Lockfile::default(), &[], &[]).unwrap();
        assert_eq!(after.for_target("codex").skills, set(&["fresh"]));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn skills_without_a_known_dir_identity_are_withheld() {
        // No identity on either side (no birth time, or no dir) must not
        // count as a match: names are kept only for a present, matching one.
        let (root, scope, _home) = sandbox("unbound");
        let p = Ownership::path(&scope).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"targets":{"codex":{"skills":["a"]}}}"#).unwrap();
        let loaded = Ownership::load(&scope, &lock_with(&[]), &[], &[]).unwrap();
        assert_eq!(loaded.for_target("codex").skills, set(&[]));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_names_that_escape_the_target_dir() {
        let (root, scope, _home) = sandbox("hostile");
        let path = Ownership::path(&scope).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for owned in [
            r#"{"skills":[".."]}"#,
            r#"{"skills":["../x"]}"#,
            r#"{"skills":["a\\b"]}"#,
            r#"{"skills":[""]}"#,
            r#"{"plugins":["../kit"]}"#,
        ] {
            std::fs::write(&path, format!(r#"{{"targets":{{"codex":{owned}}}}}"#)).unwrap();
            let err = format!(
                "{:#}",
                Ownership::load(&scope, &Lockfile::default(), &[], &[]).unwrap_err()
            );
            assert!(err.contains("invalid skill name"), "{owned}: {err}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
