//! Shared filesystem primitives for the "copy skill folders into a directory
//! the tool discovers" family of vendor adapters ([`super::shareddir`]'s
//! presets, and any future SKILL.md tool that auto-discovers
//! `<dir>/<name>/SKILL.md`).
//!
//! Both the project- and user-global skill dirs of these tools are laid out the
//! same way — one directory per skill, holding a `SKILL.md` — so the copy and
//! surgical-remove logic is identical across them. Keeping the single canonical
//! implementation here avoids duplicating it per adapter (see the "Derivation
//! Over Duplication" rule in `AGENTS.md`).

use crate::fsutil;
use crate::ids::DepName;
use anyhow::{Context, Result};
use std::path::Path;

use super::{Claim, MaterializedSkill};

/// Copy each skill into `dir/<name>/`, creating `dir` if needed.
///
/// Replaces any pre-existing dir of the same name so re-materialization is
/// idempotent even when `dir` is shared with the user's own hand-authored
/// skills. Each name is `claim`ed right before its entry is written.
pub fn copy_skills_into(dir: &Path, skills: &[MaterializedSkill], claim: Claim) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for s in skills {
        claim(&s.name)?;
        let dest = dir.join(&s.name);
        fsutil::remove_path_if_present(&dest)
            .with_context(|| format!("clearing {}", dest.display()))?;
        fsutil::copy_tree(&s.path, &dest, &s.root)
            .with_context(|| format!("copying skill `{}` into {}", s.name, dir.display()))?;
    }
    Ok(())
}

/// Remove `dir/<name>` for each managed `name` (a symlink there is unlinked,
/// never followed), ignoring absent entries and leaving every other entry in
/// `dir` (e.g. the user's own skills) untouched.
/// Each present entry is `claim`ed right before it is removed.
pub fn remove_managed<'a>(
    dir: &Path,
    names: impl Iterator<Item = &'a DepName>,
    claim: Claim,
) -> Result<()> {
    for name in names {
        let target = dir.join(name);
        // Judged without following a symlink, like the removal itself; a
        // regular file is the user's, not a managed entry, and is left alone.
        if std::fs::symlink_metadata(&target).is_ok_and(|m| !m.is_file()) {
            claim(name)?;
            fsutil::remove_managed_path(&target)
                .with_context(|| format!("removing {}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::dep;
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
            name: DepName::try_from(name).unwrap(),
            path: src,
            root: root.to_path_buf(),
        }
    }

    /// Re-materializing the same skill name overwrites its content (idempotent).
    #[test]
    fn copy_skills_into_overwrites_existing_name() {
        let tmp = scratch("overwrite");
        let dir = tmp.join("skills");
        let s = make_skill(&tmp, "greet");
        copy_skills_into(&dir, std::slice::from_ref(&s), &mut |_| Ok(())).unwrap();
        std::fs::write(s.path.join("SKILL.md"), "v2\n").unwrap();
        copy_skills_into(&dir, &[s], &mut |_| Ok(())).unwrap();
        let got = std::fs::read_to_string(dir.join("greet").join("SKILL.md")).unwrap();
        assert_eq!(got, "v2\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A managed entry that is a symlink is removed (or replaced) as a link:
    /// its target outside the shared dir is never touched.
    #[cfg(unix)]
    #[test]
    fn symlinked_managed_entries_are_unlinked_not_followed() {
        let tmp = scratch("symlinked");
        let dir = tmp.join("skills");
        let target = tmp.join("elsewhere");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), "keep\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("old")).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("greet")).unwrap();

        remove_managed(&dir, [&dep("old")].into_iter(), &mut |_| Ok(())).unwrap();
        copy_skills_into(&dir, &[make_skill(&tmp, "greet")], &mut |_| Ok(())).unwrap();

        assert!(
            std::fs::symlink_metadata(dir.join("old")).is_err(),
            "link removed"
        );
        let greet = std::fs::symlink_metadata(dir.join("greet")).unwrap();
        assert!(greet.is_dir(), "link replaced by a real copy");
        assert_eq!(
            std::fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "keep\n"
        );
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
        remove_managed(&dir, [&dep("greet")].into_iter(), &mut |_| Ok(())).unwrap();
        assert!(!dir.join("greet").exists(), "managed skill removed");
        assert!(dir.join("user-skill").exists(), "unnamed skill preserved");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
