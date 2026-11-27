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

use crate::ids::DepName;
use crate::{atomic, fsutil};
use anyhow::{Context, Result};
use std::path::Path;

use super::Claim;
use crate::materialized::MaterializedSkill;

/// Copy each skill into `dir/<name>/`, creating `dir` if needed.
///
/// Replaces any pre-existing dir of the same name so re-materialization is
/// idempotent even when `dir` is shared with the user's own hand-authored
/// skills. Each skill is published whole (see [`atomic::publish_dir`]): one
/// that fails to copy keeps its previous version. Each name is `claim`ed as
/// its entry is replaced (see [`copy_skills`]).
pub fn copy_skills_into(dir: &Path, skills: &[MaterializedSkill], claim: Claim) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    copy_skills(dir, dir, skills, claim)
}

/// Replace `dir` with one holding exactly `skills`, published whole (see
/// [`atomic::publish_dir`]): a failure keeps the previous `dir` intact. The
/// names are `claim`ed once `dir` is replaced (see [`copy_skills`]).
pub fn rebuild_with_skills(dir: &Path, skills: &[MaterializedSkill], claim: Claim) -> Result<()> {
    atomic::publish_dir_and(
        dir,
        |stage| copy_skills(stage, dir, skills, &mut |_| Ok(())),
        || skills.iter().try_for_each(|s| claim(s.name())),
    )
}

/// Publish each skill as `into/<name>/`, naming `dir` (where `into` ends up)
/// in errors. A name is `claim`ed once its entry is in place, before the
/// previous one is dropped (see [`atomic::publish_dir_and`]): one that fails
/// to publish leaves the previous entry (perhaps the user's own skill)
/// untouched, so claiming it would let a later cleanup delete it, and one
/// whose claim fails is put back the same way. Killed in between, spm leaves
/// its skill unclaimed, which is left alone rather than deleted.
fn copy_skills(into: &Path, dir: &Path, skills: &[MaterializedSkill], claim: Claim) -> Result<()> {
    for s in skills {
        atomic::publish_dir_and(
            &into.join(s.name()),
            |stage| {
                fsutil::copy_tree(s.path(), stage, s.root())
                    .with_context(|| format!("copying skill `{}` into {}", s.name(), dir.display()))
            },
            || claim(s.name()),
        )?;
    }
    Ok(())
}

/// Remove `dir/<name>` for each managed `name` (a symlink there is unlinked,
/// never followed), ignoring absent entries and leaving every other entry in
/// `dir` (e.g. the user's own skills) untouched.
/// Each [managed entry](fsutil::is_managed_entry) is `claim`ed right before it
/// is removed; anything else at a managed name is neither claimed nor removed.
pub fn remove_managed<'a>(
    dir: &Path,
    names: impl Iterator<Item = &'a DepName>,
    claim: Claim,
) -> Result<()> {
    for name in names {
        let target = dir.join(name);
        if fsutil::is_managed_entry(&target) {
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
        MaterializedSkill::unscanned(DepName::try_from(name).unwrap(), src, root.to_path_buf())
    }

    /// Re-materializing the same skill name overwrites its content (idempotent).
    #[test]
    fn copy_skills_into_overwrites_existing_name() {
        let tmp = scratch("overwrite");
        let dir = tmp.join("skills");
        let s = make_skill(&tmp, "greet");
        copy_skills_into(&dir, std::slice::from_ref(&s), &mut |_| Ok(())).unwrap();
        std::fs::write(s.path().join("SKILL.md"), "v2\n").unwrap();
        copy_skills_into(&dir, &[s], &mut |_| Ok(())).unwrap();
        let got = std::fs::read_to_string(dir.join("greet").join("SKILL.md")).unwrap();
        assert_eq!(got, "v2\n");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A skill failing to copy over the user's own skill of that name leaves
    /// it untouched, so it is not claimed either: a later cleanup must not
    /// delete what spm never replaced.
    #[test]
    fn a_failed_copy_never_claims_the_entry_it_left() {
        let tmp = scratch("unclaimed");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(dir.join("greet")).unwrap();
        std::fs::write(dir.join("greet/SKILL.md"), "mine\n").unwrap();
        let s = make_skill(&tmp, "greet");
        std::fs::remove_dir_all(s.path()).unwrap();
        let mut claimed = Vec::new();
        copy_skills_into(&dir, &[s], &mut |n| {
            claimed.push(n.clone());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(claimed, Vec::<DepName>::new());
        assert_eq!(
            std::fs::read_to_string(dir.join("greet/SKILL.md")).unwrap(),
            "mine\n"
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A rebuild failing part-way keeps the previous dir, and claims none of
    /// the skills it staged before failing: none of them reached `dir`.
    #[test]
    fn a_failed_rebuild_claims_nothing() {
        let tmp = scratch("rebuild-unclaimed");
        let dir = tmp.join("skills");
        let skills = [make_skill(&tmp, "a"), make_skill(&tmp, "b")];
        std::fs::remove_dir_all(skills[1].path()).unwrap();
        let mut claimed = Vec::new();
        rebuild_with_skills(&dir, &skills, &mut |n| {
            claimed.push(n.clone());
            Ok(())
        })
        .unwrap_err();
        assert_eq!(claimed, Vec::<DepName>::new());
        assert!(!dir.exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A skill failing to copy keeps its previous version; skills published
    /// before it keep their new one, and nothing staged is left behind.
    #[test]
    fn a_failed_copy_keeps_the_previous_skill() {
        let tmp = scratch("failed");
        let dir = tmp.join("skills");
        let skills = [make_skill(&tmp, "a"), make_skill(&tmp, "b")];
        copy_skills_into(&dir, &skills, &mut |_| Ok(())).unwrap();
        std::fs::write(skills[0].path().join("SKILL.md"), "v2\n").unwrap();
        std::fs::remove_dir_all(skills[1].path()).unwrap();

        let err = copy_skills_into(&dir, &skills, &mut |_| Ok(())).unwrap_err();
        assert!(
            format!("{err:#}").starts_with("copying skill `b`"),
            "{err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a/SKILL.md")).unwrap(),
            "v2\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b/SKILL.md")).unwrap(),
            "---\nname: b\n---\n"
        );
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a", "b"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// A skill whose claim can't be recorded is not left in place unclaimed:
    /// the entry it replaced (here the user's own) is put back.
    #[test]
    fn a_failed_claim_keeps_the_previous_skill() {
        let tmp = scratch("claim");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/SKILL.md"), "mine\n").unwrap();
        let skills = [make_skill(&tmp, "a")];
        let err = copy_skills_into(&dir, &skills, &mut |_| anyhow::bail!("record not saved"))
            .unwrap_err();
        assert_eq!(format!("{err:#}"), "record not saved");
        assert_eq!(
            std::fs::read_to_string(dir.join("a/SKILL.md")).unwrap(),
            "mine\n"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a"]);
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

    /// A special file (here a FIFO) at a managed name is not something spm
    /// wrote: cleanup neither claims nor removes it, and does not fail on it.
    #[cfg(unix)]
    #[test]
    fn special_files_at_managed_names_are_neither_claimed_nor_removed() {
        let tmp = scratch("special");
        let dir = tmp.join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("greet");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());

        let mut claimed = Vec::new();
        remove_managed(&dir, [&dep("greet")].into_iter(), &mut |n| {
            claimed.push(n.to_string());
            Ok(())
        })
        .unwrap();
        assert!(claimed.is_empty(), "not claimed: {claimed:?}");
        assert!(std::fs::symlink_metadata(&fifo).is_ok(), "left in place");
        crate::vendor::remove_paths(std::slice::from_ref(&fifo)).unwrap();
        assert!(std::fs::symlink_metadata(&fifo).is_ok(), "left in place");
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
