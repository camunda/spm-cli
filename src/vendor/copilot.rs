use super::{MaterializedSkill, Vendor};
use crate::scope::Scope;
use crate::{fsutil, gitignore, paths};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// GitHub Copilot CLI adapter.
///
/// Copilot CLI auto-discovers skills from `SKILL.md` files one directory deep
/// under a `skills` root. spm supports two scopes:
///
/// - **Project** ([`Scope::Project`]) — skills are copied into
///   `.agents/skills/spm-managed-skills/<name>/` inside the project and that
///   path is added to the project's `.gitignore`, so they are available to
///   Copilot when it runs in the repo but never committed. That directory is
///   entirely spm-owned, so each materialize rebuilds it from scratch.
///
/// - **Global** ([`Scope::Global`]) — skills are copied into
///   `~/.copilot/skills/<name>/`, Copilot's user-global personal-skills dir, so
///   they are available in every project. Copilot only discovers skills a single
///   directory deep there, so spm cannot nest them under an spm-owned subdir;
///   the directory is *shared* with the user's own hand-authored skills.
///   Therefore spm never wipes it — it removes only the entries it previously
///   owned (plus the ones it is about to write) and leaves everything else
///   untouched. No `.gitignore` is written (there is no repo).
pub struct Copilot;

/// Project-scope managed dir segments (relative to the project root).
const PROJECT_DIR: [&str; 3] = [".agents", "skills", "spm-managed-skills"];

/// Global-scope skills dir segments (relative to the user's home dir).
const GLOBAL_DIR: [&str; 2] = [".copilot", "skills"];

const GITIGNORE_ENTRY: &str = ".agents/skills/spm-managed-skills/";
const GITIGNORE_COMMENT: &str =
    "# spm-managed Copilot skills — materialized locally by `spm`, not committed.";

impl Vendor for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn materialize(
        &self,
        scope: &Scope,
        _project_id: &str,
        skills: &[MaterializedSkill],
        previously_managed: &[String],
    ) -> Result<()> {
        let dir = skills_dir(scope)?;
        match scope {
            Scope::Project { root } => {
                // The whole dir is spm-owned: rebuild from scratch so removed
                // skills disappear.
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)
                        .with_context(|| format!("clearing {}", dir.display()))?;
                }
                if skills.is_empty() {
                    return Ok(());
                }
                copy_skills_into(&dir, skills)?;
                ensure_gitignored(root)
            }
            Scope::Global => {
                // The dir is shared with the user's own skills: remove only the
                // entries spm owns (previously-managed ∪ about-to-write), then
                // copy the current set. Never wipe the whole directory.
                let mut owned: BTreeSet<&str> =
                    previously_managed.iter().map(String::as_str).collect();
                owned.extend(skills.iter().map(|s| s.name.as_str()));
                remove_managed(&dir, owned.iter().copied())?;
                if !skills.is_empty() {
                    copy_skills_into(&dir, skills)?;
                }
                Ok(())
            }
        }
    }

    fn clean(&self, scope: &Scope, _project_id: &str, managed: &[String]) -> Result<()> {
        let dir = skills_dir(scope)?;
        match scope {
            Scope::Project { .. } => {
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)
                        .with_context(|| format!("removing {}", dir.display()))?;
                }
                // `.gitignore` is intentionally left untouched: the entry spm
                // added is harmless once the materialized dir is gone, and
                // rewriting a user-owned file on `clean` risks clobbering it.
            }
            Scope::Global => {
                // Shared dir: remove only the skills spm owns, never the whole dir.
                remove_managed(&dir, managed.iter().map(String::as_str))?;
            }
        }
        Ok(())
    }

    fn status(&self, scope: &Scope, expected: &[String]) -> Result<super::VendorStatus> {
        // Stale detection only makes sense for the spm-owned project dir; the
        // shared global dir holds the user's own skills too.
        let detect_stale = !scope.is_global();
        Ok(super::classify(&skills_dir(scope)?, expected, detect_stale))
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

/// Copy each skill into `dir/<name>/`, creating `dir` if needed.
fn copy_skills_into(dir: &Path, skills: &[MaterializedSkill]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for s in skills {
        let dest = dir.join(&s.name);
        // Replace any pre-existing dir of the same name so re-materialization is
        // idempotent even in the shared global dir.
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("clearing {}", dest.display()))?;
        }
        fsutil::copy_tree(&s.path, &dest)
            .with_context(|| format!("copying skill `{}` into {}", s.name, dir.display()))?;
    }
    Ok(())
}

/// Remove `dir/<name>/` for each managed `name`, ignoring absent entries and
/// leaving every other entry in `dir` (e.g. the user's own skills) untouched.
fn remove_managed<'a>(dir: &Path, names: impl Iterator<Item = &'a str>) -> Result<()> {
    for name in names {
        let target = dir.join(name);
        if target.is_dir() {
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("removing {}", target.display()))?;
        }
    }
    Ok(())
}

fn ensure_gitignored(project_root: &Path) -> Result<()> {
    gitignore::ensure(project_root, GITIGNORE_COMMENT, GITIGNORE_ENTRY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spm-copilot-{name}-{}-{nanos}", std::process::id()))
    }

    /// Create a minimal source skill dir (with a SKILL.md) to copy from.
    fn make_skill(root: &Path, name: &str) -> MaterializedSkill {
        let src = root.join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
        MaterializedSkill {
            name: name.to_string(),
            path: src,
        }
    }

    #[test]
    fn ensure_gitignored_appends_block() {
        let tmp = scratch("gi");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".gitignore"), "target/\n").unwrap();

        ensure_gitignored(&tmp).unwrap();
        let after = std::fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(after.contains(GITIGNORE_ENTRY), "{after}");
        assert!(after.contains(GITIGNORE_COMMENT), "{after}");
        assert!(after.starts_with("target/\n"), "{after}");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// The global materialize must add spm's skills without disturbing a skill
    /// the user authored by hand in the same shared `~/.copilot/skills` dir, and
    /// must drop a previously-managed skill that is no longer declared.
    #[test]
    fn global_materialize_preserves_user_skills_and_purges_dropped() {
        let tmp = scratch("global-mat");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(&dir).unwrap();

        // A skill the user authored by hand — spm must never touch it.
        let user = dir.join("user-skill");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("SKILL.md"), "user\n").unwrap();

        // A skill spm managed last time that is now dropped from the manifest.
        let dropped = dir.join("old-skill");
        std::fs::create_dir_all(&dropped).unwrap();
        std::fs::write(dropped.join("SKILL.md"), "old\n").unwrap();

        let greet = make_skill(&tmp, "greet");
        // previously_managed still lists `old-skill`; current set is just `greet`.
        copy_and_purge(&dir, &[greet], &["old-skill".to_string()]);

        assert!(dir.join("greet").join("SKILL.md").exists(), "greet copied");
        assert!(!dir.join("old-skill").exists(), "dropped skill purged");
        assert!(
            dir.join("user-skill").join("SKILL.md").exists(),
            "user's own skill preserved"
        );

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Global clean removes only spm-managed skills, leaving the user's intact.
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

    /// Re-materializing the same skill name overwrites its content (idempotent).
    #[test]
    fn copy_skills_into_overwrites_existing_name() {
        let tmp = scratch("overwrite");
        let dir = tmp.join("skills");
        let s = make_skill(&tmp, "greet");
        copy_skills_into(&dir, std::slice::from_ref(&s)).unwrap();
        // Change the source, copy again — the dest must reflect the new content.
        std::fs::write(s.path.join("SKILL.md"), "v2\n").unwrap();
        copy_skills_into(&dir, &[s]).unwrap();
        let got = std::fs::read_to_string(dir.join("greet").join("SKILL.md")).unwrap();
        assert_eq!(got, "v2\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Mirror of the global materialize logic used by the test above, exercising
    /// `remove_managed` + `copy_skills_into` against an explicit dir (no HOME).
    fn copy_and_purge(dir: &Path, skills: &[MaterializedSkill], previously_managed: &[String]) {
        let mut owned: BTreeSet<&str> = previously_managed.iter().map(String::as_str).collect();
        owned.extend(skills.iter().map(|s| s.name.as_str()));
        remove_managed(dir, owned.iter().copied()).unwrap();
        copy_skills_into(dir, skills).unwrap();
    }
}
