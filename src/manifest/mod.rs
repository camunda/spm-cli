//! The user-authored `ai.json`: its model ([`Manifest`], [`SkillSpec`]), the
//! serde helpers that keep its deserialization as strict as the schema, the
//! semantic validation of a dependency spec, loading/saving of the root
//! manifest, and the lenient nested manifest a dependency may ship
//! ([`DependencyManifest`]).

mod de;
mod dependency;
mod load;
mod model;
pub(crate) mod testutil;
mod validate;

pub use dependency::DependencyManifest;
pub use model::{Manifest, SkillSpec};
pub use validate::validate_subpath;

pub const MANIFEST_FILE: &str = "ai.json";

/// Which map of `ai.json`/`ai.lock` a dependency lives in. Skills and plugins
/// share one flat name namespace and the same resolve/pin rules; the kind
/// selects the map and the few kind-specific steps (a plugin's bundled skills,
/// a skill's transitive provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyKind {
    Skill,
    Plugin,
}

impl DependencyKind {
    /// Every kind, in the order dependencies are iterated, resolved and
    /// reported: skills before plugins.
    pub const ALL: [Self; 2] = [Self::Skill, Self::Plugin];

    /// The kind an `add`/`remove` targets: `--plugin` selects plugins.
    pub fn from_plugin_flag(plugin: bool) -> Self {
        if plugin {
            Self::Plugin
        } else {
            Self::Skill
        }
    }

    /// The singular noun used in messages (`skill`, `plugin`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Plugin => "plugin",
        }
    }
}

impl std::fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod schema_parity;
