//! The resolved dependencies handed to the vendors: a skill or a full plugin,
//! named and located in the global store. Shared by the resolvers that produce
//! them (`transitive`, `cli`'s sync) and the `vendor` adapters that consume
//! them.
//!
//! Outside tests a value here can only be built from a [`Scanned`] proof of
//! the content it points at (a plugin's bundled skills from their scanned
//! plugin), so a vendor is never handed content the pre-materialize gate did
//! not pass.

use crate::ids::DepName;
use crate::scan::Scanned;
use anyhow::{bail, Result};
use std::path::{Component, Path, PathBuf};

/// A resolved skill ready to be wired into a vendor: a name plus the absolute
/// path to its scanned content in the global store.
#[derive(Debug)]
pub struct MaterializedSkill {
    name: DepName,
    path: PathBuf,
    root: PathBuf,
}

impl MaterializedSkill {
    /// The skill `name` whose content passed the gate as `scanned`.
    pub fn new(name: DepName, scanned: Scanned) -> Self {
        Self {
            name,
            path: scanned.path().to_path_buf(),
            root: scanned.root().to_path_buf(),
        }
    }

    /// A skill whose content was never scanned, for vendor tests only.
    #[cfg(test)]
    pub fn unscanned(name: DepName, path: PathBuf, root: PathBuf) -> Self {
        Self { name, path, root }
    }

    pub fn name(&self) -> &DepName {
        &self.name
    }

    /// The skill's content.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Canonical repo checkout root that bounds symlink following when this
    /// skill is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// A resolved full plugin ready to be registered with a vendor that supports
/// more than skills: a name (the manifest key) plus the absolute path to the
/// scanned plugin root in the global store (the dir holding
/// `.claude-plugin/plugin.json`, `agents/`, `skills/`, scripts, hooks, …).
#[derive(Debug)]
pub struct MaterializedPlugin {
    name: DepName,
    scanned: Scanned,
}

impl MaterializedPlugin {
    /// The plugin `name` whose whole tree passed the gate as `scanned`.
    pub fn new(name: DepName, scanned: Scanned) -> Self {
        Self { name, scanned }
    }

    /// A plugin whose content was never scanned, for vendor tests only: it
    /// bundles no skill.
    #[cfg(test)]
    pub fn unscanned(name: DepName, path: PathBuf, root: PathBuf) -> Self {
        Self {
            name,
            scanned: Scanned::assumed(path, root),
        }
    }

    pub fn name(&self) -> &DepName {
        &self.name
    }

    /// The plugin root.
    pub fn path(&self) -> &Path {
        self.scanned.path()
    }

    /// Canonical repo checkout root that bounds symlink following when this
    /// plugin is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub fn root(&self) -> &Path {
        self.scanned.root()
    }

    /// The skill `name` this plugin bundles at `path`, covered by the plugin's
    /// scan: `path` must lie under the plugin root and be a directory the scan
    /// walked within the same boundary (not one beneath a `.git` or a symlink
    /// the walk skipped), else this fails rather than hand out unscanned
    /// content.
    pub fn bundled_skill(&self, name: DepName, path: PathBuf) -> Result<MaterializedSkill> {
        let inside = path.strip_prefix(self.path()).is_ok_and(|rel| {
            rel.components().next().is_some()
                && rel.components().all(|c| matches!(c, Component::Normal(_)))
        });
        if !inside {
            bail!(
                "skill `{name}` bundled in plugin `{}` lies outside its scanned root {}",
                self.name,
                self.path().display()
            );
        }
        if !self.scanned.covers(&path) {
            bail!(
                "skill `{name}` bundled in plugin `{}` at {} is not covered by its scan",
                self.name,
                path.display()
            );
        }
        Ok(MaterializedSkill {
            name,
            path,
            root: self.root().to_path_buf(),
        })
    }
}

/// One skill a sync selects for the targets: its scanned content or — in a
/// read-only check, which materializes nothing — only its name, which the
/// collision checks still need.
#[derive(Debug)]
pub enum Selected {
    Materialized(MaterializedSkill),
    Named(DepName),
}

impl Selected {
    pub fn name(&self) -> &DepName {
        match self {
            Self::Materialized(skill) => skill.name(),
            Self::Named(name) => name,
        }
    }

    /// The scanned skill to hand to the vendors, if this one was materialized.
    pub fn materialized(self) -> Option<MaterializedSkill> {
        match self {
            Self::Materialized(skill) => Some(skill),
            Self::Named(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> MaterializedPlugin {
        MaterializedPlugin::unscanned(
            DepName::try_from("p").unwrap(),
            PathBuf::from("/store/repo/plugin"),
            PathBuf::from("/store/repo"),
        )
    }

    /// A unique scratch checkout for one filesystem test.
    fn checkout(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spm-materialized-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn write_skill(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "Greet warmly.\n").unwrap();
    }

    /// A skill the plugin's scan walked is bundled, with the plugin's boundary.
    #[test]
    fn bundled_skill_walked_by_the_plugin_scan_is_covered() {
        let repo = checkout("walked");
        let skill = repo.join("plugin/skills/a");
        write_skill(&skill);
        let scanned = crate::scan::enforce("p", &repo.join("plugin"), &repo).unwrap();
        let plugin = MaterializedPlugin::new(DepName::try_from("p").unwrap(), scanned);
        let got = plugin.bundled_skill(DepName::try_from("a").unwrap(), skill.clone());
        std::fs::remove_dir_all(&repo).unwrap();
        let got = got.unwrap();
        assert_eq!((got.path(), got.root()), (&*skill, &*repo));
    }

    /// A path lexically under the plugin root whose content the plugin's scan
    /// skipped is refused: through a symlink named `.git` (skipped by name), an
    /// alias of a real `.git` dir, or a hop out of the checkout and back in.
    #[cfg(unix)]
    #[test]
    fn bundled_skill_beneath_a_part_the_plugin_scan_skipped_is_refused() {
        use std::os::unix::fs::symlink;
        let repo = checkout("skipped");
        let outside = checkout("skipped-outside");
        let plugin_dir = repo.join("plugin");
        write_skill(&repo.join("shared/a"));
        write_skill(&plugin_dir.join("real/.git/a"));
        std::fs::create_dir_all(plugin_dir.join("assets")).unwrap();
        symlink(repo.join("shared"), plugin_dir.join("assets/.git")).unwrap();
        symlink(plugin_dir.join("real/.git"), plugin_dir.join("alias")).unwrap();
        symlink(&outside, plugin_dir.join("out")).unwrap();
        symlink(repo.join("shared"), outside.join("back")).unwrap();
        let scanned = crate::scan::enforce("p", &plugin_dir, &repo).unwrap();
        let plugin = MaterializedPlugin::new(DepName::try_from("p").unwrap(), scanned);
        let got: Vec<_> = ["assets/.git/a", "alias/a", "out/back/a"]
            .into_iter()
            .map(|rel| {
                let path = plugin_dir.join(rel);
                (
                    rel,
                    plugin.bundled_skill(DepName::try_from("a").unwrap(), path),
                )
            })
            .collect();
        std::fs::remove_dir_all(&repo).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
        for (rel, got) in got {
            let err = got.unwrap_err().to_string();
            assert!(err.contains("not covered by its scan"), "{rel}: {err}");
        }
    }

    /// Content outside the plugin root (a sibling, the root itself, or one
    /// reached through `..`) was not walked by the plugin's scan.
    #[test]
    fn bundled_skill_outside_the_plugin_root_is_refused() {
        for path in [
            "/store/repo/other/skills/a",
            "/store/repo/plugin",
            "/store/repo/plugin/../other/a",
            "/store/repo/pluginx/a",
        ] {
            let err = plugin()
                .bundled_skill(DepName::try_from("a").unwrap(), PathBuf::from(path))
                .unwrap_err()
                .to_string();
            assert!(err.contains("outside its scanned root"), "{path}: {err}");
        }
    }
}
