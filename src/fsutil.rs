use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve `link` (a symlink) and return its canonical target **iff** that
/// target still resolves inside `boundary` (the *canonical* repo checkout root)
/// and does not reach into a `.git` directory. Returns `None` for a broken
/// symlink, one whose target escapes the boundary, or one that resolves into
/// `.git` — the caller then skips it, preserving both the "never follow a
/// symlink out of the checkout" and the "never materialize `.git`" guarantees.
///
/// The caller supplies an already-canonical `boundary` so only the link target
/// is canonicalized. Hot traversal paths ([`walk_tree`], `skillcheck`,
/// `plugin_skills`) canonicalize the boundary once up front — it comes from
/// [`crate::store::Ensured::root`], which is already canonical — and call this
/// per entry to avoid re-canonicalizing the boundary on every filesystem entry.
///
/// This is the single source of truth for "does this symlink stay inside the
/// checkout (and out of `.git`)?", shared by [`walk_tree`] (behind both the
/// copy and the pre-materialize scan), the skill-loadability check (`skillcheck`),
/// and bundled-skill enumeration (`plugin_skills`), so they can never disagree
/// about which symlinks are followed.
///
/// The `.git` exclusion matters because [`walk_tree`] skips a `.git` entry by
/// *name* at every directory level, but a symlink target like `assets ->
/// ../../.git` would otherwise slip past that filter and copy the whole repo
/// metadata dir under an arbitrary name, so any resolved target with a `.git`
/// path component (relative to the boundary) is rejected as well.
pub(crate) fn resolve_within_canonical(boundary: &Path, link: &Path) -> Option<PathBuf> {
    let target = std::fs::canonicalize(link).ok()?;
    contains(boundary, &target).then_some(target)
}

/// Whether `candidate` (a *canonical* path) lies within `boundary` (also
/// canonical) and does not reach into a `.git` directory. This is the raw
/// containment predicate behind [`resolve_within_canonical`]; callers that have
/// already canonicalized both paths (the copy/scan traversals) use it directly
/// to validate *every* directory they descend into — not just symlink entries —
/// so a symlinked *root* (a bundled skill/plugin dir that is itself a symlink)
/// cannot smuggle its target's contents past the boundary.
pub(crate) fn contains(boundary: &Path, candidate: &Path) -> bool {
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
    walk_tree(src, boundary, &mut |entry| match entry {
        Entry::Dir(rel) => Ok(std::fs::create_dir_all(dst.join(rel))?),
        Entry::File { from, rel } => {
            std::fs::copy(from, dst.join(rel))?;
            Ok(())
        }
        Entry::Skipped { path, why } => {
            warn_skipped(path, why);
            Ok(())
        }
    })
}

/// Check, without writing anything, that everything [`copy_tree`] would copy
/// from `src` can be read: each directory listed and each file opened.
pub fn check_readable(src: &Path, boundary: &Path) -> Result<()> {
    walk_tree(src, boundary, &mut |entry| {
        match entry {
            Entry::File { from, .. } => {
                std::fs::File::open(from)
                    .with_context(|| format!("reading `{}`", from.display()))?;
            }
            Entry::Skipped { path, why } => warn_skipped(path, why),
            Entry::Dir(_) => {}
        }
        Ok(())
    })
}

/// What [`walk_tree`] reaches, with paths relative to the walk's root.
pub enum Entry<'a> {
    /// A directory to materialize (the root itself is `""`).
    Dir(&'a Path),
    /// A regular file to materialize, read from `from` (a symlink's target).
    File { from: &'a Path, rel: &'a Path },
    /// Something deliberately not traversed, at `path` as reached.
    Skipped { path: &'a Path, why: Skip },
}

/// Why [`walk_tree`] skipped a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// A directory resolving outside the boundary or into `.git`.
    Outside,
    /// A directory already on the recursion path (a symlink cycle).
    Cycle,
    /// A symlink that is broken, escapes the boundary or reaches into `.git`.
    Link,
}

/// The warning the materializing walks ([`copy_tree`], [`check_readable`])
/// print for a skipped path.
fn warn_skipped(path: &Path, why: Skip) {
    let path = path.display();
    match why {
        Skip::Outside => {
            eprintln!("warning: skipping directory `{path}` (resolves outside the checkout)")
        }
        Skip::Cycle => {
            eprintln!("warning: skipping already-visited directory `{path}` (symlink cycle)")
        }
        Skip::Link => eprintln!(
            "warning: skipping symlink `{path}` (target is missing or escapes the checkout)"
        ),
    }
}

/// The single traversal behind materialization ([`copy_tree`]) and the
/// pre-materialize scan ([`crate::scan`]), so the scan reads exactly what the
/// copy lands. See [`copy_tree`] for the rules: `.git` skipped, symlinks
/// followed only inside `boundary`, cycles and non-regular files skipped.
///
/// Each directory is canonicalized, checked against the boundary and then
/// listed through that checked canonical path. An IO error (an unresolvable or
/// unlistable directory, a vanished symlink target) fails the walk rather than
/// being skipped, so an unreadable tree can never pass for an empty one.
pub fn walk_tree(
    src: &Path,
    boundary: &Path,
    visit: &mut dyn FnMut(Entry) -> Result<()>,
) -> Result<()> {
    let boundary = std::fs::canonicalize(boundary)
        .with_context(|| format!("resolving checkout boundary `{}`", boundary.display()))?;
    let mut stack: Vec<PathBuf> = Vec::new();
    walk_dir(src, Path::new(""), &boundary, &mut stack, visit)
}

fn walk_dir(
    src: &Path,
    rel: &Path,
    boundary: &Path,
    stack: &mut Vec<PathBuf>,
    visit: &mut dyn FnMut(Entry) -> Result<()>,
) -> Result<()> {
    // Track the canonical path of every directory currently on the recursion
    // stack so a symlink pointing back at an ancestor can't spin forever.
    let src_canon = std::fs::canonicalize(src)
        .with_context(|| format!("resolving directory `{}`", src.display()))?;
    // Harden the *root*: not just symlink entries but the directory we are about
    // to descend into must stay inside the checkout (and out of `.git`). A
    // symlinked root pointing outside the boundary is refused here.
    if !contains(boundary, &src_canon) {
        return visit(Entry::Skipped {
            path: src,
            why: Skip::Outside,
        });
    }
    // Detect an in-boundary symlink cycle *before* visiting the directory, so
    // a skipped cycle entry (e.g. `loop -> .`) does not leave an empty
    // placeholder directory in the materialized output.
    if stack.contains(&src_canon) {
        return visit(Entry::Skipped {
            path: src,
            why: Skip::Cycle,
        });
    }
    visit(Entry::Dir(rel))?;
    let entries = std::fs::read_dir(&src_canon)
        .with_context(|| format!("listing `{}`", src_canon.display()))?;
    stack.push(src_canon);

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        // `file_type()` reflects the entry itself (does not follow symlinks).
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = rel.join(&name);
        if ft.is_symlink() {
            match resolve_within_canonical(boundary, &from) {
                Some(target) => {
                    // `metadata` follows the symlink to classify its target.
                    let meta = std::fs::metadata(&target).with_context(|| {
                        format!("resolving symlink target `{}`", target.display())
                    })?;
                    if meta.is_dir() {
                        walk_dir(&target, &to, boundary, stack, visit)?;
                    } else if meta.is_file() {
                        visit(Entry::File {
                            from: &target,
                            rel: &to,
                        })?;
                    }
                    // Non-regular symlink targets are skipped (see below).
                }
                None => visit(Entry::Skipped {
                    path: &from,
                    why: Skip::Link,
                })?,
            }
            continue;
        }
        if ft.is_dir() {
            walk_dir(&from, &to, boundary, stack, visit)?;
        } else if ft.is_file() {
            visit(Entry::File {
                from: &from,
                rel: &to,
            })?;
        }
        // Non-regular files (FIFOs, sockets, device nodes) are skipped: they
        // carry no skill content, `std::fs::copy` would error and abort the
        // install, and opening one to scan it could block indefinitely.
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
    fn walk_reports_each_skip_with_its_reason() {
        let checkout = scratch("skips");
        let outside = scratch("skips-outside");
        let root = checkout.join("p");
        std::fs::create_dir_all(&root).unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        symlink("nowhere", root.join("broken")).unwrap();
        symlink(".", root.join("loop")).unwrap();

        let skips = |src: &Path| {
            let mut out = Vec::new();
            walk_tree(src, &checkout, &mut |entry| {
                if let Entry::Skipped { path, why } = entry {
                    out.push((path.file_name().unwrap().to_owned(), why));
                }
                Ok(())
            })
            .unwrap();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        };
        let inside = skips(&root);
        let escaped_root = skips(&outside);
        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&outside).ok();

        assert_eq!(
            inside,
            [
                ("broken".into(), Skip::Link),
                ("escape".into(), Skip::Link),
                // A followed link recurses at its target, so a cycle is
                // reported at the directory it re-enters.
                ("p".into(), Skip::Cycle),
            ]
        );
        assert_eq!(
            escaped_root,
            [(outside.file_name().unwrap().to_owned(), Skip::Outside)]
        );
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
        // The skipped cycle entry must not leave an empty placeholder dir.
        assert!(!dst.join("loop").exists());
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

        // resolve_within_canonical itself refuses both.
        let checkout_canon = std::fs::canonicalize(&checkout).unwrap();
        assert!(resolve_within_canonical(&checkout_canon, &plugin.join("sneaky")).is_none());
        assert!(resolve_within_canonical(&checkout_canon, &plugin.join("cfg")).is_none());

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

        // resolve_within_canonical refuses it, and copy_tree (called with the
        // symlinked root) materializes nothing from outside.
        let checkout_canon = std::fs::canonicalize(&checkout).unwrap();
        assert!(resolve_within_canonical(&checkout_canon, &link).is_none());
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
        // intermediate symlink and treat it as in-checkout;
        // resolve_within_canonical canonicalizes the whole path and refuses it,
        // matching copy_tree, which never descends into the escaping dir.
        let checkout = scratch("mid-escape");
        std::fs::create_dir_all(&checkout).unwrap();
        let outside = scratch("mid-escape-outside");
        std::fs::write(outside.join("SKILL.md"), "secret\n").unwrap();
        symlink(&outside, checkout.join("dir")).unwrap();

        let checkout_canon = std::fs::canonicalize(&checkout).unwrap();
        assert!(
            resolve_within_canonical(&checkout_canon, &checkout.join("dir/SKILL.md")).is_none()
        );

        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn skips_non_regular_file_without_aborting() {
        // A FIFO (or socket/device node) in the source tree must be *skipped*,
        // not fed to `std::fs::copy` — which would error and abort the whole
        // install. The rest of the tree must still materialize.
        let checkout = scratch("fifo");
        let plugin = checkout.join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("real.txt"), "real\n").unwrap();
        let fifo = plugin.join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("spawn mkfifo");
        assert!(status.success(), "mkfifo failed");

        let dst_root = scratch("fifo-dst");
        let dst = dst_root.join("out");
        // Must not abort even though `pipe` is a FIFO.
        copy_tree(&plugin, &dst, &checkout).unwrap();

        assert!(dst.join("real.txt").exists());
        // The non-regular file is not materialized.
        assert!(!dst.join("pipe").exists());
        std::fs::remove_dir_all(&checkout).ok();
        std::fs::remove_dir_all(&dst_root).ok();
    }
}

/// Whether the invoking user may create entries in `dir` (write and search
/// permission for its *effective* ids, not just any write bit), asked of the
/// OS without writing anything. `None` where that can't be established without
/// trying, which is every non-Unix platform (Windows ACLs).
#[cfg(unix)]
pub fn can_create_in(dir: &Path) -> std::io::Result<Option<bool>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
    let rc = unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        )
    };
    if rc == 0 {
        return Ok(Some(true));
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EACCES | libc::EROFS | libc::EPERM) => Ok(Some(false)),
        _ => Err(err),
    }
}

#[cfg(not(unix))]
pub fn can_create_in(dir: &Path) -> std::io::Result<Option<bool>> {
    std::fs::metadata(dir).map(|_| None)
}
