use anyhow::Result;
use std::path::Path;

/// Recursively copy `src` dir into `dst`, skipping the `.git` directory.
///
/// Symlinks are skipped rather than followed: a hostile skill repo could
/// otherwise ship `SKILL.md -> ../../../.ssh/id_rsa` and have its target's
/// contents copied into the vendor plugin dir where an agent might read it.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        // `file_type()` reflects the entry itself (does not follow symlinks).
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            eprintln!(
                "warning: skipping symlink `{}` while copying skill",
                entry.path().display()
            );
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
