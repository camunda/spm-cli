//! How the reporting commands (`list`, `status`, `outdated`, `doctor`) see a
//! scope's dependencies: the one definition of which entries are direct,
//! transitive or plugins, and who requested a transitive skill.

use crate::ids::DepName;
use crate::lockfile::{LockedSkill, Lockfile};
use crate::manifest::{DependencyKind, Manifest, SkillSpec};

/// `ai.json` and `ai.lock`, read together.
pub(super) struct DependencyIndex<'a> {
    manifest: &'a Manifest,
    lock: &'a Lockfile,
}

/// A dependency declared in `ai.json`, with its `ai.lock` pin if it has one.
pub(super) struct Direct<'a> {
    pub(super) name: &'a DepName,
    pub(super) spec: &'a SkillSpec,
    pub(super) locked: Option<&'a LockedSkill>,
}

/// An `ai.lock` entry, classified against `ai.json`.
pub(super) struct Locked<'a> {
    pub(super) kind: DependencyKind,
    pub(super) name: &'a DepName,
    pub(super) locked: &'a LockedSkill,
    /// Its `ai.json` request, when declared there as this kind.
    pub(super) spec: Option<&'a SkillSpec>,
    /// Pulled in by the direct skills in its `requested_by`, not declared.
    pub(super) transitive: bool,
}

impl<'a> DependencyIndex<'a> {
    pub(super) fn new(manifest: &'a Manifest, lock: &'a Lockfile) -> Self {
        Self { manifest, lock }
    }

    /// The `kind` dependencies `ai.json` declares, in its order.
    pub(super) fn direct(&self, kind: DependencyKind) -> impl Iterator<Item = Direct<'a>> + 'a {
        let lock = self.lock;
        self.manifest
            .deps(kind)
            .iter()
            .map(move |(name, spec)| Direct {
                name,
                spec,
                locked: lock.deps(kind).get(name),
            })
    }

    /// Every `ai.lock` entry: skills, then plugins, each by name.
    pub(super) fn locked(&self) -> impl Iterator<Item = Locked<'a>> + 'a {
        let manifest = self.manifest;
        self.lock.dependencies().map(move |(kind, name, locked)| {
            let spec = manifest.deps(kind).get(name);
            Locked {
                kind,
                name,
                locked,
                spec,
                transitive: is_transitive(kind, locked, spec),
            }
        })
    }

    /// The transitively-resolved skills, by name.
    pub(super) fn transitive(&self) -> impl Iterator<Item = Locked<'a>> + 'a {
        self.locked().filter(|l| l.transitive)
    }

    /// `ai.lock` entries whose name `ai.json` doesn't declare (as any kind):
    /// transitive skills and stale pins.
    pub(super) fn undeclared(&self) -> impl Iterator<Item = Locked<'a>> + 'a {
        let manifest = self.manifest;
        self.locked()
            .filter(move |l| !manifest.contains_dependency(l.name))
    }

    /// Who requested `name`, when it is a transitive skill; empty otherwise.
    pub(super) fn requested_by(&self, name: &str) -> &'a [DepName] {
        self.lock
            .skills
            .get(name)
            .filter(|l| is_transitive(DependencyKind::Skill, l, self.manifest.skills.get(name)))
            .map_or(&[], |l| l.requested_by.as_slice())
    }

    /// Where a walk up from a transitive skill toward `ai.json` goes next from
    /// `name`: the `requested_by` of its pin (of either kind) — or nowhere once
    /// `ai.json` declares `name` (as any kind), since that declaration is what
    /// to edit, whatever a stale `ai.lock` says requested it.
    pub(super) fn requesters(&self, name: &str) -> &'a [DepName] {
        if self.manifest.contains_dependency(name) {
            return &[];
        }
        self.lock
            .get_dependency(name)
            .map_or(&[], |l| l.requested_by.as_slice())
    }
}

/// Only skills are pulled in transitively. A name declared in `ai.json` is
/// direct even if a stale or hand-edited `ai.lock` left a `requested_by` on it.
fn is_transitive(kind: DependencyKind, locked: &LockedSkill, spec: Option<&SkillSpec>) -> bool {
    kind == DependencyKind::Skill && !locked.requested_by.is_empty() && spec.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::dep;
    use crate::lockfile::tests::entry;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn requested(mut l: LockedSkill, by: &[&str]) -> LockedSkill {
        l.requested_by = by.iter().map(|n| dep(n)).collect();
        l
    }

    /// `top` is direct (its stray `requested_by` notwithstanding), `leaf` is
    /// transitive, `old` is a stale pin, `kit` a plugin and `new` declared
    /// but never locked.
    fn fixture() -> (Manifest, Lockfile) {
        let d = r#"{"git":"u","branch":"main"}"#;
        let manifest = Manifest::parse(
            &format!(
                r#"{{"targets":["claude"],"skills":{{"top":{d},"new":{d}}},"plugins":{{"kit":{d}}}}}"#
            ),
            Path::new("ai.json"),
        )
        .unwrap();
        let s = |c| entry("https://h/s.git", c, "branch:main");
        let lock = Lockfile {
            skills: BTreeMap::from([
                (dep("top"), requested(s('a'), &["other"])),
                (dep("leaf"), requested(s('b'), &["top"])),
                (dep("old"), s('c')),
            ]),
            plugins: BTreeMap::from([(dep("kit"), requested(s('d'), &["top"]))]),
            ..Default::default()
        };
        (manifest, lock)
    }

    fn names<'a>(it: impl Iterator<Item = &'a DepName>) -> Vec<&'a str> {
        it.map(DepName::as_str).collect()
    }

    #[test]
    fn direct_lists_declared_dependencies_with_their_pins() {
        let (manifest, lock) = fixture();
        let index = DependencyIndex::new(&manifest, &lock);
        let skills: Vec<_> = index.direct(DependencyKind::Skill).collect();
        assert_eq!(names(skills.iter().map(|d| d.name)), ["new", "top"]);
        assert!(skills[0].locked.is_none());
        assert_eq!(skills[1].locked.unwrap().commit.as_str(), "a".repeat(40));
        let plugins: Vec<_> = index.direct(DependencyKind::Plugin).collect();
        assert_eq!(names(plugins.iter().map(|d| d.name)), ["kit"]);
        assert!(plugins[0].locked.is_some());
    }

    #[test]
    fn only_undeclared_requested_skills_are_transitive() {
        let (manifest, lock) = fixture();
        let index = DependencyIndex::new(&manifest, &lock);
        let all: Vec<_> = index
            .locked()
            .map(|l| (l.kind, l.name.as_str(), l.spec.is_some(), l.transitive))
            .collect();
        assert_eq!(
            all,
            [
                (DependencyKind::Skill, "leaf", false, true),
                (DependencyKind::Skill, "old", false, false),
                (DependencyKind::Skill, "top", true, false),
                (DependencyKind::Plugin, "kit", true, false),
            ]
        );
        assert_eq!(names(index.transitive().map(|l| l.name)), ["leaf"]);
        assert_eq!(names(index.undeclared().map(|l| l.name)), ["leaf", "old"]);
    }

    #[test]
    fn requested_by_is_empty_unless_transitive() {
        let (manifest, lock) = fixture();
        let index = DependencyIndex::new(&manifest, &lock);
        assert_eq!(names(index.requested_by("leaf").iter()), ["top"]);
        for name in ["top", "old", "kit", "new", "missing"] {
            assert!(index.requested_by(name).is_empty(), "{name}");
        }
    }

    /// The walk up follows any undeclared pin's requesters and stops at every
    /// name `ai.json` declares, of either kind.
    #[test]
    fn requesters_stop_at_any_declared_name() {
        let (manifest, mut lock) = fixture();
        lock.skills
            .insert(dep("kit"), requested(lock.plugins["kit"].clone(), &["x"]));
        let index = DependencyIndex::new(&manifest, &lock);
        assert_eq!(names(index.requesters("leaf").iter()), ["top"]);
        for name in ["top", "kit", "new", "old", "missing"] {
            assert!(index.requesters(name).is_empty(), "{name}");
        }
    }

    /// The namespace is shared: a lock skill named like a declared plugin is
    /// declared, so it is neither transitive-undeclared nor stale.
    #[test]
    fn undeclared_spans_both_kinds_of_the_manifest() {
        let (manifest, mut lock) = fixture();
        lock.skills
            .insert(dep("kit"), entry("https://h/s.git", 'e', "branch:main"));
        let index = DependencyIndex::new(&manifest, &lock);
        assert_eq!(names(index.undeclared().map(|l| l.name)), ["leaf", "old"]);
    }
}
