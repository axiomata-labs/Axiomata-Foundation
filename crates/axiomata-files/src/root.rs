//! Roots and the path guard (decisions E1/E2 of `docs/plans/editor.md` §ED0).
//!
//! A [`Root`] is a directory whose canonical path was fixed when it was
//! built. [`Root::resolve`] turns a relative path into the location to act on
//! and refuses anything that could end up outside that directory:
//!
//! * no absolute path, no `..` — checked on the components before the file
//!   system is consulted at all;
//! * the parent directory must canonicalise inside the root, so a symlinked
//!   directory cannot redirect the path;
//! * the entry itself must be a regular file or not exist yet — never a
//!   directory, FIFO or device (reading a FIFO would block forever);
//! * links follow the root's [`LinkPolicy`].

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{FilesError, io, refused};

/// How a root treats symbolic and hard links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPolicy {
    /// No symlinked file and no hard-linked file at all. The Second-Brain
    /// workspace, which agents write into unattended, keeps this line.
    Strict,
    /// A symlink is followed if its target lies inside the same root; a
    /// hard link is allowed (pnpm's `node_modules` is made of them). For
    /// code the owner opened on purpose: IDE projects, worktrees, grants.
    Contained,
}

/// Turns a root id from the webview (`workspace`, `project:3`, `grant:7`, …)
/// into a [`Root`] — freshly on every call (E8), so a moved project folder,
/// a discarded worktree or a revoked grant is noticed at the next access
/// rather than served from a stale cache. The id grammar belongs to the
/// embedder; this crate only needs the answer.
pub trait RootResolver {
    /// The root named `id`.
    ///
    /// Errors:
    ///     [`FilesError::UnknownRoot`] for an id that names nothing, or
    ///     whatever building the [`Root`] failed with.
    fn root(&self, id: &str) -> Result<Root, FilesError>;
}

/// A guarded place on disk that relative paths are resolved against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Canonical directory every resolved path must stay under.
    dir: PathBuf,
    policy: LinkPolicy,
    /// For a single-file root: the one file name inside `dir` it gives out.
    only: Option<OsString>,
}

/// Where a relative path landed, before any policy on the entry is applied.
pub(crate) struct Resolved {
    /// The directory entry itself: canonical parent + file name. What a
    /// delete removes — for a symlink that is the link, not its target.
    pub entry: PathBuf,
    /// Where content is read from and written to: `entry`, or the canonical
    /// target of an allowed symlink.
    pub target: PathBuf,
    /// Whether `target` exists (as a regular file) right now.
    pub exists: bool,
}

impl Root {
    /// A directory root. `path` is canonicalised here, once; it must be an
    /// existing directory and not the file-system root.
    ///
    /// Errors:
    ///     [`FilesError::Io`] if `path` cannot be canonicalised,
    ///     [`FilesError::Refused`] if it is not a directory or is `/`.
    pub fn dir(path: &Path, policy: LinkPolicy) -> Result<Self, FilesError> {
        let dir = path.canonicalize().map_err(io(path))?;
        if !dir.is_dir() {
            return Err(refused(path, "root is not a directory"));
        }
        if dir.parent().is_none() {
            return Err(refused(path, "the file-system root cannot be a root"));
        }
        Ok(Self {
            dir,
            policy,
            only: None,
        })
    }

    /// A root that gives out exactly one file: what picking a single file in
    /// the open dialog grants (E7). The only relative path it accepts is the
    /// file's own name. Always [`LinkPolicy::Contained`] — the file was
    /// canonicalised here, so it is no symlink, and it may well be a hard
    /// link the owner chose deliberately.
    ///
    /// Errors:
    ///     [`FilesError::Io`] if `path` cannot be canonicalised,
    ///     [`FilesError::Refused`] if it is not a regular file.
    pub fn single_file(path: &Path) -> Result<Self, FilesError> {
        let file = path.canonicalize().map_err(io(path))?;
        if !file.is_file() {
            return Err(refused(path, "not a regular file"));
        }
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
            return Err(refused(path, "has no parent directory"));
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            policy: LinkPolicy::Contained,
            only: Some(name.to_os_string()),
        })
    }

    /// The canonical directory of this root.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// The root's link policy.
    pub fn policy(&self) -> LinkPolicy {
        self.policy
    }

    /// For a single-file root, the relative path of its one file.
    pub fn only_file(&self) -> Option<&Path> {
        self.only.as_deref().map(Path::new)
    }

    /// Resolves `rel` to the location to read from or write to.
    ///
    /// The file need not exist, but its parent directory must. An existing
    /// allowed symlink resolves to its canonical target.
    ///
    /// Errors:
    ///     [`FilesError::Refused`] for any guard failure (see the module docs),
    ///     [`FilesError::Io`] if the parent directory is missing or unreadable.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, FilesError> {
        self.resolve_entry(rel).map(|r| r.target)
    }

    pub(crate) fn resolve_entry(&self, rel: &str) -> Result<Resolved, FilesError> {
        let rel_path = Path::new(rel);
        if rel.trim().is_empty() {
            return Err(refused(rel_path, "empty path"));
        }
        validate_components(rel_path)?;
        if let Some(only) = &self.only {
            let mut normal = rel_path.components().filter(|c| *c != Component::CurDir);
            let is_the_file = matches!(
                (normal.next(), normal.next()),
                (Some(Component::Normal(name)), None) if name == only.as_os_str()
            );
            if !is_the_file {
                return Err(refused(rel_path, "this root gives out a single file only"));
            }
        }

        let full = self.dir.join(rel_path);
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else {
            return Err(refused(rel_path, "path has no file name"));
        };
        let parent_canon = parent.canonicalize().map_err(io(parent))?;
        if !parent_canon.starts_with(&self.dir) {
            return Err(refused(rel_path, "resolves outside the root"));
        }
        let entry = parent_canon.join(name);

        let meta = match fs::symlink_metadata(&entry) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Resolved {
                    target: entry.clone(),
                    entry,
                    exists: false,
                });
            }
            Err(source) => {
                return Err(FilesError::Io {
                    path: entry,
                    source,
                });
            }
        };

        if meta.file_type().is_symlink() {
            return self.resolve_symlink(rel_path, entry);
        }
        check_regular_file(rel_path, &meta, self.policy)?;
        Ok(Resolved {
            target: entry.clone(),
            entry,
            exists: true,
        })
    }

    /// The symlink branch of [`Self::resolve_entry`]: refused outright under
    /// [`LinkPolicy::Strict`], followed under [`LinkPolicy::Contained`] if the
    /// target is an existing regular file inside this root.
    fn resolve_symlink(&self, rel_path: &Path, entry: PathBuf) -> Result<Resolved, FilesError> {
        if self.policy == LinkPolicy::Strict {
            return Err(refused(rel_path, "symlinks are refused"));
        }
        // A dangling link is refused rather than written through: the write
        // would create a file wherever the link happens to point.
        let target = entry
            .canonicalize()
            .map_err(|_| refused(rel_path, "symlink target does not exist"))?;
        if !target.starts_with(&self.dir) {
            return Err(refused(rel_path, "symlink points outside the root"));
        }
        let meta = fs::metadata(&target).map_err(io(&target))?;
        check_regular_file(rel_path, &meta, self.policy)?;
        Ok(Resolved {
            entry,
            target,
            exists: true,
        })
    }
}

/// Rejects an absolute path, a `..` component, or a Windows drive prefix —
/// before the file system is consulted, so nothing is created or walked for
/// a path that was never acceptable.
pub(crate) fn validate_components(rel_path: &Path) -> Result<(), FilesError> {
    for component in rel_path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err(refused(rel_path, "`..` is not allowed")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(refused(rel_path, "path must be relative to its root"));
            }
        }
    }
    Ok(())
}

/// The entry must be a regular file, and under [`LinkPolicy::Strict`] not
/// hard-linked: a hard link shares its content with a path that may lie
/// anywhere.
fn check_regular_file(
    rel_path: &Path,
    meta: &fs::Metadata,
    policy: LinkPolicy,
) -> Result<(), FilesError> {
    if meta.is_dir() {
        return Err(refused(rel_path, "is a directory"));
    }
    if !meta.is_file() {
        return Err(refused(rel_path, "not a regular file"));
    }
    if policy == LinkPolicy::Strict && is_hard_linked(meta) {
        return Err(refused(rel_path, "hard-linked files are refused"));
    }
    Ok(())
}

#[cfg(unix)]
fn is_hard_linked(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.nlink() > 1
}

#[cfg(not(unix))]
fn is_hard_linked(_meta: &fs::Metadata) -> bool {
    false
}
