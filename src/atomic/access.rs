//! Keeping a replaced file's access control. Renaming a staged file over
//! another publishes a new file, so whatever guarded the old one (its owner,
//! mode, access-control list (ACL) and extended attributes) must be carried
//! over, or the new file is as open as its directory makes it. An in-place
//! write kept all of it; a replacement that can't keep it fails rather than
//! publish a more open file. Until it carries them, the staged file is
//! open to its owner alone, so nothing written to it is exposed meanwhile.
//!
//! - Unix: the staged file is created `0600` (on macOS, whose ACLs the mode
//!   does not mask, with an empty ACL that inherits nothing from its
//!   directory), then takes the original's owner and group, then (Linux) its
//!   extended attributes, the POSIX ACL among them, or (macOS) its ACL and
//!   extended attributes, and only then its mode, which grants no more than
//!   that ACL.
//! - Windows: the staged file is opened exclusively and given a protected,
//!   owner-only DACL before anything is written to it; [`replace`] then
//!   publishes it with `ReplaceFileW`, which gives it the replaced file's
//!   DACL and attributes.

use std::fs::{File, Metadata};
use std::path::Path;

/// Create the stage `tmp` of a file replacing an existing one, open to its
/// owner alone until [`restrict`] gives it the original's access control.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn create(tmp: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // A directory's default POSIX ACL is masked by the group bits: none here.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

/// macOS ACLs are not masked by the mode, so a `0600` file would still take
/// its directory's inheritable entries: it is created with an empty ACL
/// flagged not to inherit, at once (`openx_np`).
#[cfg(target_os = "macos")]
pub(super) fn create(tmp: &Path) -> std::io::Result<File> {
    macos::create(tmp)
}

/// Open the stage exclusively, with the right to change its DACL, so no one
/// else can open it before [`restrict`] makes it owner-only.
#[cfg(windows)]
pub(super) fn create(tmp: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .access_mode(GENERIC_WRITE | WRITE_DAC)
        .open(tmp)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn create(tmp: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

/// `openx_np` and the ACL calls it takes, which `libc` does not bind.
#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::fs::File;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const ACL_FLAG_NO_INHERIT: c_int = 1 << 17;
    const FILESEC_MODE: c_int = 4;
    const FILESEC_ACL: c_int = 5;

    extern "C" {
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_free(obj: *mut c_void) -> c_int;
        fn acl_get_flagset_np(obj: *mut c_void, flagset: *mut *mut c_void) -> c_int;
        fn acl_add_flag_np(flagset: *mut c_void, flag: c_int) -> c_int;
        fn acl_set_flagset_np(obj: *mut c_void, flagset: *mut c_void) -> c_int;
        fn filesec_init() -> *mut c_void;
        fn filesec_free(fsec: *mut c_void);
        fn filesec_set_property(fsec: *mut c_void, property: c_int, value: *const c_void) -> c_int;
        fn openx_np(path: *const c_char, flags: c_int, fsec: *mut c_void) -> c_int;
    }

    /// An `acl_t` or `filesec_t`, freed when dropped.
    struct Owned(*mut c_void, unsafe extern "C" fn(*mut c_void));

    impl Owned {
        fn new(p: *mut c_void, free: unsafe extern "C" fn(*mut c_void)) -> std::io::Result<Self> {
            if p.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self(p, free))
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from the allocator `self.1` frees, once.
            unsafe { (self.1)(self.0) }
        }
    }

    unsafe extern "C" fn free_acl(p: *mut c_void) {
        // SAFETY: the caller passes an `acl_t` from `acl_init`.
        unsafe { acl_free(p) };
    }

    fn check(rc: c_int) -> std::io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    pub(super) fn create(tmp: &Path) -> std::io::Result<File> {
        let path = CString::new(tmp.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
        let mode: libc::mode_t = 0o600;
        // SAFETY: each pointer is checked non-null by `Owned::new` and lives
        // until its guard drops, after `openx_np` has copied what it needs;
        // `path` is NUL-terminated and `fd`, when valid, is owned by no one
        // else.
        unsafe {
            let acl = Owned::new(acl_init(0), free_acl)?;
            let mut flags = std::ptr::null_mut();
            check(acl_get_flagset_np(acl.0, &mut flags))?;
            check(acl_add_flag_np(flags, ACL_FLAG_NO_INHERIT))?;
            check(acl_set_flagset_np(acl.0, flags))?;
            let fsec = Owned::new(filesec_init(), filesec_free)?;
            let acl_ptr: *const *mut c_void = &acl.0;
            check(filesec_set_property(fsec.0, FILESEC_ACL, acl_ptr.cast()))?;
            let mode_ptr: *const libc::mode_t = &mode;
            check(filesec_set_property(fsec.0, FILESEC_MODE, mode_ptr.cast()))?;
            let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
            let fd = openx_np(path.as_ptr(), flags, fsec.0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(File::from_raw_fd(fd))
        }
    }
}

/// Give the staged file `tmp` (open as `file`, from [`create`]) the
/// access control of `dest`, whose metadata is `original`, before anything
/// is written to it. Each step narrows or keeps what the one before
/// allowed: owner, then ACL, then mode.
#[cfg(unix)]
pub(super) fn restrict(
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
    xattr::copy(dest, tmp)?;
    // Last: with an ACL in place, the group bits are its mask, so the mode
    // grants nothing the ACL does not. (After the owner too, as changing it
    // clears the set-user/group-ID bits.)
    file.set_permissions(std::fs::Permissions::from_mode(original.mode() & 0o7777))
}

/// Make the staged file `file` owner-only: a protected DACL, so none of its
/// directory's inheritable entries apply. [`replace`] gives it `dest`'s.
#[cfg(windows)]
pub(super) fn restrict(_: &Path, _: &Path, file: &File, _: &Metadata) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };
    // Protected (`P`), one entry: full access (`FA`) for the owner (`OW`).
    let sddl: Vec<u16> = "D:P(A;;FA;;;OW)".encode_utf16().chain([0]).collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated; `sd` receives a LocalAlloc'd
    // descriptor, freed below.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor; `dacl` points into it.
    let ok = unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) };
    let rc = if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: the handle is open with WRITE_DAC (see `private`) and
        // `dacl` is valid while `sd` is.
        let rc = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(rc as i32))
        }
    };
    // SAFETY: `sd` was allocated by the conversion above.
    unsafe { LocalFree(sd) };
    rc
}

#[cfg(not(any(unix, windows)))]
pub(super) fn restrict(_: &Path, _: &Path, _: &File, _: &Metadata) -> std::io::Result<()> {
    Ok(())
}

/// Replace the file `dest` with `tmp`, moving `dest` to `backup` on the
/// way, with `ReplaceFileW`: `tmp` takes `dest`'s DACL and attributes (any
/// of which that can't be kept fails it). On failure the previous file is
/// at `dest` or, at worst, `backup` (see `replace_with_backup`).
#[cfg(windows)]
pub(super) fn replace(tmp: &Path, dest: &Path, backup: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let (dest, tmp, backup) = (verbatim(dest)?, verbatim(tmp)?, verbatim(backup)?);
    // SAFETY: the paths are NUL-terminated UTF-16 strings outliving the
    // call; no flags, and the reserved pointers null.
    let ok = unsafe {
        ReplaceFileW(
            dest.as_ptr(),
            tmp.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `p` as a NUL-terminated `\\?\` path, which (as std does for its own
/// calls) lifts the `MAX_PATH` limit on what `ReplaceFileW` accepts.
#[cfg(windows)]
fn verbatim(p: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
    let p: Vec<u16> = std::path::absolute(p)?.as_os_str().encode_wide().collect();
    let mut out = if p.starts_with(&wide(r"\\?\")) {
        p
    } else if let Some(unc) = p.strip_prefix(&wide(r"\\")[..]) {
        [wide(r"\\?\UNC\"), unc.to_vec()].concat()
    } else {
        [wide(r"\\?\"), p].concat()
    };
    out.push(0);
    Ok(out)
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
