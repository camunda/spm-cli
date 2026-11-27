//! Keeping a replaced file's access control. Renaming a staged file over
//! another publishes a new inode, so whatever guarded the old one (its
//! owner, mode, access-control list (ACL) and extended attributes) must be
//! carried over, or the new file is as open as its directory makes it. An
//! in-place write kept all of it; a replacement that can't keep it fails
//! rather than publish a more open file.
//!
//! - Unix: the staged file takes the original's owner and group, then its
//!   mode, then (Linux) its extended attributes, the POSIX ACL among them,
//!   or (macOS) its ACL and extended attributes.
//! - Windows: [`replace`] replaces an existing file with `ReplaceFileW`,
//!   which keeps the replaced file's ACL, owner and attributes.

use std::fs::{File, Metadata};
use std::path::Path;

/// Give the staged file `tmp` (open as `file`) the access control of `dest`,
/// whose metadata is `original`.
#[cfg(unix)]
pub(super) fn copy(
    dest: &Path,
    tmp: &Path,
    file: &File,
    original: &Metadata,
) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let staged = file.metadata()?;
    if (staged.uid(), staged.gid()) != (original.uid(), original.gid()) {
        std::os::unix::fs::fchown(file, Some(original.uid()), Some(original.gid()))?;
    }
    // After the owner: changing it clears the set-user/group-ID bits.
    file.set_permissions(std::fs::Permissions::from_mode(original.mode() & 0o7777))?;
    xattr::copy(dest, tmp)
}

#[cfg(not(unix))]
pub(super) fn copy(_: &Path, _: &Path, _: &File, _: &Metadata) -> std::io::Result<()> {
    Ok(())
}

/// Elsewhere a rename keeps nothing to carry over: `false`, nothing done.
#[cfg(not(windows))]
pub(super) fn replace(_: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// Replace an existing file `dest` with `tmp`, keeping `dest`'s ACL, owner
/// and attributes (a rename would give it `tmp`'s, inherited from the
/// directory); any of them that can't be kept fails the replacement.
/// `false`, nothing done, where there is no file at `dest` to keep them of.
#[cfg(windows)]
pub(super) fn replace(tmp: &Path, dest: &Path) -> std::io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    if !std::fs::symlink_metadata(dest).is_ok_and(|m| m.is_file()) {
        return Ok(false);
    }
    let wide = |p: &Path| {
        let mut w: Vec<u16> = p.as_os_str().encode_wide().collect();
        w.push(0);
        w
    };
    let (dest, tmp) = (wide(dest), wide(tmp));
    // SAFETY: `dest` and `tmp` are NUL-terminated UTF-16 strings outliving
    // the call; no backup file, no flags, and the reserved pointers null.
    let ok = unsafe {
        ReplaceFileW(
            dest.as_ptr(),
            tmp.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(true)
}

/// Extended attributes, the POSIX ACL (`system.posix_acl_access`) among
/// them.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod xattr {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const ACL: &CStr = c"system.posix_acl_access";

    fn cstr(p: &Path) -> std::io::Result<CString> {
        CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other)
    }

    fn unsupported(e: &std::io::Error) -> bool {
        e.raw_os_error() == Some(libc::ENOTSUP)
    }

    /// Size a buffer with a first call, then fill it: `call(ptr, len)`
    /// returns the length used, or -1.
    fn sized(mut call: impl FnMut(*mut u8, usize) -> isize) -> std::io::Result<Vec<u8>> {
        let len = usize::try_from(call(std::ptr::null_mut(), 0))
            .map_err(|_| std::io::Error::last_os_error())?;
        let mut buf = vec![0u8; len];
        let len = usize::try_from(call(buf.as_mut_ptr(), buf.len()))
            .map_err(|_| std::io::Error::last_os_error())?;
        buf.truncate(len);
        Ok(buf)
    }

    /// The names of `p`'s extended attributes; none where the filesystem
    /// has none.
    fn names(p: &CStr) -> std::io::Result<Vec<CString>> {
        // SAFETY: `p` is NUL-terminated and `buf` holds `len` bytes.
        let list = sized(|buf, len| unsafe { libc::llistxattr(p.as_ptr(), buf.cast(), len) });
        match list {
            Ok(list) => Ok(list
                .split(|b| *b == 0)
                .filter(|n| !n.is_empty())
                .filter_map(|n| CString::new(n).ok())
                .collect()),
            Err(e) if unsupported(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The value of `p`'s attribute `name`, if it has one.
    fn get(p: &CStr, name: &CStr) -> std::io::Result<Option<Vec<u8>>> {
        // SAFETY: `p` and `name` are NUL-terminated and `buf` holds `len`
        // bytes.
        let value = sized(|buf, len| unsafe {
            libc::lgetxattr(p.as_ptr(), name.as_ptr(), buf.cast(), len)
        });
        match value {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.raw_os_error() == Some(libc::ENODATA) || unsupported(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn check(rc: libc::c_int) -> std::io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Give `to` every extended attribute of `from` (set only where it
    /// differs, so one `to` already carries, like a security label, needs no
    /// privilege), and drop an access ACL `to` inherited from its
    /// directory's default ACL that `from` lacks.
    pub(super) fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        let (from, to) = (cstr(from)?, cstr(to)?);
        for name in names(&from)? {
            let Some(value) = get(&from, &name)? else {
                continue;
            };
            if get(&to, &name)?.as_deref() == Some(&value[..]) {
                continue;
            }
            // SAFETY: `to` and `name` are NUL-terminated and `value` holds
            // `value.len()` bytes.
            check(unsafe {
                libc::lsetxattr(
                    to.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            })?;
        }
        if get(&from, ACL)?.is_none() && get(&to, ACL)?.is_some() {
            // SAFETY: `to` and `ACL` are NUL-terminated.
            check(unsafe { libc::lremovexattr(to.as_ptr(), ACL.as_ptr()) })?;
        }
        Ok(())
    }
}

/// The ACL and extended attributes, through `copyfile(3)`.
#[cfg(target_os = "macos")]
mod xattr {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub(super) fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        let cstr = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes());
        let (from, to) = (
            cstr(from).map_err(std::io::Error::other)?,
            cstr(to).map_err(std::io::Error::other)?,
        );
        let flags = libc::COPYFILE_ACL | libc::COPYFILE_XATTR | libc::COPYFILE_NOFOLLOW;
        // SAFETY: `from` and `to` are NUL-terminated; no copy state.
        let rc = unsafe { libc::copyfile(from.as_ptr(), to.as_ptr(), std::ptr::null_mut(), flags) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// Other Unixes: the owner and mode only.
#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
mod xattr {
    pub(super) fn copy(_: &std::path::Path, _: &std::path::Path) -> std::io::Result<()> {
        Ok(())
    }
}
