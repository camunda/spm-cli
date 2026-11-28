//! Publishing files and directories all at once: the one implementation
//! behind every file and directory spm writes outside the store. A new
//! version is staged beside its destination and renamed into place, so a
//! failed or interrupted write leaves the previous one intact, and a reader
//! never sees a truncated file or a half-copied directory: the old one or
//! the new one (or, for a directory where the OS can't swap two entries,
//! briefly neither; see [`publish_dir`]).

mod access;

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
    // One more inspection than links followed: the last link's target too.
    for followed in 0..=MAX_LINKS {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() && followed < MAX_LINKS => {
                let target = std::fs::read_link(&p)?;
                p = match p.parent() {
                    Some(dir) => dir.join(target),
                    None => target,
                };
            }
            Ok(m) if m.file_type().is_symlink() => break,
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

/// A hidden, unique sibling of `dest` (`.spm-<pid>-<nanos>-<n>.<ext>`): on
/// the same filesystem, so renaming it over `dest` is atomic. Its name does
/// not grow with `dest`'s, so any name the filesystem allows can be staged.
/// It is always created exclusively, so a name of the user's that happens to
/// match fails the write rather than being overwritten.
fn sibling(dest: &Path, ext: &str) -> std::io::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let Some(dir) = dest.parent().filter(|_| dest.file_name().is_some()) else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{} is not a file path", dest.display()),
        ));
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!(".spm-{}-{nanos}-{n}.{ext}", std::process::id())))
}

/// The regular file `dest` replaces, if any: a symlink there is replaced
/// (keeping its target's access when that is a file), a missing one is
/// created, and anything else (a directory, FIFO or device) is refused
/// rather than swapped for a regular file.
fn original(dest: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    let absent = |e: std::io::Error| match e.kind() {
        ErrorKind::NotFound => Ok(None),
        _ => Err(e),
    };
    match std::fs::symlink_metadata(dest) {
        Ok(m) if m.is_file() => Ok(Some(m)),
        Ok(m) if m.file_type().is_symlink() => match std::fs::metadata(dest) {
            Ok(t) => Ok(Some(t).filter(std::fs::Metadata::is_file)),
            Err(e) => absent(e),
        },
        Ok(_) => Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{} is not a regular file", dest.display()),
        )),
        Err(e) => absent(e),
    }
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
    /// disk, with the access control of the file it replaces (see
    /// [`access`]): a write that can't keep it fails rather than publish a
    /// more open file; a `dest` it may not write is refused, read-only
    /// included, even where its directory would allow the rename, as is one
    /// that is not a regular file (see [`original`]). `dest`'s directory
    /// must exist. A symlink at `dest` is
    /// replaced, not followed: pass a [`resolve`]d path to write through it.
    pub fn write(
        dest: &Path,
        write: impl FnOnce(&mut File) -> std::io::Result<()>,
    ) -> std::io::Result<Self> {
        let tmp = sibling(dest, "tmp")?;
        let original = original(dest)?;
        if original.is_some() {
            // A rename asks only the directory: ask the file too, as the
            // in-place write this replaces did, without truncating it.
            std::fs::OpenOptions::new().write(true).open(dest)?;
        }
        let file = match original {
            Some(_) => access::create(&tmp)?,
            None => std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?,
        };
        let staged = Self {
            tmp,
            dest: dest.to_path_buf(),
            published: false,
        };
        // Bound after `staged`, so on an error or a panic it is closed before
        // `staged` removes it: Windows can't delete a file open unshared.
        let mut file = file;
        if let Some(original) = &original {
            access::restrict(dest, &staged.tmp, &file, original)?;
        }
        write(&mut file)?;
        file.sync_all()?;
        Ok(staged)
    }

    /// Atomically replace the destination with the staged file.
    pub fn publish(mut self) -> std::io::Result<()> {
        // A rename would leave the staged file's owner-only DACL in place of
        // the replaced file's.
        #[cfg(windows)]
        if std::fs::symlink_metadata(&self.dest).is_ok_and(|m| m.is_file()) {
            replace_with_backup(&self.tmp, &self.dest, access::replace)?;
            self.published = true;
            return Ok(());
        }
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

/// Replace the file `dest` with `tmp` through `replace`, which may move
/// `dest` to a fresh hidden sibling `backup` on the way and, failing, leave
/// it there (as `ReplaceFileW` can): the previous file is then put back, so
/// a failure never leaves `dest` without it. Once replaced, the backup is
/// dropped.
#[cfg(any(windows, test))]
fn replace_with_backup(
    tmp: &Path,
    dest: &Path,
    replace: impl FnOnce(&Path, &Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let backup = sibling(dest, "old")?;
    let Err(e) = replace(tmp, dest, &backup) else {
        // Only the previous version, kept for recovery; should it linger,
        // it is a hidden sibling like any abandoned stage.
        let _ = std::fs::remove_file(&backup);
        return Ok(());
    };
    let gone = |p: &Path| std::fs::symlink_metadata(p).is_err();
    if gone(dest) && !gone(&backup) {
        if let Err(r) = std::fs::rename(&backup, dest) {
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "{e}; the previous {} is kept at {}: {r}",
                    dest.display(),
                    backup.display()
                ),
            ));
        }
    }
    Err(e)
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

/// Replace the directory (or symlink) at `dest` with the one `build` fills,
/// creating `dest`'s parent if needed. `build` writes into an empty hidden
/// sibling of `dest` (see [`sibling`]); only once it succeeds does the new
/// entry take the previous one's place, which is then removed. A failure (in `build` or while swapping) leaves the previous
/// entry intact and nothing staged behind; a previous entry that can't be
/// removed whole is put back as left, as a failed in-place removal would
/// leave it. Any other file at `dest` (a regular file, a FIFO) is the
/// user's: it is refused before anything is built.
///
/// Where the OS can swap two entries at once (Linux, macOS), readers see the
/// old entry or the new one throughout. Elsewhere (Windows, or a filesystem
/// that can't swap), the previous entry is moved aside first, so for a moment
/// there is none. A process killed mid-publish leaves its hidden siblings
/// behind (and there, perhaps no `dest`, which the next publish creates
/// again); they are never deleted by name, as only the process that made
/// one knows it is spm's and abandoned.
pub fn publish_dir(
    dest: &Path,
    build: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    publish_dir_and(dest, build, |_| Ok(()))
}

/// What [`publish_dir_and`] reports about the entry it publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The new entry is in place, and the previous one is yet to be dropped.
    Claim,
    /// The new entry was withdrawn after its claim: `dest` holds what it held
    /// before (or nothing, where it held nothing).
    Release,
}

/// [`publish_dir`], reporting the new entry to `record`: [`Hold::Claim`] once
/// it is in place, before the previous one is dropped, and [`Hold::Release`]
/// should it then be withdrawn — because the claim failed, or because the
/// previous entry can't be removed whole and is put back. So what `record`
/// notes about the new entry (e.g. that spm owns it) is true whenever it
/// stays, and is taken back whenever it does not.
pub fn publish_dir_and(
    dest: &Path,
    build: impl FnOnce(&Path) -> anyhow::Result<()>,
    mut record: impl FnMut(Hold) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let clearing = || format!("clearing {}", dest.display());
    let writing = || format!("writing {}", dest.display());
    let existing = match std::fs::symlink_metadata(dest) {
        Ok(m) if m.is_dir() || m.file_type().is_symlink() => true,
        Ok(_) => return Err(std::io::Error::from(ErrorKind::NotADirectory)).with_context(clearing),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => return Err(e).with_context(clearing),
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = sibling(dest, "tmp")?;
    std::fs::create_dir(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let mut staged = StagedDir(Some(tmp.clone()));
    build(&tmp)?;
    if !existing {
        std::fs::rename(&tmp, dest).with_context(writing)?;
        staged.0 = None;
        let Err(e) = record(Hold::Claim) else {
            return Ok(());
        };
        let Ok(new) = sibling(dest, "tmp") else {
            return Err(e);
        };
        if std::fs::rename(dest, &new).is_err() {
            return Err(e);
        }
        drop(StagedDir(Some(new)));
        return Err(released(e, dest, record));
    }

    // `old` ends up holding the previous entry.
    let old = if exchange(&tmp, dest).with_context(writing)? {
        tmp
    } else {
        // A directory cannot be renamed over a non-empty one (nor over any
        // directory on Windows), so the previous entry is moved aside first
        // and restored should the new one fail to take its place.
        let aside = sibling(dest, "old")?;
        std::fs::rename(dest, &aside).with_context(clearing)?;
        if let Err(e) = std::fs::rename(&tmp, dest) {
            let _ = std::fs::rename(&aside, dest);
            return Err(e).with_context(writing);
        }
        aside
    };
    staged.0 = None;
    let e = match record(Hold::Claim) {
        Err(e) => e,
        // `remove_dir_all` unlinks a symlink itself, never its target.
        Ok(()) => match std::fs::remove_dir_all(&old) {
            Ok(()) => return Ok(()),
            // What is left of an entry that can't be removed whole (e.g. a
            // read-only subdirectory of the user's) goes back in place, as if
            // removed in place, rather than lingering hidden beside the new one.
            Err(e) => anyhow::Error::new(e).context(format!("removing {}", dest.display())),
        },
    };
    if put_back(dest, old)? {
        return Err(released(e, dest, record));
    }
    Err(e)
}

/// `err`, once `record` heard the entry at `dest` was withdrawn.
fn released(
    err: anyhow::Error,
    dest: &Path,
    mut record: impl FnMut(Hold) -> anyhow::Result<()>,
) -> anyhow::Error {
    match record(Hold::Release) {
        Ok(()) => err,
        Err(e) => err.context(format!("also failed to release {}: {e:#}", dest.display())),
    }
}

/// Put the previous entry `old` back at `dest` and drop the new one there,
/// as far as the OS allows: whatever can't be moved stays where it is.
/// `true` once `old` is back in place, the new entry gone from `dest`.
fn put_back(dest: &Path, old: PathBuf) -> std::io::Result<bool> {
    let new = if exchange(&old, dest).unwrap_or(false) {
        old
    } else {
        let new = sibling(dest, "tmp")?;
        if std::fs::rename(dest, &new).is_err() {
            return Ok(false);
        }
        if std::fs::rename(&old, dest).is_err() {
            let _ = std::fs::rename(&new, dest);
            return Ok(false);
        }
        new
    };
    drop(StagedDir(Some(new)));
    Ok(true)
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
        let d = crate::testutil::scratch_path(&format!("spm-atomic-{tag}"));
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

    /// A write that panics, like one that fails, leaves no staged file:
    /// it is closed before being removed.
    #[test]
    fn a_panicking_write_leaves_no_staged_file() {
        let dir = scratch("panic");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let unwound = std::panic::catch_unwind(|| replace(&p, |_| panic!("interrupted")));
        assert!(unwound.is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        assert_eq!(entries(&dir), ["state.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file named as long as the filesystem allows is replaced too: the
    /// staged file's name does not grow with it.
    #[test]
    fn a_file_name_of_any_length_is_replaced() {
        let dir = scratch("long");
        let p = dir.join("n".repeat(255));
        replace(&p, |f| f.write_all(b"v1")).unwrap();
        replace(&p, |f| f.write_all(b"v2")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v2");
        assert_eq!(entries(&dir), ["n".repeat(255)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Only a regular file is replaced: a directory at the destination is
    /// refused before anything is staged.
    #[test]
    fn a_directory_in_place_of_the_file_is_refused() {
        let dir = scratch("notfile");
        let p = dir.join("ai.json");
        std::fs::create_dir(&p).unwrap();
        let err = replace(&p, |f| f.write_all(b"new")).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
        assert!(p.is_dir());
        assert_eq!(entries(&dir), ["ai.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIFO or device reached through a symlink (`.gitignore -> /dev/null`)
    /// is refused, not swapped for a regular file.
    #[cfg(unix)]
    #[test]
    fn a_special_file_behind_a_symlink_is_refused() {
        use std::os::unix::fs::FileTypeExt;
        let dir = scratch("fifo");
        let fifo = dir.join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        let link = dir.join(".gitignore");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        let err = replace(&link, |f| f.write_all(b"new")).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
        assert!(std::fs::symlink_metadata(&fifo)
            .unwrap()
            .file_type()
            .is_fifo());
        assert_eq!(entries(&dir), [".gitignore", "pipe"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A read-only file is refused exactly when an in-place write would be,
    /// even though its directory would let a rename replace it: a process
    /// allowed to write it anyway (root, on Unix) replaces it.
    #[test]
    fn a_read_only_file_is_not_replaced() {
        let dir = scratch("readonly");
        let p = dir.join("ai.json");
        std::fs::write(&p, "old").unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms).unwrap();
        let writable = std::fs::OpenOptions::new().write(true).open(&p).is_ok();
        let result = replace(&p, |f| f.write_all(b"new"));
        if writable {
            result.unwrap();
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        } else {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::PermissionDenied);
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        }
        assert_eq!(entries(&dir), ["ai.json"]);
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

    /// The extended attribute `name` of `p`, if it has one.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn xattr(p: &Path, name: &str) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt;
        let p = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        let mut buf = vec![0u8; 4096];
        // SAFETY: valid NUL-terminated strings and a buffer of `buf.len()`.
        #[cfg(target_os = "linux")]
        let n = unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), buf.as_mut_ptr().cast(), 4096) };
        // SAFETY: as above.
        #[cfg(target_os = "macos")]
        let n = unsafe {
            libc::getxattr(
                p.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                4096,
                0,
                0,
            )
        };
        let n = usize::try_from(n).ok()?;
        buf.truncate(n);
        Some(buf)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn set_xattr(p: &Path, name: &str, value: &[u8]) {
        use std::os::unix::ffi::OsStrExt;
        let p = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        let (v, len) = (value.as_ptr().cast(), value.len());
        // SAFETY: valid NUL-terminated strings and a value of `len` bytes.
        #[cfg(target_os = "linux")]
        let rc = unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), v, len, 0) };
        // SAFETY: as above.
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), v, len, 0, 0) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    }

    /// A POSIX access ACL (`system.posix_acl_access`): `entries` of
    /// (tag, perm, id).
    #[cfg(target_os = "linux")]
    fn posix_acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in entries {
            acl.extend(tag.to_le_bytes());
            acl.extend(perm.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        acl
    }

    /// A replaced file keeps its access-control list and extended
    /// attributes, as an in-place write would: one granting a named user
    /// read access while denying the owning group (mode 0640) must not turn
    /// into a plain 0640 file the group can read.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_replaced_file_keeps_its_access_control_list() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("acl");
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        const ANY: u32 = u32::MAX;
        // user::rw- user:4242:r-- group::--- mask::r-- other::---
        let acl = posix_acl(&[
            (0x01, 6, ANY),
            (0x02, 4, 4242),
            (0x04, 0, ANY),
            (0x10, 4, ANY),
            (0x20, 0, ANY),
        ]);
        set_xattr(&p, "system.posix_acl_access", &acl);
        set_xattr(&p, "user.spm-test", b"kept");
        let before = xattr(&p, "system.posix_acl_access").unwrap();
        replace(&p, |f| f.write_all(b"new")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(xattr(&p, "system.posix_acl_access"), Some(before));
        assert_eq!(xattr(&p, "user.spm-test").as_deref(), Some(&b"kept"[..]));
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file without an ACL is not replaced by one inheriting its
    /// directory's default ACL, which could grant what the file did not.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_replaced_file_gains_no_inherited_access_control_list() {
        let dir = scratch("acl-default");
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        const ANY: u32 = u32::MAX;
        // default: user::rw- user:4242:rw- group::r-- mask::rw- other::---
        let default = posix_acl(&[
            (0x01, 6, ANY),
            (0x02, 6, 4242),
            (0x04, 4, ANY),
            (0x10, 6, ANY),
            (0x20, 0, ANY),
        ]);
        set_xattr(&dir, "system.posix_acl_default", &default);
        assert_eq!(xattr(&p, "system.posix_acl_access"), None);
        replace(&p, |f| f.write_all(b"new")).unwrap();
        assert_eq!(xattr(&p, "system.posix_acl_access"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A replaced file keeps its access-control list and extended
    /// attributes, as an in-place write would.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_replaced_file_keeps_its_access_control_list() {
        let dir = scratch("acl");
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        // `-n` lists each entry's stored UUID: resolving it to a name asks
        // the directory service, which can time out and print the UUID.
        let acl = |p: &Path| {
            let out = std::process::Command::new("ls")
                .arg("-len")
                .arg(p)
                .output()
                .unwrap();
            assert!(out.status.success());
            let text = String::from_utf8(out.stdout).unwrap();
            text.lines().skip(1).map(str::to_owned).collect::<Vec<_>>()
        };
        let status = std::process::Command::new("chmod")
            .args(["+a", "user:nobody deny read"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(status.success());
        set_xattr(&p, "com.camunda.spm-test", b"kept");
        let before = acl(&p);
        assert!(!before.is_empty());
        replace(&p, |f| f.write_all(b"new")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(acl(&p), before);
        assert_eq!(
            xattr(&p, "com.camunda.spm-test").as_deref(),
            Some(&b"kept"[..])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Unlike a POSIX one, a macOS ACL is not masked by the mode, so the
    /// stage inherits none of its directory's entries, not even while being
    /// written: one granting read would let a descriptor opened then outlive
    /// the stage taking the replaced file's ACL.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_replaced_file_gains_no_inherited_access_control_list() {
        let dir = scratch("inherit");
        let chmod = |args: &[&str], p: &Path| {
            let status = std::process::Command::new("chmod")
                .args(args)
                .arg(p)
                .status()
                .unwrap();
            assert!(status.success());
        };
        let acl = |p: &Path| {
            let out = std::process::Command::new("ls")
                .arg("-len")
                .arg(p)
                .output()
                .unwrap();
            assert!(out.status.success());
            let text = String::from_utf8(out.stdout).unwrap();
            text.lines().skip(1).map(str::to_owned).collect::<Vec<_>>()
        };
        chmod(&["+a", "group:everyone allow read,file_inherit"], &dir);
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        assert_eq!(acl(&p).len(), 1, "the directory's entry is inherited");
        chmod(&["-N"], &p);
        replace(&p, |f| {
            let stage = entries(&dir)
                .into_iter()
                .find(|e| e.ends_with(".tmp"))
                .unwrap();
            assert_eq!(acl(&dir.join(stage)), Vec::<String>::new());
            f.write_all(b"new")
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(acl(&p), Vec::<String>::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A replacement that fails having moved the previous file to its backup
    /// (as `ReplaceFileW` can) puts it back: no failure loses both versions.
    #[test]
    fn a_failed_replacement_puts_the_backup_back() {
        let dir = scratch("backup-failed");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let staged = Staged::write(&p, |f| f.write_all(b"new")).unwrap();
        let err = replace_with_backup(staged.path(), &p, |_, dest, backup| {
            std::fs::rename(dest, backup)?;
            Err(std::io::Error::other("unable to move replacement"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "unable to move replacement");
        drop(staged);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
        assert_eq!(entries(&dir), ["state.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A successful replacement drops the backup it made on the way.
    #[test]
    fn a_replacement_drops_its_backup() {
        let dir = scratch("backup-ok");
        let p = dir.join("state.json");
        std::fs::write(&p, "old").unwrap();
        let staged = Staged::write(&p, |f| f.write_all(b"new")).unwrap();
        replace_with_backup(staged.path(), &p, |tmp, dest, backup| {
            std::fs::rename(dest, backup)?;
            std::fs::rename(tmp, dest)
        })
        .unwrap();
        drop(staged);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(entries(&dir), ["state.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A replaced file keeps its DACL (here: protected, the current user
    /// alone), and its stage inherits none of the (more open) directory's
    /// entries while written.
    #[cfg(windows)]
    #[test]
    fn a_replaced_file_keeps_its_access_control_list() {
        let icacls = |p: &Path, args: &[&str]| {
            let out = std::process::Command::new("icacls")
                .arg(p)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let dir = scratch("acl");
        let p = dir.join("settings.json");
        std::fs::write(&p, "old").unwrap();
        let user = std::env::var("USERNAME").unwrap();
        icacls(&p, &["/inheritance:r", "/grant:r", &format!("{user}:F")]);
        let before = icacls(&p, &[]);
        assert!(!before.contains("(I)"), "{before}");
        // The directory passes entries on to new files: the stage must not
        // take them.
        assert!(icacls(&dir, &[]).contains("(OI)"));
        let staged = Staged::write(&p, |f| f.write_all(b"new")).unwrap();
        let stage = icacls(staged.path(), &[]);
        assert!(!stage.contains("(I)"), "the stage inherited: {stage}");
        staged.publish().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(icacls(&p, &[]), before);
        assert_eq!(entries(&dir), ["settings.json"]);
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

    /// A chain of exactly [`MAX_LINKS`] symlinks resolves to its target; one
    /// more is too many.
    #[cfg(unix)]
    #[test]
    fn resolve_follows_up_to_max_links() {
        let dir = scratch("max-links");
        let mut next = PathBuf::from("real.json");
        for i in 0..=MAX_LINKS {
            let link = PathBuf::from(format!("l{i}"));
            std::os::unix::fs::symlink(&next, dir.join(&link)).unwrap();
            next = link;
        }
        let longest = dir.join(format!("l{}", MAX_LINKS - 1));
        assert_eq!(resolve(&longest).unwrap(), dir.join("real.json"));
        let err = resolve(&dir.join(format!("l{MAX_LINKS}"))).unwrap_err();
        assert!(err.to_string().contains("too many levels"), "{err}");
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

    /// A failed commit (recording the new entry as spm's) puts the previous
    /// entry back, or drops a new one that replaced none: what the commit
    /// would have recorded is never left in place unrecorded.
    #[test]
    fn a_failed_commit_puts_the_previous_directory_back() {
        let root = scratch("dir-commit");
        let dest = root.join("skill");
        publish_dir(&dest, |d| write_dir(d, "old")).unwrap();
        let mut heard = Vec::new();
        let err = publish_dir_and(
            &dest,
            |d| write_dir(d, "new"),
            |hold| {
                heard.push(hold);
                match hold {
                    Hold::Claim => anyhow::bail!("record not saved"),
                    Hold::Release => Ok(()),
                }
            },
        )
        .unwrap_err();
        assert_eq!(format!("{err:#}"), "record not saved");
        assert_eq!(heard, [Hold::Claim, Hold::Release]);
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/file")).unwrap(),
            "old"
        );
        assert_eq!(entries(&root), ["skill"]);

        let fresh = root.join("fresh");
        let mut placed = Vec::new();
        let err = publish_dir_and(
            &fresh,
            |d| write_dir(d, "new"),
            |hold| {
                placed.push((hold, fresh.join("sub/file").exists()));
                match hold {
                    Hold::Claim => anyhow::bail!("record not saved"),
                    Hold::Release => anyhow::bail!("record still not saved"),
                }
            },
        )
        .unwrap_err();
        assert_eq!(
            placed,
            [(Hold::Claim, true), (Hold::Release, false)],
            "claimed once in place, released once withdrawn"
        );
        assert_eq!(
            format!("{err:#}"),
            format!(
                "also failed to release {}: record still not saved: record not saved",
                fresh.display()
            )
        );
        assert_eq!(entries(&root), ["skill"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Entries beside the destination are the user's whatever their names,
    /// even ones shaped like a staged entry: a publish never deletes them.
    #[test]
    fn publish_dir_never_takes_a_look_alike_for_its_own() {
        let root = scratch("dir-look-alike");
        let dest = root.join("greet");
        let mine = [".greet.123-456-7.tmp", ".spm-123-456-7.old", ".spm-staging"];
        for m in mine {
            write_dir(&root.join(m), "mine").unwrap();
        }
        publish_dir(&dest, |d| write_dir(d, "v1")).unwrap();
        publish_dir(&root.join(mine[1]), |d| write_dir(d, "dep")).unwrap();
        publish_dir(&dest, |d| write_dir(d, "v2")).unwrap();
        let mut all = mine.to_vec();
        all.push("greet");
        all.sort();
        assert_eq!(entries(&root), all);
        for m in [mine[0], mine[2]] {
            let file = root.join(m).join("sub/file");
            assert_eq!(std::fs::read_to_string(file).unwrap(), "mine");
        }
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
        let mut heard = Vec::new();
        let err = publish_dir_and(
            &dest,
            |d| write_dir(d, "new"),
            |hold| {
                heard.push(hold);
                Ok(())
            },
        )
        .unwrap_err();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            format!("{err:#}").starts_with(&format!("removing {}", dest.display())),
            "{err:#}"
        );
        assert_eq!(
            heard,
            [Hold::Claim, Hold::Release],
            "claimed, then released"
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
