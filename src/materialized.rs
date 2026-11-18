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
    path: PathBuf,
    root: PathBuf,
}

impl MaterializedPlugin {
    /// The plugin `name` whose whole tree passed the gate as `scanned`.
    pub fn new(name: DepName, scanned: Scanned) -> Self {
        Self {
            name,
            path: scanned.path().to_path_buf(),
            root: scanned.root().to_path_buf(),
        }
    }

    /// A plugin whose content was never scanned, for vendor tests only.
    #[cfg(test)]
    pub fn unscanned(name: DepName, path: PathBuf, root: PathBuf) -> Self {
        Self { name, path, root }
    }

    pub fn name(&self) -> &DepName {
        &self.name
    }

    /// The plugin root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Canonical repo checkout root that bounds symlink following when this
    /// plugin is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The skill `name` this plugin bundles at `path`, covered by the plugin's
    /// scan: `path` must lie under the plugin root (walked by the scan within
    /// the same boundary), else this fails rather than hand out unscanned
    /// content.
    pub fn bundled_skill(&self, name: DepName, path: PathBuf) -> Result<MaterializedSkill> {
        let inside = path.strip_prefix(&self.path).is_ok_and(|rel| {
            rel.components().next().is_some()
                && rel.components().all(|c| matches!(c, Component::Normal(_)))
        });
        if !inside {
            bail!(
                "skill `{name}` bundled in plugin `{}` lies outside its scanned root {}",
                self.name,
                self.path.display()
            );
        }
        Ok(MaterializedSkill {
            name,
            path,
            root: self.root.clone(),
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

    /// A bundled skill inherits the plugin's scanned root and boundary.
    #[test]
    fn bundled_skill_under_the_plugin_root_is_covered_by_its_scan() {
        let path = PathBuf::from("/store/repo/plugin/skills/a");
        let skill = plugin()
            .bundled_skill(DepName::try_from("a").unwrap(), path.clone())
            .unwrap();
        assert_eq!(
            (skill.name().as_str(), skill.path(), skill.root()),
            ("a", &*path, Path::new("/store/repo"))
        );
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
