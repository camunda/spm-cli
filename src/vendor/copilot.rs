use super::{MaterializedSkill, Vendor};
use crate::{fsutil, gitignore};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// GitHub Copilot CLI adapter.
///
/// Copilot CLI auto-discovers skills from `.agents/skills/**/SKILL.md` relative
/// to the directory it runs in. To make spm-managed skills **truly project
/// local** — available to Copilot when it runs in the repo, but never committed
/// — spm copies each resolved skill into
/// `.agents/skills/spm-managed-skills/<name>/` inside the project and adds that
/// path to the project's `.gitignore` (with an explanatory comment).
///
/// This deliberately replaces the previous user-global approach (shelling out to
/// `copilot plugin marketplace add`/`install`): there is no global state to
/// register, prune, or leave orphaned, and nothing spm generates is committed.
pub struct Copilot;

/// Directory segments (relative to the project root) that spm materializes
/// skills into. Kept in sync with the `.gitignore` entry below.
const MANAGED_DIR: [&str; 3] = [".agents", "skills", "spm-managed-skills"];

/// The `.gitignore` line (forward-slashed, trailing slash) that ignores the
/// materialized skills, plus the comment written just above it.
const GITIGNORE_ENTRY: &str = ".agents/skills/spm-managed-skills/";
const GITIGNORE_COMMENT: &str =
    "# spm-managed Copilot skills — materialized locally by `spm`, not committed.";

impl Vendor for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn materialize(
        &self,
        project_root: &Path,
        _project_id: &str,
        skills: &[MaterializedSkill],
    ) -> Result<()> {
        let dir = managed_dir(project_root);

        // Rebuild from scratch so removed skills disappear.
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clearing {}", dir.display()))?;
        }

        if skills.is_empty() {
            return Ok(());
        }

        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        for s in skills {
            let dest = dir.join(&s.name);
            fsutil::copy_tree(&s.path, &dest)
                .with_context(|| format!("copying skill `{}` into {}", s.name, dir.display()))?;
        }

        ensure_gitignored(project_root)
    }

    fn clean(&self, project_root: &Path, _project_id: &str) -> Result<()> {
        let dir = managed_dir(project_root);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        // `.gitignore` is intentionally left untouched: the entry spm added is
        // harmless once the materialized dir is gone, and rewriting a
        // user-owned file on `clean` risks clobbering their content.
        Ok(())
    }

    fn status(&self, project_root: &Path, expected: &[String]) -> Result<super::VendorStatus> {
        Ok(super::classify(&managed_dir(project_root), expected))
    }
}

/// Absolute path of the project-local managed skills directory.
fn managed_dir(project_root: &Path) -> PathBuf {
    MANAGED_DIR
        .iter()
        .fold(project_root.to_path_buf(), |p, seg| p.join(seg))
}

/// Ensure the project's `.gitignore` ignores the managed skills dir.
fn ensure_gitignored(project_root: &Path) -> Result<()> {
    gitignore::ensure(project_root, GITIGNORE_COMMENT, GITIGNORE_ENTRY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_gitignored_appends_block() {
        let tmp = std::env::temp_dir().join(format!("spm-gi-copilot-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let gi = tmp.join(".gitignore");
        std::fs::write(&gi, "target/\n").unwrap();

        ensure_gitignored(&tmp).unwrap();
        let after = std::fs::read_to_string(&gi).unwrap();
        assert!(after.contains(GITIGNORE_ENTRY), "{after}");
        assert!(after.contains(GITIGNORE_COMMENT), "{after}");
        assert!(after.starts_with("target/\n"), "{after}");

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
