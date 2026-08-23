//! Shared filesystem primitives for the "copy skill folders into a directory
//! the tool discovers" family of vendor adapters (Copilot, Gemini, and any
//! future SKILL.md tool that auto-discovers `<dir>/<name>/SKILL.md`).
//!
//! Both the project- and user-global skill dirs of these tools are laid out the
//! same way — one directory per skill, holding a `SKILL.md` — so the copy and
//! surgical-remove logic is identical across them. Keeping the single canonical
//! implementation here avoids duplicating it per adapter (see the "Derivation
//! Over Duplication" rule in `AGENTS.md`).

use crate::fsutil;
use anyhow::{Context, Result};
use std::path::Path;

use super::MaterializedSkill;

/// Copy each skill into `dir/<name>/`, creating `dir` if needed.
///
/// Replaces any pre-existing dir of the same name so re-materialization is
/// idempotent even when `dir` is shared with the user's own hand-authored
/// skills.
pub fn copy_skills_into(dir: &Path, skills: &[MaterializedSkill]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for s in skills {
        let dest = dir.join(&s.name);
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
pub fn remove_managed<'a>(dir: &Path, names: impl Iterator<Item = &'a str>) -> Result<()> {
    for name in names {
        let target = dir.join(name);
        if target.is_dir() {
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("removing {}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "spm-dirskills-{name}-{}-{nanos}",
            std::process::id()
        ))
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

    /// Re-materializing the same skill name overwrites its content (idempotent).
    #[test]
    fn copy_skills_into_overwrites_existing_name() {
        let tmp = scratch("overwrite");
        let dir = tmp.join("skills");
        let s = make_skill(&tmp, "greet");
        copy_skills_into(&dir, std::slice::from_ref(&s)).unwrap();
        std::fs::write(s.path.join("SKILL.md"), "v2\n").unwrap();
        copy_skills_into(&dir, &[s]).unwrap();
        let got = std::fs::read_to_string(dir.join("greet").join("SKILL.md")).unwrap();
        assert_eq!(got, "v2\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// `remove_managed` removes only the named skills, leaving others intact.
    #[test]
    fn remove_managed_leaves_unnamed_entries() {
        let tmp = scratch("remove");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["greet", "user-skill"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
            std::fs::write(dir.join(n).join("SKILL.md"), "x\n").unwrap();
        }
        remove_managed(&dir, ["greet"].into_iter()).unwrap();
        assert!(!dir.join("greet").exists(), "managed skill removed");
        assert!(dir.join("user-skill").exists(), "unnamed skill preserved");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
