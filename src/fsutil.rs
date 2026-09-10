use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve `link` (a symlink) and return its canonical target **iff** that
/// target still resolves inside `boundary` (the repo checkout root) and does
/// not reach into a `.git` directory. Returns `None` for a broken symlink, one
/// whose target escapes the boundary, or one that resolves into `.git` — the
/// caller then skips it, preserving both the "never follow a symlink out of the
/// checkout" and the "never materialize `.git`" guarantees.
///
/// `boundary` is canonicalized here so the containment check compares like for
/// like on every platform (Windows verbatim `\\?\` prefixes included). This is
/// the single source of truth for "does this symlink stay inside the checkout
/// (and out of `.git`)?", shared by the copy (`copy_tree`), the pre-materialize
/// scan (`scan`), and the skill-loadability check (`skillcheck`), so they can
/// never disagree about which symlinks are followed.
///
/// The `.git` exclusion matters because `copy_tree`/`scan` skip a `.git` entry
/// by *name* at every directory level, but a symlink target like `assets ->
/// ../../.git` would otherwise slip past that filter and copy the whole repo
/// metadata dir under an arbitrary name. Rejecting any resolved target with a
/// `.git` path component (relative to the boundary) keeps the two filters in
/// lockstep.
pub fn resolve_within(boundary: &Path, link: &Path) -> Option<PathBuf> {
    let boundary = std::fs::canonicalize(boundary).ok()?;
    let target = std::fs::canonicalize(link).ok()?;
    contains(&boundary, &target).then_some(target)
}

/// Whether `candidate` (a *canonical* path) lies within `boundary` (also
/// canonical) and does not reach into a `.git` directory. This is the raw
/// containment predicate behind [`resolve_within`]; callers that have already
/// canonicalized both paths (the copy/scan traversals) use it directly to
/// validate *every* directory they descend into — not just symlink entries — so
/// a symlinked *root* (a bundled skill/plugin dir that is itself a symlink)
/// cannot smuggle its target's contents past the boundary.
fn contains(boundary: &Path, candidate: &Path) -> bool {
    match candidate.strip_prefix(boundary) {
        // `strip_prefix` succeeds only when `candidate` is `boundary` itself
        // (empty relative path) or lies within it.
        Ok(rel) => !rel.components().any(|c| c.as_os_str() == ".git"),
        Err(_) => false,
    }
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
/// vendor dir where an agent might read it. The same guard applies to the copy
/// *root* itself: if `src` (or any directory reached while recursing) resolves
/// outside `boundary`, it is skipped rather than traversed — so a bundled
/// skill/plugin directory that is itself a symlink cannot bypass the boundary.
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
    let boundary = std::fs::canonicalize(boundary)
        .with_context(|| format!("resolving checkout boundary `{}`", boundary.display()))?;
    let mut stack: Vec<PathBuf> = Vec::new();
    copy_dir(src, dst, &boundary, &mut stack)
}

fn copy_dir(src: &Path, dst: &Path, boundary: &Path, stack: &mut Vec<PathBuf>) -> Result<()> {
    // Track the canonical path of every directory currently on the recursion
    // stack so a symlink pointing back at an ancestor can't spin forever.
    let src_canon = std::fs::canonicalize(src)
        .with_context(|| format!("resolving copy source `{}`", src.display()))?;
    // Harden the *root*: not just symlink entries but the directory we are about
    // to descend into must stay inside the checkout (and out of `.git`). A
    // symlinked copy root pointing outside the boundary is refused here.
    if !contains(boundary, &src_canon) {
        eprintln!(
            "warning: skipping directory `{}` (resolves outside the checkout)",
            src.display()
        );
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
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

    #[test]
    fn skips_symlink_resolving_into_dotgit() {
        // A symlink pointing at the checkout's `.git` resolves *inside* the
        // boundary but must still be refused: copy_tree/scan skip `.git` by
        // name, and following it via a symlink would copy the whole repo
        // metadata dir under an arbitrary name.
        let checkout = scratch("dotgit");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        std::fs::write(checkout.join(".git/config"), "[core]\n").unwrap();
        let plugin = checkout.join("plugins/p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("real.txt"), "real\n").unwrap();
        symlink("../../.git", plugin.join("sneaky")).unwrap();
        // Also a symlink to a file *inside* `.git`.
        symlink("../../.git/config", plugin.join("cfg")).unwrap();

        // resolve_within itself refuses both.
        assert!(resolve_within(&checkout, &plugin.join("sneaky")).is_none());
        assert!(resolve_within(&checkout, &plugin.join("cfg")).is_none());

        let dst_root = scratch("dotgit-dst");
        let dst = dst_root.join("out");
        copy_tree(&plugin, &dst, &checkout).unwrap();

        assert!(dst.join("real.txt").exists());
        // Neither the `.git` dir nor any file inside it is materialized.
        assert!(!dst.join("sneaky").exists());
        assert!(!dst.join("cfg").exists());
        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&dst_root).ok();
    }

    #[test]
    fn skips_symlinked_copy_root_escaping_boundary() {
        // The *copy root itself* is a symlink whose target lies outside the
        // checkout. copy_tree must refuse to traverse it — otherwise a bundled
        // skill/plugin dir that is a symlink could exfiltrate arbitrary content.
        let checkout = scratch("root-escape");
        std::fs::create_dir_all(&checkout).unwrap();
        let outside = scratch("root-escape-outside");
        std::fs::write(outside.join("secret.txt"), "TOP SECRET\n").unwrap();
        // `link` lives inside the checkout but points at the outside dir.
        let link = checkout.join("link");
        symlink(&outside, &link).unwrap();

        // resolve_within refuses it, and copy_tree (called with the symlinked
        // root) materializes nothing from outside.
        assert!(resolve_within(&checkout, &link).is_none());
        let dst_root = scratch("root-escape-dst");
        let dst = dst_root.join("out");
        copy_tree(&link, &dst, &checkout).unwrap();
        assert!(!dst.join("secret.txt").exists());

        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&outside).ok();
        std::fs::remove_dir_all(&dst_root).ok();
    }

    #[test]
    fn skips_file_reached_via_escaping_intermediate_dir_symlink() {
        // `dir -> outside` (escaping), then `dir/SKILL.md` is a real file in the
        // outside dir. A shallow check on the final component would follow the
        // intermediate symlink and treat it as in-checkout; resolve_within
        // canonicalizes the whole path and refuses it, matching copy_tree, which
        // never descends into the escaping dir.
        let checkout = scratch("mid-escape");
        std::fs::create_dir_all(&checkout).unwrap();
        let outside = scratch("mid-escape-outside");
        std::fs::write(outside.join("SKILL.md"), "secret\n").unwrap();
        symlink(&outside, checkout.join("dir")).unwrap();

        assert!(resolve_within(&checkout, &checkout.join("dir/SKILL.md")).is_none());

        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
