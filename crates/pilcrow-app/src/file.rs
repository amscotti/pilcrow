//! The first file slice: read a note, identify it, and replace it atomically.
//!
//! This module deliberately has no GPUI dependency. Everything here is
//! synchronous and belongs on the background executor when it is called by the
//! app; only the launch read is allowed to touch the filesystem before the
//! event loop starts.
//!
//! A note is read as UTF-8, a leading BOM is removed, and CRLF is normalised to
//! LF. Invalid UTF-8 and missing files are errors, never an empty note: an
//! unreadable file must not look like a blank page that is safe to overwrite.
//! Writes go to a private sibling, are synced, and only then replace the
//! target. A failed write reports the failure and removes its temp file.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};

use crate::ffi::{capture_metadata, clear_inherited_acl};

/// The largest note Pilcrow will read into memory. Bigger files are refused
/// before allocation, so a sparse or accidental 1 GiB file cannot take the
/// app down.
pub const MAX_NOTE_BYTES: u64 = 64 * 1024 * 1024;

/// A fingerprint of a file's on-disk identity for save/conflict checks.
///
/// Size and modification time alone can miss an external edit that preserves
/// both (some editors write within the same timestamp tick and the same
/// length). The digest is a fast non-cryptographic hash of the raw bytes, used
/// only to answer "is this still the file we read?" — never for security.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskState {
    /// The size reported by the filesystem when the note was read or saved.
    pub size: u64,
    /// The modification time when the note was read or saved.
    pub mtime: SystemTime,
    /// FNV-1a of the raw file bytes at that revision.
    pub digest: u64,
}

impl DiskState {
    fn from_metadata_and_digest(metadata: &Metadata, digest: u64) -> Result<Self> {
        let mtime = metadata
            .modified()
            .with_context(|| "the filesystem did not report a modification time")?;
        Ok(Self {
            size: metadata.len(),
            mtime,
            digest,
        })
    }

    /// Conflict identity is content, not metadata. A `touch` that leaves the
    /// bytes alone must not force a conflict or stop autosave.
    pub fn same_content(self, other: Self) -> bool {
        self.size == other.size && self.digest == other.digest
    }
}

fn digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn digest_reader(file: &mut File) -> Result<u64> {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hash);
        }
        total = total.saturating_add(read as u64);
        if total > MAX_NOTE_BYTES {
            bail!(
                "note is larger than the {} MiB limit",
                MAX_NOTE_BYTES / (1024 * 1024)
            );
        }
        for byte in &buffer[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Read the current stamp for `path`, or `None` when the file does not exist.
///
/// Errors other than "not found" are surfaced so a permission problem cannot
/// masquerade as a brand-new note.
pub fn stamp(path: &Path) -> Result<Option<DiskState>> {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            bail!("{} is not a regular file", path.display());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not stat {}", path.display()));
        }
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not open {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("could not stat {}", path.display()))?;
    if metadata.len() > MAX_NOTE_BYTES {
        bail!(
            "{} is larger than the {} MiB note limit",
            path.display(),
            MAX_NOTE_BYTES / (1024 * 1024)
        );
    }
    let digest =
        digest_reader(&mut file).with_context(|| format!("could not read {}", path.display()))?;
    Ok(Some(DiskState::from_metadata_and_digest(
        &metadata, digest,
    )?))
}

/// Whether the file at `path` still matches `expected`.
///
/// A missing file or a different digest returns `false`. A read/permission
/// error is surfaced: the save treats it as a failure, never as permission to
/// overwrite.
pub fn unchanged(path: &Path, expected: DiskState) -> Result<bool> {
    Ok(stamp(path)?.is_some_and(|current| current.same_content(expected)))
}

/// Read a note and return its text plus the stamp it was read at.
///
/// The file is decoded strictly as UTF-8. A leading BOM is stripped and CRLF
/// line endings are normalised; every other byte is preserved. The returned
/// stamp comes from the same open file handle as the bytes, so it describes
/// the version that was actually read.
pub fn read(path: &Path) -> Result<(String, DiskState)> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        cleanup_stale_temps(parent);
    }
    let metadata =
        fs::metadata(path).with_context(|| format!("could not stat {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let mut file = File::open(path)
        .with_context(|| format!("could not open {} for reading", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("could not stat {}", path.display()))?;
    if metadata.len() > MAX_NOTE_BYTES {
        bail!(
            "{} is larger than the {} MiB note limit",
            path.display(),
            MAX_NOTE_BYTES / (1024 * 1024)
        );
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_NOTE_BYTES) as usize);
    (&mut file)
        .take(MAX_NOTE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() as u64 > MAX_NOTE_BYTES {
        bail!(
            "{} is larger than the {} MiB note limit",
            path.display(),
            MAX_NOTE_BYTES / (1024 * 1024)
        );
    }
    let digest = digest(&bytes);
    let stamp = DiskState::from_metadata_and_digest(&metadata, digest)?;

    let text = String::from_utf8(bytes).map_err(|error| {
        anyhow!(
            "{} is not valid UTF-8 ({})",
            path.display(),
            error.utf8_error()
        )
    })?;
    let text = normalise(text);
    Ok((text, stamp))
}

/// Read at most `max_bytes` from `path`, refusing a larger file before
/// allocating for it.
///
/// Auxiliary app files (drafts, session, preferences, backups) use this
/// instead of an unbounded read, so a stray huge file in the app data root
/// cannot be loaded whole on launch or during a save. A file that grows past
/// the limit after its metadata is read is still refused: the `take` cap is
/// the backstop, exactly as in [`read`].
pub fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut file = File::open(path)
        .with_context(|| format!("could not open {} for reading", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("could not stat {}", path.display()))?;
    if metadata.len() > max_bytes {
        bail!(
            "{} is larger than the {max_bytes}-byte limit",
            path.display()
        );
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes) as usize);
    (&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        bail!(
            "{} is larger than the {max_bytes}-byte limit",
            path.display()
        );
    }
    Ok(bytes)
}

fn normalise(text: String) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.replace("\r\n", "\n")
}

/// Why a checked write did not replace the note.
#[derive(Debug)]
pub enum WriteFailure {
    /// The target is no longer the version the caller last read or wrote.
    /// The payload is the state actually observed, if any, so an explicit
    /// overwrite can be bound to the version the person saw.
    Conflict(Option<DiskState>),
    /// The write failed for any other reason.
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for WriteFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<io::Error> for WriteFailure {
    fn from(error: io::Error) -> Self {
        Self::Failed(error.into())
    }
}

/// Write `bytes` to `path` through a private sibling temp file.
///
/// The sequence is: create a unique temp file in the target directory, clear
/// any inherited ACL, write, `sync_all`, copy the old file's ACL/xattrs, check
/// the expected stamp, rename over the target, restore the old mode, and sync
/// the directory. If anything fails, the temp file is removed and the error is
/// returned; the original is never truncated in place. A symlink target is
/// resolved first so saving through an alias updates the note rather than
/// replacing the link itself.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<DiskState> {
    match write_atomic_inner(path, bytes, None, true) {
        Ok(state) => Ok(state),
        Err(WriteFailure::Conflict(_)) => {
            bail!("{} changed while it was being saved", path.display())
        }
        Err(WriteFailure::Failed(error)) => Err(error),
    }
}

/// Write `bytes` only when `expected` still describes the target.
///
/// The check runs after the temp file is complete but immediately before the
/// rename, so an external edit made while a large note was being written is
/// still observed. `None` means the caller has no stamp: the write proceeds
/// only when the target does not exist. There is deliberately no public
/// unchecked mode here; an explicit overwrite after a conflict warning is
/// expressed by binding `expected` to the disk version the person saw, so a
/// further external edit still conflicts instead of being replaced blind.
/// Files the app itself owns (drafts, session, preferences, backups) use
/// [`write_atomic`], which replaces without a stamp check.
pub fn write_atomic_checked(
    path: &Path,
    bytes: &[u8],
    expected: Option<DiskState>,
) -> Result<DiskState, WriteFailure> {
    write_atomic_inner(path, bytes, expected, false)
}

fn write_atomic_inner(
    path: &Path,
    bytes: &[u8],
    expected: Option<DiskState>,
    overwrite: bool,
) -> Result<DiskState, WriteFailure> {
    if bytes.len() as u64 > MAX_NOTE_BYTES {
        return Err(WriteFailure::Failed(anyhow!(
            "{} is larger than the {} MiB note limit",
            path.display(),
            MAX_NOTE_BYTES / (1024 * 1024)
        )));
    }
    let target = resolve_target(path)?;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(WriteFailure::Failed(anyhow!(
            "could not save {}: the directory {} does not exist",
            target.display(),
            parent.display()
        )));
    }

    let existing_permissions = fs::metadata(&target).ok().map(|meta| meta.permissions());
    let mut temp = TempFile::create(parent)?;

    // A default ACL on the directory can grant access to a newly created file.
    // Clear it before any bytes are written; the old note's ACL is copied back
    // after the content is complete.
    if let Some(file) = temp.file.as_ref() {
        // Best effort: a filesystem without ACL support must still save.
        let _ = clear_inherited_acl(file, &temp.path);
    }

    let file = temp.file.as_mut().expect("a new temp file is open");
    file.write_all(bytes)
        .with_context(|| format!("could not write {}", temp.path.display()))?;
    file.sync_all()
        .with_context(|| format!("could not sync {}", temp.path.display()))?;

    // Capture the stamp of the inode that will be renamed. A post-rename stat
    // is not needed, so a filesystem that briefly hides the new name cannot
    // turn a landed save into a reported failure.
    let written = DiskState::from_metadata_and_digest(&file.metadata()?, digest(bytes))?;

    // Capture the old note's metadata before the rename, but apply it only
    // after the new note is in place. The temp file therefore stays private for
    // its whole life, and an unsupported xattr cannot fail the save.
    let captured = if target.is_file() {
        capture_metadata(&target)
    } else {
        None
    };

    if !overwrite {
        let current = stamp(&target)?;
        let matches = match expected {
            Some(expected) => current.is_some_and(|current| current.same_content(expected)),
            None => current.is_none(),
        };
        if !matches {
            return Err(WriteFailure::Conflict(current));
        }
    }

    drop(temp.file.take());
    // The temp file stays 0600 through the rename. A crash or a failed cleanup
    // can therefore strand only a private file, never a readable copy. On
    // macOS/Linux the replace is a swap: the old target is read back after the
    // exchange and compared, which closes the check-to-rename race. A
    // filesystem without exchange support falls back to a checked rename.
    if overwrite {
        fs::rename(&temp.path, &target).with_context(|| {
            format!(
                "could not replace {} with the completed note",
                target.display()
            )
        })?;
    } else {
        install_temp(&temp.path, &target, expected)?;
    }
    temp.disarm();

    let metadata_ok = captured
        .map(|captured| captured.apply(&target))
        .unwrap_or(true);
    if metadata_ok && let Some(permissions) = existing_permissions {
        let _ = fs::set_permissions(&target, permissions);
    }
    if let Ok(file) = File::open(&target) {
        let _ = file.sync_all();
    }
    sync_parent(parent);
    Ok(written)
}

/// Install a completed temp file over the target without a check-to-rename
/// race on filesystems that support atomic exchange or exclusive rename.
fn install_temp(
    temp: &Path,
    target: &Path,
    expected: Option<DiskState>,
) -> Result<(), WriteFailure> {
    let exists = fs::symlink_metadata(target).is_ok();
    if exists {
        match exchange_paths(temp, target) {
            Ok(()) => {
                // `temp` now names the old target. Its bytes are the version
                // that was actually replaced, not a pre-check guess.
                let observed = match stamp(temp) {
                    Ok(observed) => observed,
                    Err(error) => {
                        return Err(rollback(temp, target, error));
                    }
                };
                let matches = match expected {
                    Some(expected) => {
                        observed.is_some_and(|observed| observed.same_content(expected))
                    }
                    None => observed.is_none(),
                };
                if !matches {
                    if let Err(error) = exchange_paths(temp, target) {
                        preserve_external(temp, target);
                        return Err(WriteFailure::Failed(anyhow!(error).context(
                            "the note changed during the swap and the old version was kept beside it",
                        )));
                    }
                    return Err(WriteFailure::Conflict(observed));
                }
                let _ = fs::remove_file(temp);
                return Ok(());
            }
            Err(error) if unsupported_rename(&error) => {
                // Platforms/filesystems without exchange still need to save.
                // The digest check has already run; this is the documented
                // narrower race on those volumes.
                fs::rename(temp, target).with_context(|| {
                    format!(
                        "could not replace {} with the completed note",
                        target.display()
                    )
                })?;
                return Ok(());
            }
            Err(error) => {
                return Err(WriteFailure::Failed(
                    anyhow!(error).context(format!("could not replace {}", target.display())),
                ));
            }
        }
    }

    match exclusive_rename(temp, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(WriteFailure::Conflict(stamp(target)?))
        }
        Err(error) if unsupported_rename(&error) => {
            // Claim the missing name exclusively, then replace our own empty
            // file. This works on filesystems without RENAME_NOREPLACE.
            match OpenOptions::new().write(true).create_new(true).open(target) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(WriteFailure::Conflict(stamp(target)?));
                }
                Err(error) => {
                    return Err(WriteFailure::Failed(
                        anyhow!(error).context(format!("could not create {}", target.display())),
                    ));
                }
            }
            fs::rename(temp, target).with_context(|| {
                format!(
                    "could not replace {} with the completed note",
                    target.display()
                )
            })?;
            Ok(())
        }
        Err(error) => Err(WriteFailure::Failed(
            anyhow!(error).context(format!("could not replace {}", target.display())),
        )),
    }
}

/// Swap back after a failed read of the replaced target. If even that fails,
/// keep the external bytes in a sidecar so they are never deleted by cleanup.
fn rollback(temp: &Path, target: &Path, error: anyhow::Error) -> WriteFailure {
    if exchange_paths(temp, target).is_err() {
        preserve_external(temp, target);
        return WriteFailure::Failed(
            anyhow!(error)
                .context("the note changed during the swap and the old version was kept beside it"),
        );
    }
    WriteFailure::Failed(anyhow!(error).context("could not verify the replaced note"))
}

fn preserve_external(temp: &Path, target: &Path) {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("note");
    let sidecar = target.with_file_name(format!(".{name}.pilcrow-external"));
    let _ = fs::rename(temp, sidecar);
}

#[cfg(unix)]
fn unsupported_rename(error: &io::Error) -> bool {
    // ENOTSUP and EOPNOTSUPP are the same value on Linux but distinct on
    // macOS, so compare values rather than matching both as patterns.
    error.raw_os_error().is_some_and(|code| {
        code == libc::ENOTSUP
            || code == libc::EOPNOTSUPP
            || code == libc::ENOSYS
            || code == libc::EINVAL
    })
}

#[cfg(not(unix))]
fn unsupported_rename(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
}

#[cfg(target_os = "freebsd")]
fn exchange_paths(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn exchange_paths(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn exchange_paths(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
)))]
fn exchange_paths(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(target_os = "freebsd")]
fn exclusive_rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn exclusive_rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn exclusive_rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
)))]
fn exclusive_rename(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

fn resolve_target(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path)
            .with_context(|| format!("could not resolve the symlink {}", path.display())),
        Ok(_) | Err(_) => Ok(path.to_path_buf()),
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) {
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

/// A temp file that removes itself unless it is explicitly disarmed.
struct TempFile {
    path: PathBuf,
    file: Option<File>,
    armed: bool,
}

impl TempFile {
    fn create(parent: &Path) -> Result<Self> {
        let pid = std::process::id();
        let mut seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();

        for _ in 0..64 {
            seed = seed.wrapping_add(1);
            let path = parent.join(format!(".pilcrow-tmp-{pid}-{seed}"));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);

            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }

            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        armed: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("could not create a temporary file in {}", parent.display())
                    });
                }
            }
        }

        bail!(
            "could not create a unique temporary file in {}",
            parent.display()
        )
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            // Close first: Windows cannot remove a file that is still open.
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The pid a temp name was created by: `.pilcrow-tmp-{pid}-{seed}`.
fn temp_pid(name: &str) -> Option<u32> {
    name.strip_prefix(".pilcrow-tmp-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// Whether a process is still running. Only Unix can answer without a
/// platform crate; everywhere else the caller keeps the age rule.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    // Signal 0 probes existence without delivering anything. EPERM means
    // the process exists but belongs to someone else.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// Remove temporary siblings left by a crash. A temp whose creating
/// process is gone is removed at any age: it can never be renamed into
/// place, and its bytes were never the note (the rename is the commit;
/// drafts and autosaves hold the same text). A temp whose creator might
/// still be writing is removed only once it is older than a day, so a
/// live write from another window is never touched.
pub fn cleanup_stale_temps(directory: &Path) -> usize {
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(".pilcrow-tmp-") {
            continue;
        }
        let orphaned = temp_pid(name).is_some_and(|pid| !process_alive(pid));
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(24 * 60 * 60));
        if (orphaned || stale) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Canonical identity for a note path.
///
/// Existing paths are canonicalised as a whole, so symlinks are followed and
/// `..` is resolved. A path that does not exist yet canonicalises its parent
/// and keeps the chosen basename; this is the identity a Save As will adopt.
pub fn identity(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(canonical) => return Ok(canonical),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("could not resolve {}", path.display()));
        }
    }

    let file_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("{} does not name a file", path.display()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).with_context(|| {
        format!(
            "could not resolve the parent directory {} of {}",
            parent.display(),
            path.display()
        )
    })?;

    Ok(parent.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let base = std::env::temp_dir();
            loop {
                let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!(
                    "pilcrow-file-test-{}-{}-{id}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|duration| duration.as_nanos())
                        .unwrap_or_default()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create {}: {error}", path.display()),
                }
            }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_write_round_trip_and_stamp() {
        let dir = TempDir::new();
        let path = dir.join("note.md");
        assert_eq!(stamp(&path).expect("stat missing file"), None);

        let written = write_atomic(&path, b"# Hello\n\nbody\n").expect("write");
        assert_eq!(written.size, 14);
        assert_eq!(stamp(&path).expect("stat").expect("present"), written);

        let (text, read_stamp) = read(&path).expect("read");
        assert_eq!(text, "# Hello\n\nbody\n");
        assert_eq!(read_stamp, written);
    }

    #[test]
    fn read_strips_a_bom_and_normalises_crlf() {
        let dir = TempDir::new();
        let path = dir.join("windows.md");
        fs::write(&path, b"\xef\xbb\xbffirst\r\nsecond\r\n").expect("fixture");

        let (text, _) = read(&path).expect("read");
        assert_eq!(text, "first\nsecond\n");
    }

    #[test]
    fn read_rejects_invalid_utf8_without_losing_the_file() {
        let dir = TempDir::new();
        let path = dir.join("broken.md");
        let original = b"before\xffafter".to_vec();
        fs::write(&path, &original).expect("fixture");

        let error = read(&path).expect_err("invalid UTF-8 must be an error");
        let message = error.to_string();
        assert!(message.contains("UTF-8"), "{message}");
        assert_eq!(fs::read(&path).expect("file survives"), original);
    }

    #[test]
    fn read_reports_a_missing_file() {
        let dir = TempDir::new();
        let path = dir.join("missing.md");
        let error = read(&path).expect_err("missing file must be an error");
        assert!(error.to_string().contains("could not stat"), "{error}");
    }

    #[test]
    fn write_rejects_a_missing_parent() {
        let dir = TempDir::new();
        let path = dir.join("missing").join("note.md");
        let error = write_atomic(&path, b"hello").expect_err("missing parent");
        assert!(error.to_string().contains("does not exist"), "{error}");
        assert!(!path.exists());
    }

    #[test]
    fn write_does_not_leave_a_temp_file_after_success() {
        let dir = TempDir::new();
        let path = dir.join("note.md");
        write_atomic(&path, b"hello").expect("write");

        let names = fs::read_dir(&dir.0)
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("note.md")]);
    }

    #[test]
    fn write_removes_the_temp_file_after_a_failed_rename() {
        let dir = TempDir::new();
        let target = dir.join("target.md");
        fs::create_dir(&target).expect("directory target");

        let error = write_atomic(&target, b"hello").expect_err("a directory cannot be replaced");
        assert!(!error.to_string().is_empty());

        let names = fs::read_dir(&dir.0)
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("target.md")]);
    }

    #[test]
    fn identity_resolves_existing_relative_paths_and_parents() {
        let existing = Path::new("Cargo.toml");
        assert!(existing.exists(), "tests run from the crate root");
        assert_eq!(
            identity(existing).expect("relative identity"),
            fs::canonicalize(existing).expect("canonical")
        );
        assert_eq!(
            identity(Path::new("../pilcrow-app/Cargo.toml")).expect("dot-dot identity"),
            fs::canonicalize(existing).expect("canonical")
        );
    }

    #[test]
    fn identity_for_a_new_path_resolves_its_parent() {
        let dir = TempDir::new();
        let child = dir.join("child");
        fs::create_dir(&child).expect("child");

        let path = child.join("..").join("new-note.md");
        let expected = fs::canonicalize(&dir.0)
            .expect("canonical parent")
            .join("new-note.md");
        assert_eq!(identity(&path).expect("identity"), expected);
    }

    #[test]
    fn unchanged_detects_an_external_edit_with_the_same_size_and_mtime() {
        let dir = TempDir::new();
        let path = dir.join("same.md");
        fs::write(&path, b"one").expect("fixture");
        let before = stamp(&path).expect("stat").expect("present");

        // Same length, same modification time: only the content digest can
        // tell these revisions apart.
        fs::write(&path, b"two").expect("external edit");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for timestamp");
        file.set_modified(before.mtime).expect("restore mtime");

        assert!(!unchanged(&path, before).expect("compare"));
        assert_eq!(stamp(&path).expect("stat").expect("present").size, 3);
    }

    #[test]
    fn unchanged_reports_a_missing_file_as_changed() {
        let dir = TempDir::new();
        let path = dir.join("gone.md");
        fs::write(&path, b"one").expect("fixture");
        let before = stamp(&path).expect("stat").expect("present");
        fs::remove_file(&path).expect("remove");

        assert!(!unchanged(&path, before).expect("missing is changed"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    #[test]
    fn write_preserves_extended_attributes() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        #[cfg(target_os = "macos")]
        fn set(path: &std::path::Path, name: &CString, value: &[u8]) -> bool {
            unsafe {
                libc::setxattr(
                    CString::new(path.as_os_str().as_bytes()).unwrap().as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                ) == 0
            }
        }
        #[cfg(not(target_os = "macos"))]
        fn set(path: &std::path::Path, name: &CString, value: &[u8]) -> bool {
            unsafe {
                libc::setxattr(
                    CString::new(path.as_os_str().as_bytes()).unwrap().as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                ) == 0
            }
        }

        #[cfg(target_os = "macos")]
        fn get(path: &std::path::Path, name: &CString, buffer: &mut [u8]) -> isize {
            unsafe {
                libc::getxattr(
                    CString::new(path.as_os_str().as_bytes()).unwrap().as_ptr(),
                    name.as_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                    0,
                )
            }
        }
        #[cfg(not(target_os = "macos"))]
        fn get(path: &std::path::Path, name: &CString, buffer: &mut [u8]) -> isize {
            unsafe {
                libc::getxattr(
                    CString::new(path.as_os_str().as_bytes()).unwrap().as_ptr(),
                    name.as_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            }
        }

        let dir = TempDir::new();
        let path = dir.join("tags.md");
        fs::write(&path, b"old").expect("fixture");
        let name = CString::new(if cfg!(target_os = "macos") {
            "com.pilcrow.test"
        } else {
            "user.pilcrow-test"
        })
        .unwrap();
        if !set(&path, &name, b"kept") {
            // Some CI filesystems (tmpfs without user xattrs) do not support
            // this; the save must still work, so skip the assertion there.
            return;
        }

        write_atomic(&path, b"new").expect("write");
        let mut value = [0u8; 8];
        let read = get(&path, &name, &mut value);
        assert_eq!(read, 4, "the extended attribute was dropped");
        assert_eq!(&value[..4], b"kept");
    }

    #[test]
    fn a_checked_write_refuses_after_an_external_edit_and_leaves_the_file_alone() {
        let dir = TempDir::new();
        let path = dir.join("checked.md");
        fs::write(&path, b"one").expect("fixture");
        let expected = stamp(&path).expect("stat").expect("present");

        fs::write(&path, b"theirs").expect("external edit");
        let error = write_atomic_checked(&path, b"ours", Some(expected))
            .expect_err("the checked write must conflict");
        assert!(matches!(error, WriteFailure::Conflict(_)));
        assert_eq!(fs::read(&path).expect("read"), b"theirs");

        let names = fs::read_dir(&dir.0)
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("checked.md")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_note_is_private_by_default() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let path = dir.join("new.md");
        write_atomic(&path, b"private").expect("write");
        let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a new note should not be world-readable");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn write_preserves_a_macos_deny_acl() {
        use std::process::Command;

        let dir = TempDir::new();
        let path = dir.join("acl.md");
        fs::write(&path, b"old").expect("fixture");
        let status = Command::new("chmod")
            .arg("+a")
            .arg("everyone deny write")
            .arg(&path)
            .status()
            .expect("chmod");
        assert!(status.success(), "could not set the ACL fixture");

        write_atomic(&path, b"new").expect("write");
        let output = Command::new("ls")
            .arg("-le")
            .arg(&path)
            .output()
            .expect("ls");
        let listing = String::from_utf8_lossy(&output.stdout);
        assert!(
            listing.contains("deny write"),
            "the deny ACL was dropped: {listing}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_new_temp_does_not_keep_an_inherited_directory_acl() {
        use std::process::Command;

        let dir = TempDir::new();
        let status = Command::new("chmod")
            .arg("+a")
            .arg("everyone allow read,file_inherit,directory_inherit")
            .arg(&dir.0)
            .status()
            .expect("chmod");
        assert!(status.success(), "could not set the inherited ACL");

        let path = dir.join("inherited.md");
        write_atomic(&path, b"private").expect("write");
        let output = Command::new("ls")
            .arg("-le")
            .arg(&path)
            .output()
            .expect("ls");
        let listing = String::from_utf8_lossy(&output.stdout);
        assert!(
            !listing.contains("everyone allow read"),
            "the temp kept the inherited ACL: {listing}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_an_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let path = dir.join("mode.md");
        fs::write(&path, b"old").expect("fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod");

        write_atomic(&path, b"new").expect("write");
        let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }
    #[test]
    fn unchanged_ignores_metadata_only_changes() {
        let dir = TempDir::new();
        let path = dir.join("touched.md");
        fs::write(&path, b"same bytes").expect("fixture");
        let before = stamp(&path).expect("stat").expect("present");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for timestamp");
        file.set_modified(before.mtime + std::time::Duration::from_secs(3600))
            .expect("touch mtime");
        assert!(
            unchanged(&path, before).expect("compare"),
            "a metadata-only touch must not force a conflict"
        );
    }

    #[test]
    fn oversize_sparse_notes_are_rejected_before_reading() {
        let dir = TempDir::new();
        let path = dir.join("huge.md");
        let file = fs::File::create(&path).expect("sparse fixture");
        file.set_len(MAX_NOTE_BYTES + 1).expect("set length");
        let read_error = read(&path).expect_err("read must refuse");
        assert!(
            read_error.to_string().contains("note limit"),
            "{read_error}"
        );
        let stamp_error = stamp(&path).expect_err("stat must refuse");
        assert!(
            stamp_error.to_string().contains("note limit"),
            "{stamp_error}"
        );
    }
    #[test]
    fn stale_private_temps_are_scavenged_but_fresh_ones_survive() {
        let dir = TempDir::new();
        let old = dir.join(".pilcrow-tmp-1-old");
        let fresh = dir.join(".pilcrow-tmp-1-fresh");
        fs::write(&old, b"old").expect("old");
        fs::write(&fresh, b"fresh").expect("fresh");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&old)
            .expect("open old");
        file.set_modified(SystemTime::now() - std::time::Duration::from_secs(48 * 60 * 60))
            .expect("age old");
        assert_eq!(cleanup_stale_temps(&dir.0), 1);
        assert!(!old.exists());
        assert!(fresh.exists());
    }
    #[cfg(unix)]
    #[test]
    fn orphaned_temps_are_scavenged_at_any_age_but_live_ones_survive() {
        // A pid that is certainly dead: spawn a child and reap it.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        child.wait().expect("reap true");

        let dir = TempDir::new();
        let orphaned = dir.0.join(format!(".pilcrow-tmp-{dead_pid}-1"));
        let live = dir.0.join(format!(".pilcrow-tmp-{}-1", std::process::id()));
        fs::write(&orphaned, b"orphan").expect("orphan");
        fs::write(&live, b"live").expect("live");

        assert_eq!(cleanup_stale_temps(&dir.0), 1);
        assert!(!orphaned.exists(), "a dead writer's temp survived");
        assert!(live.exists(), "a live writer's temp was removed");
    }
    #[test]
    fn a_metadata_only_touch_does_not_block_a_checked_write() {
        let dir = TempDir::new();
        let path = dir.join("touched-write.md");
        fs::write(&path, b"one").expect("fixture");
        let expected = stamp(&path).expect("stat").expect("present");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for timestamp");
        file.set_modified(expected.mtime + std::time::Duration::from_secs(3600))
            .expect("touch");
        write_atomic_checked(&path, b"two", Some(expected)).expect("write");
        assert_eq!(fs::read(&path).expect("read"), b"two");
    }
    #[test]
    fn a_write_past_the_note_limit_is_refused_before_touching_the_target() {
        let dir = TempDir::new();
        let path = dir.join("too-big.md");
        fs::write(&path, b"keep").expect("fixture");
        let bytes = vec![b'x'; (MAX_NOTE_BYTES + 1) as usize];
        let error = write_atomic_checked(&path, &bytes, None).expect_err("write must refuse");
        assert!(matches!(error, WriteFailure::Failed(_)));
        assert_eq!(fs::read(&path).expect("read"), b"keep");
    }
    #[test]
    fn read_bounded_reads_up_to_the_limit_and_refuses_past_it() {
        let dir = TempDir::new();
        let path = dir.join("bounded.md");
        fs::write(&path, b"12345").expect("fixture");
        assert_eq!(read_bounded(&path, 5).expect("at limit"), b"12345");
        let error = read_bounded(&path, 4).expect_err("past the limit must refuse");
        assert!(error.to_string().contains("larger than"), "{error:#}");
    }
    #[test]
    fn read_bounded_refuses_a_sparse_file_past_the_limit() {
        let dir = TempDir::new();
        let path = dir.join("sparse.md");
        let file = fs::File::create(&path).expect("fixture");
        file.set_len(MAX_NOTE_BYTES + 1).expect("sparse");
        drop(file);
        assert!(read_bounded(&path, MAX_NOTE_BYTES).is_err());
    }
}
