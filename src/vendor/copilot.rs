use super::{MaterializedSkill, Vendor};
use crate::fsutil;
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
            if !dest.join("SKILL.md").exists() {
                eprintln!(
                    "warning: skill `{}` has no SKILL.md at its root — Copilot may ignore it",
                    s.name
                );
            }
        }

        ensure_gitignored(project_root)
    }

    fn clean(&self, project_root: &Path, _project_id: &str) -> Result<()> {
        let dir = managed_dir(project_root);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        remove_gitignore_entry(project_root)
    }
}

/// Absolute path of the project-local managed skills directory.
fn managed_dir(project_root: &Path) -> PathBuf {
    MANAGED_DIR
        .iter()
        .fold(project_root.to_path_buf(), |p, seg| p.join(seg))
}

/// Ensure the project's `.gitignore` ignores the managed skills dir, adding the
/// comment + entry once. Idempotent: an existing entry (however authored) is
/// left untouched.
fn ensure_gitignored(project_root: &Path) -> Result<()> {
    let path = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == GITIGNORE_ENTRY) {
        return Ok(());
    }

    let mut out = existing;
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n'); // blank line separating our block from prior content
    }
    out.push_str(GITIGNORE_COMMENT);
    out.push('\n');
    out.push_str(GITIGNORE_ENTRY);
    out.push('\n');

    std::fs::write(&path, out).with_context(|| format!("updating {}", path.display()))
}

/// Remove the comment + entry spm added to `.gitignore` (used by `clean`).
/// Lines the user added independently are preserved.
fn remove_gitignore_entry(project_root: &Path) -> Result<()> {
    let path = project_root.join(".gitignore");
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };

    let mut kept: Vec<&str> = existing
        .lines()
        .filter(|l| {
            let t = l.trim();
            t != GITIGNORE_ENTRY && t != GITIGNORE_COMMENT
        })
        .collect();
    // Trim trailing blank lines left behind by the removal.
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }

    let out = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    std::fs::write(&path, out).with_context(|| format!("updating {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_and_remove_gitignore_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("spm-gi-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let gi = tmp.join(".gitignore");
        std::fs::write(&gi, "target/\n").unwrap();

        ensure_gitignored(&tmp).unwrap();
        let after = std::fs::read_to_string(&gi).unwrap();
        assert!(after.contains(GITIGNORE_ENTRY), "{after}");
        assert!(after.contains(GITIGNORE_COMMENT), "{after}");
        assert!(
            after.starts_with("target/\n"),
            "preserves prior content: {after}"
        );

        // Idempotent: a second call must not duplicate the entry.
        ensure_gitignored(&tmp).unwrap();
        let twice = std::fs::read_to_string(&gi).unwrap();
        assert_eq!(twice.matches(GITIGNORE_ENTRY).count(), 1, "{twice}");

        remove_gitignore_entry(&tmp).unwrap();
        let cleaned = std::fs::read_to_string(&gi).unwrap();
        assert!(!cleaned.contains(GITIGNORE_ENTRY), "{cleaned}");
        assert!(!cleaned.contains(GITIGNORE_COMMENT), "{cleaned}");
        assert_eq!(cleaned, "target/\n");

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
