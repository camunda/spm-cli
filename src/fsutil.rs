use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve `link` (a symlink) and return its canonical target **iff** that
/// target still resolves inside `boundary` (the repo checkout root). Returns
/// `None` for a broken symlink or one whose target escapes the boundary — the
/// caller then skips it, preserving the "never follow a symlink out of the
/// checkout" security guarantee.
///
/// `boundary` is canonicalized here so the containment check compares like for
/// like on every platform (Windows verbatim `\\?\` prefixes included). This is
/// the single source of truth for "does this symlink stay inside the checkout?",
/// shared by the copy (`copy_tree`), the pre-materialize scan (`scan`), and the
/// skill-loadability check (`skillcheck`), so they can never disagree about
/// which symlinks are followed.
pub fn resolve_within(boundary: &Path, link: &Path) -> Option<PathBuf> {
    let boundary = std::fs::canonicalize(boundary).ok()?;
    let target = std::fs::canonicalize(link).ok()?;
    (target == boundary || target.starts_with(&boundary)).then_some(target)
}

/// Recursively copy `src` dir into `dst`, skipping the `.git` directory.
///
/// A symlink is followed **only** when its target resolves inside `boundary`
/// (the repo checkout root): a plugin may legitimately symlink shared assets
/// that live elsewhere in its own checkout (e.g. `plugins/x/agents ->
/// ../../agents`), and those must be materialized rather than silently dropped.
/// A symlink whose target escapes the checkout — the classic `SKILL.md ->
/// ../../../.ssh/id_rsa` exfiltration attempt — is skipped, so the security
/// boundary is preserved: content outside the checkout is never copied into the
/// vendor dir where an agent might read it.
///
/// Symlink cycles that stay inside the boundary (e.g. `loop -> .`) are detected
/// via the set of canonical directories on the active recursion path and
/// skipped rather than followed into an infinite loop.
///
/// Windows caveat: creating symlinks there needs privilege, but *following* one
/// and canonicalizing its target work the same way, so a checkout that already
/// contains in-repo symlinks materializes identically. The containment check
/// compares canonical paths, which is cross-platform safe.
pub fn copy_tree(src: &Path, dst: &Path, boundary: &Path) -> Result<()> {
    let mut stack: Vec<PathBuf> = Vec::new();
    copy_dir(src, dst, boundary, &mut stack)
}

fn copy_dir(src: &Path, dst: &Path, boundary: &Path, stack: &mut Vec<PathBuf>) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    // Track the canonical path of every directory currently on the recursion
    // stack so a symlink pointing back at an ancestor can't spin forever.
    let src_canon = std::fs::canonicalize(src)
        .with_context(|| format!("resolving copy source `{}`", src.display()))?;
    if stack.contains(&src_canon) {
        eprintln!(
            "warning: skipping already-visited directory `{}` (symlink cycle)",
            src.display()
        );
        return Ok(());
    }
    stack.push(src_canon);

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        // `file_type()` reflects the entry itself (does not follow symlinks).
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(&name);
        if ft.is_symlink() {
            match resolve_within(boundary, &from) {
                Some(target) => {
                    // `metadata` follows the symlink to classify its target.
                    let meta = std::fs::metadata(&target).with_context(|| {
                        format!("resolving symlink target `{}`", target.display())
                    })?;
                    if meta.is_dir() {
                        copy_dir(&target, &to, boundary, stack)?;
                    } else if meta.is_file() {
                        std::fs::copy(&target, &to)?;
                    }
                }
                None => {
                    eprintln!(
                        "warning: skipping symlink `{}` (target is missing or escapes the checkout)",
                        from.display()
                    );
                }
            }
            continue;
        }
        if ft.is_dir() {
            copy_dir(&from, &to, boundary, stack)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }

    stack.pop();
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("spm-fsutil-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn follows_symlink_resolving_inside_boundary() {
        // Layout mirrors camunda/design-system: a plugin subdir whose `agents`
        // is a symlink to a repo-root dir that lives *outside* the subdir but
        // *inside* the checkout.
        let checkout = scratch("in-root");
        std::fs::create_dir_all(checkout.join("agents")).unwrap();
        std::fs::write(checkout.join("agents/dev.md"), "dev\n").unwrap();
        let plugin = checkout.join("plugins/p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("real.txt"), "real\n").unwrap();
        symlink("../../agents", plugin.join("agents")).unwrap();

        let dst_root = scratch("in-root-dst");
        let dst = dst_root.join("out");
        copy_tree(&plugin, &dst, &checkout).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("real.txt")).unwrap(),
            "real\n"
        );
        // The symlinked-in directory and its contents are materialized.
        assert_eq!(
            std::fs::read_to_string(dst.join("agents/dev.md")).unwrap(),
            "dev\n"
        );
        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&dst_root).ok();
    }

    #[test]
    fn skips_symlink_escaping_boundary() {
        let checkout = scratch("escape");
        let plugin = checkout.join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("real.txt"), "real\n").unwrap();
        // A secret living outside the checkout — the exfiltration target.
        let outside = scratch("escape-outside");
        std::fs::write(outside.join("secret.txt"), "TOP SECRET\n").unwrap();
        symlink(outside.join("secret.txt"), plugin.join("secret.txt")).unwrap();

        let dst_root = scratch("escape-dst");
        let dst = dst_root.join("out");
        copy_tree(&plugin, &dst, &checkout).unwrap();

        assert!(dst.join("real.txt").exists());
        // The escaping symlink is never followed.
        assert!(!dst.join("secret.txt").exists());
        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&outside).ok();
        std::fs::remove_dir_all(&dst_root).ok();
    }

    #[test]
    fn breaks_symlink_cycle_inside_boundary() {
        let checkout = scratch("cycle");
        let plugin = checkout.join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("real.txt"), "real\n").unwrap();
        // `loop -> .` resolves to the plugin dir itself: a cycle that stays
        // inside the boundary. It must not send the copy into an infinite loop.
        symlink(".", plugin.join("loop")).unwrap();

        let dst_root = scratch("cycle-dst");
        let dst = dst_root.join("out");
        copy_tree(&plugin, &dst, &checkout).unwrap();

        assert!(dst.join("real.txt").exists());
        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&dst_root).ok();
    }
}
