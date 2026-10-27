//! The resolved dependencies handed to the vendors: a skill or a full plugin,
//! named and located in the global store. Shared by the resolvers that produce
//! them (`transitive`, `plugin`'s bundled skills, `cli`'s sync) and the
//! `vendor` adapters that consume them.

use crate::ids::DepName;
use std::path::PathBuf;

/// A resolved skill ready to be wired into a vendor: a name plus the absolute
/// path to its content in the global store.
pub struct MaterializedSkill {
    pub name: DepName,
    pub path: PathBuf,
    /// Canonical repo checkout root that bounds symlink following when this
    /// skill is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub root: PathBuf,
}

/// A resolved full plugin ready to be registered with a vendor that supports
/// more than skills: a name (the manifest key) plus the absolute path to the
/// plugin root in the global store (the dir holding `.claude-plugin/plugin.json`,
/// `agents/`, `skills/`, scripts, hooks, …).
pub struct MaterializedPlugin {
    pub name: DepName,
    pub path: PathBuf,
    /// Canonical repo checkout root that bounds symlink following when this
    /// plugin is copied into a vendor dir (see [`crate::store::Ensured::root`]).
    pub root: PathBuf,
}
