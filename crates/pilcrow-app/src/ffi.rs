//! Platform file-metadata FFI.
//!
//! ACL and extended-attribute calls are grouped here so the save path stays
//! about content and conflict checks. On macOS [`Acl`] owns the `acl_t`
//! pointer; on platforms without ACL support it is an empty type and the
//! capture/apply paths are no-ops.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::path::Path;

/// Metadata captured from the old note before a rename.
///
/// Applying it after the rename keeps the temp file private for its whole life
/// and makes metadata preservation best effort: a filesystem that does not
/// support ACLs or xattrs still gets the new content.
pub(crate) struct CapturedMetadata {
    acl: Acl,
    attrs: Vec<(CString, Vec<u8>)>,
}

impl CapturedMetadata {
    /// Applies the captured security metadata. Returns false when an ACL was
    /// present but could not be restored; the caller then leaves the note at
    /// its private temp mode rather than widening access.
    pub(crate) fn apply(&self, path: &Path) -> bool {
        let mut acl_ok = self.acl.apply(path);
        for (name, value) in &self.attrs {
            if name.to_bytes() == b"system.posix_acl_access" {
                if set_xattr(path, name, value).is_err() {
                    acl_ok = false;
                }
            } else {
                let _ = set_xattr(path, name, value);
            }
        }
        acl_ok
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct Acl(*mut libc::c_void);

#[cfg(target_os = "macos")]
impl Acl {
    fn read(path: *const std::ffi::c_char) -> Self {
        #[cfg(target_os = "macos")]
        const ACL_TYPE_EXTENDED: libc::c_uint = 0x100;

        unsafe extern "C" {
            fn acl_get_file(path: *const libc::c_char, acl_type: libc::c_uint)
            -> *mut libc::c_void;
        }
        Self(unsafe { acl_get_file(path, ACL_TYPE_EXTENDED) })
    }

    fn apply(&self, path: &Path) -> bool {
        if self.0.is_null() {
            return true;
        }
        use std::os::unix::ffi::OsStrExt;

        #[cfg(target_os = "macos")]
        const ACL_TYPE_EXTENDED: libc::c_uint = 0x100;

        unsafe extern "C" {
            fn acl_set_file(
                path: *const libc::c_char,
                acl_type: libc::c_uint,
                acl: *mut libc::c_void,
            ) -> libc::c_int;
        }

        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        if unsafe { acl_set_file(path.as_ptr(), ACL_TYPE_EXTENDED, self.0) } == 0 {
            true
        } else {
            eprintln!(
                "pilcrow: could not restore the note ACL: {}",
                io::Error::last_os_error()
            );
            false
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Acl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe extern "C" {
                fn acl_free(object: *mut libc::c_void) -> libc::c_int;
            }
            unsafe {
                acl_free(self.0);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct Acl;

#[cfg(not(target_os = "macos"))]
impl Acl {
    fn read(_path: *const std::ffi::c_char) -> Self {
        Self
    }

    fn apply(&self, _path: &Path) -> bool {
        true
    }
}

/// Read the old note's ACL and extended attributes, best effort.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
pub(crate) fn capture_metadata(path: &Path) -> Option<CapturedMetadata> {
    use std::os::unix::ffi::OsStrExt;

    let path_c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let attrs = read_xattrs(&path_c);
    Some(CapturedMetadata {
        acl: Acl::read(path_c.as_ptr()),
        attrs,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(crate) fn capture_metadata(_path: &Path) -> Option<CapturedMetadata> {
    Some(CapturedMetadata {
        acl: Acl,
        attrs: Vec::new(),
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
fn read_xattrs(path: &CString) -> Vec<(CString, Vec<u8>)> {
    let mut out = Vec::new();
    let size = unsafe { list_xattr(path.as_ptr(), std::ptr::null_mut(), 0) };
    if size <= 0 {
        return out;
    }
    let mut names = vec![0u8; size as usize];
    let read = unsafe { list_xattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if read <= 0 {
        return out;
    }
    names.truncate(read as usize);
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let Ok(name) = CString::new(name) else {
            continue;
        };
        let size = unsafe { get_xattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if size < 0 {
            continue;
        }
        let mut value = vec![0u8; size as usize];
        let read = unsafe {
            get_xattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if read < 0 {
            continue;
        }
        value.truncate(read as usize);
        out.push((name, value));
    }
    out
}

#[cfg(target_os = "macos")]
unsafe fn list_xattr(path: *const libc::c_char, buffer: *mut libc::c_char, size: usize) -> isize {
    unsafe { libc::listxattr(path, buffer, size, 0) }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn list_xattr(path: *const libc::c_char, buffer: *mut libc::c_char, size: usize) -> isize {
    unsafe { libc::listxattr(path, buffer, size) }
}

#[cfg(target_os = "macos")]
unsafe fn get_xattr(
    path: *const libc::c_char,
    name: *const libc::c_char,
    buffer: *mut libc::c_void,
    size: usize,
) -> isize {
    unsafe { libc::getxattr(path, name, buffer, size, 0, 0) }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn get_xattr(
    path: *const libc::c_char,
    name: *const libc::c_char,
    buffer: *mut libc::c_void,
    size: usize,
) -> isize {
    unsafe { libc::getxattr(path, name, buffer, size) }
}

#[cfg(target_os = "macos")]
fn set_xattr(path: &Path, name: &CString, value: &[u8]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_xattr(path: &Path, name: &CString, value: &[u8]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn set_xattr(_path: &Path, _name: &CString, _value: &[u8]) -> io::Result<()> {
    Ok(())
}

/// Remove an ACL inherited from the directory's default ACL, best effort.
#[cfg(target_os = "macos")]
pub(crate) fn clear_inherited_acl(file: &File, _path: &Path) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    unsafe extern "C" {
        fn acl_init(count: libc::c_int) -> *mut libc::c_void;
        fn acl_set_fd(fd: libc::c_int, acl: *mut libc::c_void) -> libc::c_int;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }

    let acl = unsafe { acl_init(0) };
    if acl.is_null() {
        return Ok(());
    }
    let result = unsafe { acl_set_fd(file.as_raw_fd(), acl) };
    unsafe {
        acl_free(acl);
    }
    if result != 0 {
        // ENOTSUP, EPERM and filesystems without ACLs are fine; the temp file
        // remains mode 0600.
        return Ok(());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn clear_inherited_acl(_file: &File, path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes())?;
    let name = c"system.posix_acl_access";
    let result = unsafe { libc::removexattr(path.as_ptr(), name.as_ptr()) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENODATA) | Some(libc::ENOTSUP)
    ) {
        return Ok(());
    }
    // A filesystem without ACL support simply keeps the 0600 mode.
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(crate) fn clear_inherited_acl(_file: &File, _path: &Path) -> io::Result<()> {
    Ok(())
}
