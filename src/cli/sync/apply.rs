//! The apply phase of a sync: write a checked [`Plan`] into the targets, then
//! `ai.lock` and the links file — and the private snapshot linked content is
//! materialized from.

use super::{install_command, Apply, Plan};
use crate::ids::DepName;
use crate::links::{Linked, Links};
use crate::owned::Owned;
use crate::scope::Scope;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

impl Plan {
    /// Project the planned skills + plugins into every configured target, then
    /// persist `ai.lock`. Returns the number of locked skills + plugins.
    pub(super) fn apply(self, scope: &Scope) -> Result<usize> {
        let lock = &self.lock;
        if self.apply == Apply::LockOnly {
            // Nothing is installed or removed now: the targets keep what they
            // hold, and the next install or clean must still know it is spm's.
            self.previously_owned.freeze(scope, &self.targets)?;
            lock.save(&self.dir)?;
            println!(
                "wrote ai.lock only; run `{}` to install into the targets",
                install_command(scope)
            );
            return Ok(lock.dependencies().count());
        }
        if self.declares_any {
            println!("materializing: {}", self.targets.join(", "));
        }
        // Skills copied from a linked plugin's working tree are listed in the
        // links file (an spm without an ownership record falls back to it).
        // Record the planned ones alongside the previous ones before any
        // vendor writes; the final links file (below) narrows it once all
        // succeeded. Each target's record claims them as they are written.
        if let Some(links) = &self.links {
            let on_disk = Links::load(scope)?;
            let mut pending = on_disk.clone();
            pending.managed.extend(links.managed.iter().cloned());
            pending.managed.sort();
            pending.managed.dedup();
            if pending != on_disk {
                pending.save(scope)?;
            }
        }
        let desired = Owned {
            skills: self.materialized.iter().map(|m| m.name.clone()).collect(),
            plugins: self.plugins.iter().map(|p| p.name.clone()).collect(),
        };
        self.previously_owned
            .install(scope, &self.targets, &desired, |i, owned, claim| {
                let vendor = &self.vendors[i];
                vendor.materialize(
                    scope,
                    &lock.id,
                    &self.materialized,
                    &owned.skill_names(),
                    claim,
                )?;
                match vendor.plugins() {
                    Some(p) => {
                        p.materialize_plugins(scope, &lock.id, &self.plugins, &owned.plugin_names())
                    }
                    None => Ok(()),
                }
            })?;
        // Only persist `ai.lock` once every fallible step (resolution, fetching,
        // collision checks, and vendor materialization) has succeeded —
        // otherwise a failed `install` could leave behind a lockfile pointing at
        // state that was never actually materialized. `--locked` never writes
        // it: it already matches (checked in `plan`).
        if !self.locked {
            lock.save(&self.dir)?;
        }
        if let Some(links) = &self.links {
            links.save(scope)?;
        }
        Ok(lock.dependencies().count())
    }
}

/// Private copies of the linked directories, made with the same safe-copy
/// rules as materialization (no `.git`, no symlink leaving the directory), and
/// removed once the sync is done.
pub(super) struct Snapshots {
    pub(super) root: PathBuf,
}

impl Snapshots {
    /// Snapshot every link in `linked`, pointing its `content` at the copy.
    pub(super) fn take(linked: &mut BTreeMap<DepName, Linked>) -> Result<Option<Self>> {
        if linked.is_empty() {
            return Ok(None);
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!("spm-links-{}-{nanos}", std::process::id()));
        // A fresh directory only its owner can enter: the copies may come from
        // a working copy other users can't read.
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut b = builder;
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b
        };
        builder
            .create(&root)
            .with_context(|| format!("creating {}", root.display()))?;
        let snapshots = Self {
            root: std::fs::canonicalize(&root)
                .with_context(|| format!("resolving {}", root.display()))?,
        };
        // Numbered slots, not names: names that differ only in case (e.g. a
        // skill `Foo` and a plugin `foo`) must not share one on a
        // case-insensitive filesystem.
        for (slot, (name, l)) in linked.iter_mut().enumerate() {
            if crate::fsutil::contains(&l.dir, &snapshots.root) {
                bail!(
                    "linked directory `{}` for `{name}` contains spm's temporary directory \
                     `{}` — link the skill or plugin directory itself",
                    l.display,
                    snapshots.root.display()
                );
            }
            let content = snapshots.root.join(slot.to_string());
            crate::fsutil::copy_tree(&l.dir, &content, &l.dir)
                .with_context(|| format!("snapshotting linked `{name}` ({})", l.display))?;
            l.content = content;
        }
        Ok(Some(snapshots))
    }
}

impl Drop for Snapshots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::sync::tests::source;
    use crate::ids::dep;

    fn linked(dir: &std::path::Path) -> Linked {
        Linked {
            dir: dir.to_path_buf(),
            content: dir.to_path_buf(),
            display: dir.display().to_string(),
        }
    }

    /// Each link gets its own slot, even when names only differ in case (a
    /// skill `Foo` and a plugin `foo`), which would share a directory on a
    /// case-insensitive filesystem; the root is private and removed on drop.
    #[test]
    fn snapshots_are_private_distinct_and_temporary() {
        let skill = source("skill", &[("SKILL.md", "skill")]);
        let plugin = source("plugin", &[(".claude-plugin/plugin.json", "{}")]);
        let mut map = BTreeMap::from([(dep("Foo"), linked(&skill)), (dep("foo"), linked(&plugin))]);
        let snapshots = Snapshots::take(&mut map).unwrap().unwrap();
        let (s, p) = (&map["Foo"].content, &map["foo"].content);
        assert_eq!(
            std::fs::read_to_string(s.join("SKILL.md")).unwrap(),
            "skill"
        );
        assert!(
            !s.join(".claude-plugin").exists(),
            "plugin merged into skill"
        );
        assert!(!p.join("SKILL.md").exists(), "skill merged into plugin");
        assert!(p.join(".claude-plugin/plugin.json").is_file());
        assert_eq!(map["Foo"].dir, skill, "the working copy stays protected");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&snapshots.root)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let root = snapshots.root.clone();
        drop(snapshots);
        assert!(!root.exists());
        std::fs::remove_dir_all(skill).unwrap();
        std::fs::remove_dir_all(plugin).unwrap();
    }
}
