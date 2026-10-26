//! The `ai.json` model: the [`Manifest`] and [`SkillSpec`] types and the
//! kind-agnostic view over a manifest's skill and plugin maps.

use super::de::{deserialize_unique_plugins, deserialize_unique_skills, present};
use super::DependencyKind;
use crate::ids::DepName;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The user-authored, VCS-committed declaration file (`ai.json`). Its shape
/// mirrors `schema/ai.schema.json` field for field — unknown keys and explicit
/// `null`s are rejected here too — which the `schema_parity` tests guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Optional editor hint pointing at the schema; kept so saving the
    /// manifest preserves it.
    #[serde(
        rename = "$schema",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub schema: Option<String>,
    /// Target vendors, e.g. `["claude", "copilot"]`.
    pub targets: Vec<String>,
    /// Opt-in gate for transitive dependency resolution. When `true`, spm reads
    /// each resolved skill's own co-located `ai.json` and recursively resolves
    /// the skills it declares. Default `false`: nested manifests are ignored and
    /// behavior is exactly as before. Read **only** from the root project's own
    /// manifest — a dependency's nested `ai.json` cannot re-enable recursion
    /// (that field is rejected there), so a single opt-out at the root is final.
    #[serde(
        default,
        rename = "resolveTransitive",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub resolve_transitive: bool,
    /// Project source allowlist (see `crate::policy`): when set, every skill and
    /// plugin source — direct or transitive — must match one of these patterns
    /// (and the machine policy, if any). Read **only** from the root project's
    /// manifest; a dependency's nested `ai.json` declaring it is rejected.
    #[serde(
        default,
        rename = "allowedSources",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub allowed_sources: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_unique_skills")]
    pub skills: BTreeMap<DepName, SkillSpec>,
    /// Full-plugin dependencies (agents/MCP servers/hooks/scripts + bundled
    /// skills), keyed by local name. Unlike a `skills` entry — which points at a
    /// single `SKILL.md` folder — a plugin `path` points at a Claude Code plugin
    /// root (a dir holding `.claude-plugin/plugin.json`).
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_unique_plugins"
    )]
    pub plugins: BTreeMap<DepName, SkillSpec>,
}

/// A single skill dependency. Exactly one of tag/branch/commit/version selects
/// the version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSpec {
    pub git: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub tag: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub branch: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub commit: Option<String>,
    /// A semver requirement (`^1.2`, `~1.4`, `>=1.2, <2`) resolved to the
    /// highest matching semver tag. Written as `version` in `ai.json`.
    #[serde(
        rename = "version",
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub range: Option<String>,
    /// Optional subdirectory inside the repo (monorepo of skills).
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "present"
    )]
    pub path: Option<String>,
}

impl Manifest {
    /// The map holding dependencies of `kind`.
    pub fn deps(&self, kind: DependencyKind) -> &BTreeMap<DepName, SkillSpec> {
        match kind {
            DependencyKind::Skill => &self.skills,
            DependencyKind::Plugin => &self.plugins,
        }
    }

    /// The map holding dependencies of `kind`, for editing.
    pub fn deps_mut(&mut self, kind: DependencyKind) -> &mut BTreeMap<DepName, SkillSpec> {
        match kind {
            DependencyKind::Skill => &mut self.skills,
            DependencyKind::Plugin => &mut self.plugins,
        }
    }

    /// Every declared dependency with its kind: skills, then plugins, each in
    /// name order.
    pub fn dependencies(&self) -> impl Iterator<Item = (DependencyKind, &DepName, &SkillSpec)> {
        DependencyKind::ALL
            .into_iter()
            .flat_map(move |kind| self.deps(kind).iter().map(move |(n, s)| (kind, n, s)))
    }

    /// The kind `name` is declared as, if it is declared at all. Names are
    /// unique across kinds (enforced by [`Manifest::parse`]); should a
    /// hand-built manifest still repeat one, the skill wins.
    pub fn kind_of(&self, name: &str) -> Option<DependencyKind> {
        DependencyKind::ALL
            .into_iter()
            .find(|&kind| self.deps(kind).contains_key(name))
    }

    /// Whether `name` is declared as a skill or a plugin.
    pub fn contains_dependency(&self, name: &str) -> bool {
        self.kind_of(name).is_some()
    }

    /// Whether any skill or plugin is declared.
    pub fn declares_any(&self) -> bool {
        self.dependencies().next().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The kind-agnostic view: skills then plugins, each in name order, with
    /// name lookups spanning both maps and edits landing in the kind's map.
    #[test]
    fn dependencies_span_both_kinds_skills_first() {
        let dep = r#"{"git":"u","branch":"main"}"#;
        let text = format!(
            r#"{{"targets":["claude"],"skills":{{"b":{dep},"a":{dep}}},"plugins":{{"0p":{dep}}}}}"#
        );
        let mut m = Manifest::parse(&text, Path::new("ai.json")).unwrap();
        let all: Vec<_> = m
            .dependencies()
            .map(|(k, n, _)| (k, n.to_string()))
            .collect();
        assert_eq!(
            all,
            [
                (DependencyKind::Skill, "a".to_string()),
                (DependencyKind::Skill, "b".to_string()),
                (DependencyKind::Plugin, "0p".to_string()),
            ]
        );
        assert!(m.declares_any());
        assert!(m.contains_dependency("0p") && m.contains_dependency("a"));
        assert!(!m.contains_dependency("missing"));
        assert_eq!(m.kind_of("missing"), None);

        let spec = m.deps_mut(DependencyKind::Plugin).remove("0p").unwrap();
        assert_eq!(m.kind_of("0p"), None);
        m.deps_mut(DependencyKind::Skill)
            .insert(crate::ids::dep("0p"), spec);
        assert_eq!(m.kind_of("0p"), Some(DependencyKind::Skill));
        assert_eq!(m.deps(DependencyKind::Plugin).len(), 0);

        m.deps_mut(DependencyKind::Skill).clear();
        assert!(!m.declares_any());
    }
}
