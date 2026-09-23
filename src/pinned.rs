//! Acting on a resolved path without re-walking it by name.
//!
//! [`Root::resolve`](crate::Root::resolve) proves a path safe at one moment.
//! Handing that path as a string to a later `open`/`rename`/`unlink` would
//! let a process inside the root swap a directory for a symlink in between,
//! and the kernel would follow the swap — the TOCTOU the ED0.1 security audit
//! reported. So every action here first *pins* the parent directory: it opens
//! the root's directory and walks down to the parent one component at a time
//! with `openat(…, O_DIRECTORY | O_NOFOLLOW)`, then acts relative to the
//! file descriptor it ends up holding. A component swapped for a symlink
//! fails the walk instead of redirecting it; one swapped after the walk no
//! longer matters, because nothing below it is looked up by name again.
//!
//! The walk follows the *canonical* path the guard produced, which contains
//! no symlinks by construction — so `O_NOFOLLOW` at every step is correct
//! for both link policies. What stays out of reach is the root's own path
//! above its directory, which lies outside the root and is not something a
//! process confined to the root can change.

use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RawMode};
use rustix::io::Errno;

use crate::error::{FilesError, refused};
use crate::root::{LinkPolicy, Root};

/// Suffix of the temp file a write goes through before its rename.
const TMP_SUFFIX: &str = ".axiomata-tmp";

/// A directory held open by descriptor, plus the name of the entry in it.
struct Pinned<'a> {
    dir: OwnedFd,
    name: &'a OsStr,
}

/// Pins the parent directory of `path`, a canonical path inside `root`.
fn pin<'a>(root: &Root, rel: &str, path: &'a Path) -> Result<Pinned<'a>, FilesError> {
    let inside = path
        .strip_prefix(root.path())
        .map_err(|_| refused(rel, "resolves outside the root"))?;
    let name = inside
        .file_name()
        .ok_or_else(|| refused(rel, "path has no file name"))?;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir =
        rustix::fs::open(root.path(), dir_flags, Mode::empty()).map_err(errno(rel, root.path()))?;
    for component in inside.parent().into_iter().flat_map(Path::components) {
        dir = rustix::fs::openat(
            &dir,
            component.as_os_str(),
            dir_flags | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(errno(rel, path))?;
    }
    Ok(Pinned { dir, name })
}

/// Opens the file at `target` for reading, re-checking on the descriptor
/// what the guard checked by name: a regular file, and under
/// [`LinkPolicy::Strict`] no hard link.
///
/// `O_NONBLOCK` keeps a FIFO swapped in after the guard from blocking the
/// open; it has no effect on a regular file.
pub(crate) fn open_read(root: &Root, rel: &str, target: &Path) -> Result<File, FilesError> {
    let pinned = pin(root, rel, target)?;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(&pinned.dir, pinned.name, flags, Mode::empty())
        .map_err(errno(rel, target))?;
    let stat = rustix::fs::fstat(&fd).map_err(errno(rel, target))?;
    if FileType::from_raw_mode(stat.st_mode as RawMode) != FileType::RegularFile {
        return Err(refused(rel, "not a regular file"));
    }
    if root.policy() == LinkPolicy::Strict && stat.st_nlink > 1 {
        return Err(refused(rel, "hard-linked files are refused"));
    }
    Ok(File::from(fd))
}

/// Writes `bytes` to a fresh temp file beside `target` and renames it over,
/// both relative to the pinned parent. The temp file is created with
/// `O_EXCL | O_NOFOLLOW`, so a leftover or planted entry at its name fails
/// the write instead of being followed or reused. An existing target's
/// permission bits are copied onto the temp file first, so a script keeps
/// its `+x`.
pub(crate) fn replace(
    root: &Root,
    rel: &str,
    target: &Path,
    bytes: &[u8],
) -> Result<(), FilesError> {
    use std::io::Write as _;

    let pinned = pin(root, rel, target)?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(pinned.name);
    tmp_name.push(TMP_SUFFIX);
    let create =
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(&pinned.dir, &tmp_name, create, Mode::from_raw_mode(0o666))
        .map_err(errno(rel, target))?;

    let written = (|| -> Result<(), FilesError> {
        if let Ok(stat) = rustix::fs::statat(&pinned.dir, pinned.name, AtFlags::SYMLINK_NOFOLLOW)
            && FileType::from_raw_mode(stat.st_mode as RawMode) == FileType::RegularFile
        {
            let mode = Mode::from_raw_mode(stat.st_mode as RawMode & 0o7777);
            rustix::fs::fchmod(&fd, mode).map_err(errno(rel, target))?;
        }
        let mut file = File::from(fd);
        file.write_all(bytes).map_err(|source| FilesError::Io {
            path: target.to_path_buf(),
            source,
        })?;
        rustix::fs::renameat(&pinned.dir, &tmp_name, &pinned.dir, pinned.name)
            .map_err(errno(rel, target))
    })();
    if written.is_err() {
        // The temp file is ours: `O_EXCL` guaranteed this call created it.
        let _ = rustix::fs::unlinkat(&pinned.dir, &tmp_name, AtFlags::empty());
    }
    written
}

/// Removes the directory entry at `entry` — for a symlink, the link itself.
pub(crate) fn unlink(root: &Root, rel: &str, entry: &Path) -> Result<(), FilesError> {
    let pinned = pin(root, rel, entry)?;
    rustix::fs::unlinkat(&pinned.dir, pinned.name, AtFlags::empty()).map_err(errno(rel, entry))
}

/// Maps a failed `*at` call onto [`FilesError`]. `ELOOP` and `ENOTDIR` mean
/// a component turned into a symlink (or a file) after the guard ran —
/// refused, not an I/O accident; `ENOENT` means the file vanished.
fn errno<'a>(rel: &'a str, path: &'a Path) -> impl FnOnce(Errno) -> FilesError + 'a {
    move |err| match err {
        Errno::LOOP | Errno::NOTDIR => refused(rel, "changed while being opened"),
        Errno::NOENT => FilesError::NotFound { path: rel.into() },
        other => FilesError::Io {
            path: path.to_path_buf(),
            source: other.into(),
        },
    }
}
