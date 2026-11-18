//! Publishing files and directories all at once: the one implementation
//! behind every file and directory spm writes outside the store. A new
//! version is staged beside its destination and renamed into place, so a
//! failed or interrupted write leaves the previous one intact, and a reader
//! never sees a truncated file or a half-copied directory: the old one or
//! the new one (or, for a directory where the OS can't swap two entries,
//! briefly neither; see [`publish_dir`]).

use anyhow::Context;
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many symlinks [`resolve`] follows before giving up (Linux's limit).
const MAX_LINKS: usize = 40;

/// `path` with every symlink at its final component followed, even a dangling
/// one, so replacing the result writes through the link (as an in-place write
/// would) instead of replacing the link itself. A missing `path` is itself.
pub fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let mut p = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&p)?;
                p = match p.parent() {
                    Some(dir) => dir.join(target),
                    None => target,
                };
            }
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(p),
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::InvalidInput,
        format!("{}: too many levels of symbolic links", path.display()),
    ))
}

/// Replace the file at `path` (written through a symlink, see [`resolve`])
/// with what `write` writes, all at once (see [`Staged`]).
pub fn replace(
    path: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    Staged::write(&resolve(path)?, write)?.publish()
}

/// A name no other staging entry has: `<pid>-<nanos>-<n>.<ext>`.
fn unique(ext: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{n}.{ext}", std::process::id())
}

/// Whether `name` is one [`unique`] makes for `ext`.
fn is_unique(name: &str, ext: &str) -> bool {
    let Some(stamp) = name.strip_suffix(ext).and_then(|n| n.strip_suffix('.')) else {
        return false;
    };
    let parts: Vec<_> = stamp.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A hidden, unique sibling of `dest` (`.<name>.<pid>-<nanos>-<n>.<ext>`):
/// on the same filesystem, so renaming it over `dest` is atomic.
fn sibling(dest: &Path, ext: &str) -> std::io::Result<PathBuf> {
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{} is not a file path", dest.display()),
        ));
    };
    Ok(dir.join(format!(".{}.{}", name.to_string_lossy(), unique(ext))))
}

/// A complete replacement for a file, staged beside it as a hidden `.tmp`
/// file (which readers listing the directory for `.json` files skip) until
/// [`Staged::publish`] renames it over the destination. Dropped unpublished,
/// it is removed.
#[must_use = "a staged file is removed unless published"]
pub struct Staged {
    tmp: PathBuf,
    dest: PathBuf,
    published: bool,
}

impl Staged {
    /// Stage what `write` writes as the next content of `dest`, flushed to
    /// disk, with the permissions of the file it replaces. `dest`'s directory
    /// must exist. A symlink at `dest` is replaced, not followed: pass a
    /// [`resolve`]d path to write through it.
    pub fn write(
        dest: &Path,
        write: impl FnOnce(&mut File) -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        let tmp = sibling(dest, "tmp")?;
        let perms = std::fs::metadata(dest)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.permissions());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Created no more open than the file it replaces (the umask only
        // narrows it further), so no reader can open it while it is written.
        #[cfg(unix)]
        if let Some(perms) = &perms {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(perms.mode() & 0o777);
        }
        let mut file = options.open(&tmp)?;
        let staged = Self {
            tmp,
            dest: dest.to_path_buf(),
            published: false,
        };
        write(&mut file)?;
        // Windows refuses to delete a read-only file, so an unpublished one
        // could not be cleaned up; replacing a read-only file fails there
        // anyway.
        if let Some(perms) = perms.filter(|p| cfg!(unix) || !p.readonly()) {
            file.set_permissions(perms)?;
        }
        file.sync_all()?;
        Ok(staged)
    }

    /// Atomically replace the destination with the staged file.
    pub fn publish(mut self) -> std::io::Result<()> {
        std::fs::rename(&self.tmp, &self.dest)?;
        self.published = true;
        Ok(())
    }

    /// Where the replacement is staged.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.tmp
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// A directory being built beside its destination, removed unless published.
struct StagedDir(Option<PathBuf>);

impl Drop for StagedDir {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The directory [`publish_dir`] stages in, beside the destinations it
/// publishes: everything in it named by [`unique`] is spm's, so what an
/// interrupted publish left there can be cleared without guessing whether a
/// look-alike name beside the destination is the user's. Removed once empty.
const STAGING: &str = ".spm-staging";

/// The [`STAGING`] directory of `parent`, created if needed and removed (when
/// empty) on drop.
struct Staging(PathBuf);

impl Staging {
    /// Create (or reuse) `parent`'s staging directory, clearing what an
    /// interrupted publish left in it. A symlink there is refused: clearing
    /// through it could delete files of the user's.
    fn open(parent: &Path) -> std::io::Result<Self> {
        let dir = parent.join(STAGING);
        match std::fs::create_dir(&dir) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            res => res?,
        }
        let staging = Self(dir);
        if !std::fs::symlink_metadata(&staging.0)?.is_dir() {
            return Err(std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!("{} is not a directory", staging.0.display()),
            ));
        }
        for entry in std::fs::read_dir(&staging.0)? {
            let entry = entry?;
            let leftover = entry
                .file_name()
                .to_str()
                .is_some_and(|n| is_unique(n, "tmp") || is_unique(n, "old"));
            if !leftover {
                continue;
            }
            // `remove_dir_all` unlinks a symlink itself, never its target.
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(staging)
    }

    /// A fresh path in the staging directory.
    fn entry(&self, ext: &str) -> PathBuf {
        self.0.join(unique(ext))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Replace the directory (or symlink) at `dest` with the one `build` fills,
/// creating `dest`'s parent if needed. `build` writes into an empty directory
/// staged beside `dest` (in its parent's [`STAGING`] directory, so even a
/// name as long as the filesystem allows can be staged); only once it
/// succeeds does the new entry take the previous one's place, which is then
/// removed. A failure (in `build` or while swapping) leaves the previous
/// entry intact and nothing staged behind; a previous entry that can't be
/// removed whole is put back as left, as a failed in-place removal would
/// leave it. Any other file at `dest` (a regular file, a FIFO) is the
/// user's: it is refused before anything is built.
///
/// Where the OS can swap two entries at once (Linux, macOS), readers see the
/// old entry or the new one throughout. Elsewhere (Windows, or a filesystem
/// that can't swap), the previous entry is moved aside first, so for a moment
/// there is none. An interrupted publish leaves its staged entries (and
/// there, perhaps no `dest`): the next publish beside it clears them, so
/// callers must not publish into one directory concurrently (spm holds the
/// state lock).
pub fn publish_dir(
    dest: &Path,
    build: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let clearing = || format!("clearing {}", dest.display());
    let writing = || format!("writing {}", dest.display());
    let existing = match std::fs::symlink_metadata(dest) {
        Ok(m) if m.is_dir() || m.file_type().is_symlink() => true,
        Ok(_) => return Err(std::io::Error::from(ErrorKind::NotADirectory)).with_context(clearing),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => return Err(e).with_context(clearing),
    };
    let parent = dest
        .parent()
        .with_context(|| format!("{} has no parent directory", dest.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let staging = Staging::open(parent)
        .with_context(|| format!("staging in {}", parent.join(STAGING).display()))?;
    let tmp = staging.entry("tmp");
    std::fs::create_dir(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut staged = StagedDir(Some(tmp.clone()));
    build(&tmp)?;
    if !existing {
        std::fs::rename(&tmp, dest).with_context(writing)?;
        staged.0 = None;
        return Ok(());
    }

    // `old` ends up holding the previous entry.
    let old = if exchange(&tmp, dest).with_context(writing)? {
        tmp
    } else {
        // A directory cannot be renamed over a non-empty one (nor over any
        // directory on Windows), so the previous entry is moved aside first
        // and restored should the new one fail to take its place.
        let aside = staging.entry("old");
        std::fs::rename(dest, &aside).with_context(clearing)?;
        if let Err(e) = std::fs::rename(&tmp, dest) {
            let _ = std::fs::rename(&aside, dest);
            return Err(e).with_context(writing);
        }
        aside
    };
    staged.0 = None;
    // `remove_dir_all` unlinks a symlink itself, never its target.
    let Err(e) = std::fs::remove_dir_all(&old) else {
        return Ok(());
    };
    // What is left of an entry that can't be removed whole (e.g. a read-only
    // subdirectory of the user's) goes back in place, as if removed in place,
    // rather than lingering hidden beside the new one; the new one is dropped.
    let new = if exchange(&old, dest).unwrap_or(false) {
        Some(old)
    } else {
        let new = staging.entry("tmp");
        match std::fs::rename(dest, &new) {
            Ok(()) if std::fs::rename(&old, dest).is_ok() => Some(new),
            Ok(()) => {
                let _ = std::fs::rename(&new, dest);
                None
            }
            Err(_) => None,
        }
    };
    drop(StagedDir(new));
    Err(e).with_context(|| format!("removing {}", dest.display()))
}

/// Atomically swap the entries at `a` and `b`, both existing: `false`, with
/// nothing changed, where the OS or the filesystem can't.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let cstr = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes());
    let (a, b) = (
        cstr(a).map_err(std::io::Error::other)?,
        cstr(b).map_err(std::io::Error::other)?,
    );
    // SAFETY: `a` and `b` are valid NUL-terminated strings outliving the call.
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    // SAFETY: as above.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::ENOTSUP) => Ok(false),
        _ => Err(err),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange(_: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "spm-atomic-{tag}-{}-{n}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every name in `dir`, sorted.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A failed write (e.g. a full disk) or an abandoned stage leaves the
    /// previous file intact and no staging file behind.
    #[test]
    fn a_failed_replacement_keeps_the_previous_file() {
        let dir = scratch("failed");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let err = replace(&p, |f| {
            f.write_all(b"ne")?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        drop(Staged::write(&p, |f| f.write_all(b"new")).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        assert_eq!(entries(&dir), ["state.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Until published, readers see the old file and the staged one beside
    /// it; publishing replaces the entry at the destination.
    #[test]
    fn publishing_replaces_the_destination_at_once() {
        let dir = scratch("publish");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let staged = Staged::write(&p, |f| f.write_all(b"new")).unwrap();
        assert!(staged.path().is_file());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        staged.publish().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(entries(&dir), ["state.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A replaced file keeps its permissions, as an in-place write would.
    #[cfg(unix)]
    #[test]
    fn a_replaced_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        replace(&p, |f| f.write_all(b"new")).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The staged file is never more open than the one it replaces, not even
    /// while being written: a descriptor opened then would outlive a chmod.
    #[cfg(unix)]
    #[test]
    fn a_staged_file_is_private_while_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode-staged");
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        replace(&p, |f| {
            assert_eq!(f.metadata()?.permissions().mode() & 0o077, 0);
            f.write_all(b"new")
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A symlink, even a dangling one or a chain of them, is written through:
    /// the links stay and what they point at is replaced.
    #[cfg(unix)]
    #[test]
    fn replace_writes_through_symlinks() {
        let dir = scratch("symlink");
        let real = dir.join("real.json");
        let link = dir.join("link.json");
        let chain = dir.join("chain.json");
        std::os::unix::fs::symlink("real.json", &link).unwrap();
        std::os::unix::fs::symlink(&link, &chain).unwrap();
        replace(&chain, |f| f.write_all(b"dangling")).unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "dangling");
        replace(&link, |f| f.write_all(b"new")).unwrap();
        for l in [&link, &chain] {
            assert!(std::fs::symlink_metadata(l).unwrap().is_symlink());
        }
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(entries(&dir), ["chain.json", "link.json", "real.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn write_dir(dir: &Path, content: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir.join("sub"))?;
        std::fs::write(dir.join("sub").join("file"), content)?;
        Ok(())
    }

    /// A directory is created, then replaced as a whole: entries the new
    /// build lacks are gone, and nothing is left beside it.
    #[test]
    fn publish_dir_creates_then_replaces_a_directory() {
        let root = scratch("dir");
        let dest = root.join("parent").join("skill");
        publish_dir(&dest, |d| {
            write_dir(d, "v1")?;
            Ok(std::fs::write(d.join("dropped"), "x")?)
        })
        .unwrap();
        publish_dir(&dest, |d| write_dir(d, "v2")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/file")).unwrap(),
            "v2"
        );
        assert_eq!(entries(&dest), ["sub"]);
        assert_eq!(entries(dest.parent().unwrap()), ["skill"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A build failing part-way keeps the previous directory intact and
    /// leaves no staged directory behind.
    #[test]
    fn a_failed_build_keeps_the_previous_directory() {
        let root = scratch("dir-failed");
        let dest = root.join("skill");
        publish_dir(&dest, |d| write_dir(d, "old")).unwrap();
        let err = publish_dir(&dest, |d| {
            write_dir(d, "half")?;
            anyhow::bail!("disk full")
        })
        .unwrap_err();
        assert_eq!(format!("{err:#}"), "disk full");
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/file")).unwrap(),
            "old"
        );
        assert_eq!(entries(&root), ["skill"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What an interrupted publish left in the staging directory (a staged
    /// build, a previous entry moved aside) is cleared by the next one beside
    /// it, even with the destination gone; entries beside the destination
    /// are never taken for leftovers, whatever their names.
    #[test]
    fn publish_dir_clears_only_what_an_interrupted_one_left() {
        let root = scratch("dir-interrupted");
        let dest = root.join("greet");
        let staging = root.join(STAGING);
        write_dir(&staging.join("123-456-7.old"), "old").unwrap();
        write_dir(&staging.join("123-456-8.tmp"), "half").unwrap();
        std::fs::write(staging.join("123-456-9.tmp"), "half").unwrap();
        for mine in [".greet.123-456-7.tmp", ".greet.2026-01-01.old"] {
            write_dir(&root.join(mine), "mine").unwrap();
        }
        publish_dir(&dest, |d| write_dir(d, "new")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/file")).unwrap(),
            "new"
        );
        assert_eq!(
            entries(&root),
            [".greet.123-456-7.tmp", ".greet.2026-01-01.old", "greet"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A name as long as the filesystem allows is staged and replaced: the
    /// staged entry's name does not grow with it.
    #[test]
    fn publish_dir_stages_a_name_of_any_length() {
        let root = scratch("dir-long");
        let dest = root.join("n".repeat(255));
        publish_dir(&dest, |d| write_dir(d, "v1")).unwrap();
        publish_dir(&dest, |d| write_dir(d, "v2")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/file")).unwrap(),
            "v2"
        );
        assert_eq!(entries(&root), ["n".repeat(255)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A file of the user's at the destination is refused before anything
    /// is built, and left as it was.
    #[test]
    fn publish_dir_refuses_a_file_in_the_way() {
        let root = scratch("dir-file");
        let dest = root.join("skill");
        std::fs::write(&dest, "mine").unwrap();
        let mut built = false;
        let err = publish_dir(&dest, |_| {
            built = true;
            Ok(())
        })
        .unwrap_err();
        assert!(!built);
        assert!(
            format!("{err:#}").starts_with(&format!("clearing {}", dest.display())),
            "{err:#}"
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "mine");
        assert_eq!(entries(&root), ["skill"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A previous entry that can't be removed whole (a read-only subdirectory)
    /// fails the publish and is put back as left, not hidden beside the new
    /// one.
    #[cfg(unix)]
    #[test]
    fn an_undeletable_previous_directory_is_put_back() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("dir-locked");
        let dest = root.join("skill");
        let locked = dest.join("locked");
        write_dir(&locked, "old").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root can delete from a read-only directory: nothing to put back.
        if std::fs::write(locked.join("probe"), "").is_ok() {
            std::fs::remove_file(locked.join("probe")).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::remove_dir_all(&root).unwrap();
            return;
        }
        let err = publish_dir(&dest, |d| write_dir(d, "new")).unwrap_err();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            format!("{err:#}").starts_with(&format!("removing {}", dest.display())),
            "{err:#}"
        );
        assert!(locked.join("sub").is_dir(), "left in place");
        assert!(!dest.join("sub").exists(), "the new build is dropped");
        assert_eq!(entries(&root), ["skill"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A symlink at the destination is replaced as a link: its target is
    /// never touched.
    #[cfg(unix)]
    #[test]
    fn publish_dir_replaces_a_symlink_without_following_it() {
        let root = scratch("dir-link");
        let target = root.join("elsewhere");
        write_dir(&target, "keep").unwrap();
        let dest = root.join("skill");
        std::os::unix::fs::symlink(&target, &dest).unwrap();
        publish_dir(&dest, |d| write_dir(d, "new")).unwrap();
        assert!(std::fs::symlink_metadata(&dest).unwrap().is_dir());
        assert_eq!(
            std::fs::read_to_string(target.join("sub/file")).unwrap(),
            "keep"
        );
        assert_eq!(entries(&root), ["elsewhere", "skill"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
