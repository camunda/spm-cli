use super::dirskills::{copy_skills_into, remove_managed};
use super::{MaterializedSkill, Vendor};
use crate::scope::Scope;
use crate::{gitignore, paths};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Google Gemini CLI adapter.
///
/// Gemini CLI auto-discovers skills from `SKILL.md` files one directory deep
/// under a `skills` root, in two discovery tiers spm maps onto its two scopes:
///
/// - **Project** ([`Scope::Project`]) — workspace skills live in
///   `<root>/.gemini/skills/<name>/`. Gemini treats this dir as team-shared and
///   version-controlled, so it is *shared* with the user's own hand-authored
///   skills: spm never wipes it and adds only its own entries. spm's materialized
///   skills must still stay out of VCS, so each spm-managed skill subdir is added
///   to the project's `.gitignore` (the user's own committed skills are left
///   ignorable-free).
///
/// - **Global** ([`Scope::Global`]) — user skills live in
///   `~/.gemini/skills/<name>/`, available across every project. Also shared with
///   the user's own skills; no `.gitignore` (there is no repo).
///
/// In both scopes the dir is shared, so spm removes only the entries it
/// previously owned (from `previously_managed`) plus the ones it is about to
/// write, and never touches anything else. Both tiers also accept an
/// `.agents/skills` alias, but spm writes the tool-native `.gemini/skills` path
/// to keep each vendor's footprint isolated and unambiguous.
pub struct Gemini;

/// Project-scope skills dir segments (relative to the project root).
const PROJECT_DIR: [&str; 2] = [".gemini", "skills"];
/// Global-scope skills dir segments (relative to the user's home dir).
const GLOBAL_DIR: [&str; 2] = [".gemini", "skills"];

const GITIGNORE_COMMENT: &str =
    "# spm-managed Gemini skills — materialized locally by `spm`, not committed.";

impl Vendor for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn materialize(
        &self,
        scope: &Scope,
        _project_id: &str,
        skills: &[MaterializedSkill],
        previously_managed: &[String],
    ) -> Result<()> {
        let dir = skills_dir(scope)?;
        // The dir is shared with the user's own skills in both scopes: remove
        // only the entries spm owns (previously-managed ∪ about-to-write), then
        // copy the current set. Never wipe the whole directory.
        let mut owned: BTreeSet<&str> = previously_managed.iter().map(String::as_str).collect();
        owned.extend(skills.iter().map(|s| s.name.as_str()));
        remove_managed(&dir, owned.iter().copied())?;
        if !skills.is_empty() {
            copy_skills_into(&dir, skills)?;
        }
        // Keep spm's materialized skills out of VCS without ignoring the user's
        // own committed skills in the same shared dir: ignore each managed
        // subdir by name.
        if let Scope::Project { root } = scope {
            for s in skills {
                gitignore::ensure(root, GITIGNORE_COMMENT, &gitignore_entry(&s.name))?;
            }
        }
        Ok(())
    }

    fn clean(&self, scope: &Scope, _project_id: &str, managed: &[String]) -> Result<()> {
        // Shared dir: remove only the skills spm owns, never the whole dir.
        // `.gitignore` is intentionally left untouched: the per-skill entries spm
        // added are harmless once the materialized dirs are gone, and rewriting a
        // user-owned file on `clean` risks clobbering it.
        remove_managed(&skills_dir(scope)?, managed.iter().map(String::as_str))
    }

    fn status(&self, scope: &Scope, expected: &[String]) -> Result<super::VendorStatus> {
        // The dir is shared with the user's own skills in *both* scopes, so stale
        // detection would mislabel those — never enable it here.
        Ok(super::classify(&skills_dir(scope)?, expected, false))
    }
}

/// Absolute path of the managed skills directory for a scope.
fn skills_dir(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Project { root } => Ok(join_all(root, &PROJECT_DIR)),
        Scope::Global => Ok(join_all(&paths::home_dir()?, &GLOBAL_DIR)),
    }
}

fn join_all(base: &Path, segs: &[&str]) -> PathBuf {
    segs.iter().fold(base.to_path_buf(), |p, seg| p.join(seg))
}

/// `.gitignore` entry for a single spm-managed skill subdir.
fn gitignore_entry(skill: &str) -> String {
    format!(".gemini/skills/{skill}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spm-gemini-{name}-{}-{nanos}", std::process::id()))
    }

    fn make_skill(root: &Path, name: &str) -> MaterializedSkill {
        let src = root.join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
        MaterializedSkill {
            name: name.to_string(),
            path: src,
        }
    }

    /// Project materialize drops skills one level deep under `.gemini/skills/`
    /// and gitignores each managed subdir, without touching a user's own skill in
    /// the same shared dir, and purges a previously-managed-but-dropped skill.
    #[test]
    fn project_materialize_preserves_user_skills_gitignores_and_purges() {
        let tmp = scratch("proj-mat");
        std::fs::create_dir_all(&tmp).unwrap();
        let dir = tmp.join(".gemini").join("skills");
        std::fs::create_dir_all(&dir).unwrap();

        // A skill the user authored & committed by hand — spm must never touch it.
        let user = dir.join("user-skill");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("SKILL.md"), "user\n").unwrap();

        // A skill spm managed last time that is now dropped from the manifest.
        let dropped = dir.join("old-skill");
        std::fs::create_dir_all(&dropped).unwrap();
        std::fs::write(dropped.join("SKILL.md"), "old\n").unwrap();

        let scope = Scope::Project { root: tmp.clone() };
        let greet = make_skill(&tmp, "greet");
        Gemini
            .materialize(&scope, "spm-1234", &[greet], &["old-skill".to_string()])
            .unwrap();

        assert!(
            dir.join("greet").join("SKILL.md").exists(),
            "greet copied one level deep"
        );
        assert!(!dir.join("old-skill").exists(), "dropped skill purged");
        assert!(user.join("SKILL.md").exists(), "user's own skill preserved");
        let gi = std::fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(gi.contains(".gemini/skills/greet/"), "{gi}");
        assert!(!gi.contains("user-skill"), "user's skill not ignored: {gi}");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Global materialize/clean operate on `~/.gemini/skills` and touch only
    /// spm-managed entries. Exercised against an explicit dir (no HOME) via the
    /// shared helpers the adapter uses.
    #[test]
    fn global_clean_removes_only_managed() {
        let tmp = scratch("global-clean");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["greet", "user-skill"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
            std::fs::write(dir.join(n).join("SKILL.md"), "x\n").unwrap();
        }

        remove_managed(&dir, ["greet"].into_iter()).unwrap();

        assert!(!dir.join("greet").exists(), "managed skill removed");
        assert!(dir.join("user-skill").exists(), "user skill preserved");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn gitignore_entry_is_scoped_to_the_named_skill() {
        assert_eq!(gitignore_entry("greet"), ".gemini/skills/greet/");
    }
}
