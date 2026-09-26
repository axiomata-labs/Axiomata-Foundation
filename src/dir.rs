//! Directories inside a root, for the file app's tree (`docs/plans/editor.md`,
//! ED4, W6, W13): listing one folder, making a folder, renaming an entry, and
//! counting or removing a whole folder.
//!
//! The same discipline as [`crate::pinned`]: nothing is looked up by name
//! twice. A folder is reached by walking from the root's directory one
//! component at a time with `openat(O_DIRECTORY | O_NOFOLLOW)`, so a
//! component that is (or turns into) a symlink fails the walk instead of
//! leading out of the root; everything then happens relative to the
//! descriptor held. Consequences worth knowing:
//!
//! * a symlinked folder is listed as a link and cannot be opened as a folder
//!   here, even under [`LinkPolicy::Contained`] — the tree shows it, the file
//!   service reads a file through it as before;
//! * a name that is not UTF-8 cannot be named back by the webview, so it is
//!   left out of a listing;
//! * `.gitignore` files are read along the same walk, and an entry they
//!   ignore is *marked*, never hidden (W6: greyed out in the tree).

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RawMode, RenameFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::error::{FilesError, refused};
use crate::pinned::{errno, pin};
use crate::root::{Root, validate_components};

/// The most entries one listing returns; a folder with more says so (`truncated`).
pub const MAX_LISTING: usize = 5_000;
/// The most files [`count_tree`] counts before it stops and says "at least".
pub const MAX_COUNT: usize = 100_000;
/// The deepest [`delete_tree`] goes; deeper than this is refused, not recursed.
const MAX_DEPTH: usize = 256;
/// A `.gitignore` larger than this is read only up to here (the rest of its
/// rules then marks nothing — cosmetic: the mark never decides access).
const MAX_GITIGNORE_BYTES: u64 = 256 * 1024;

/// What an entry is, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Link,
    /// A FIFO, a socket, a device — shown, never opened.
    Other,
}

/// One entry of a listed folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    /// Bytes, for a regular file.
    pub size: Option<u64>,
    /// A `.gitignore` along the way ignores it (W6: shown greyed out).
    pub ignored: bool,
}

/// A listed folder: folders first, then files, each by name ignoring case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub entries: Vec<DirEntry>,
    /// More than [`MAX_LISTING`] entries: the rest is left out.
    pub truncated: bool,
}

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);

/// The folder `rel` (`""` for the root itself) opened by walking to it, plus
/// the canonical-style path it stands for and the `.gitignore` rules met on the way.
struct Walked {
    dir: OwnedFd,
    path: PathBuf,
    ignores: Vec<Gitignore>,
}

fn normal_components(rel: &str) -> Result<Vec<&OsStr>, FilesError> {
    let rel_path = Path::new(rel);
    validate_components(rel_path)?;
    Ok(rel_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect())
}

fn walk_to(root: &Root, rel: &str) -> Result<Walked, FilesError> {
    let names = normal_components(rel)?;
    let mut dir =
        rustix::fs::open(root.path(), DIR_FLAGS, Mode::empty()).map_err(errno(rel, root.path()))?;
    let mut path = root.path().to_path_buf();
    let mut ignores = Vec::new();
    ignores.extend(gitignore_in(&dir, &path));
    for name in names {
        dir = rustix::fs::openat(&dir, name, DIR_FLAGS | OFlags::NOFOLLOW, Mode::empty())
            .map_err(errno(rel, &path))?;
        path.push(name);
        ignores.extend(gitignore_in(&dir, &path));
    }
    Ok(Walked { dir, path, ignores })
}

/// The `.gitignore` directly in `dir` (whose path is `path`), if there is a readable one.
fn gitignore_in(dir: &OwnedFd, path: &Path) -> Option<Gitignore> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(dir, ".gitignore", flags, Mode::empty()).ok()?;
    let stat = rustix::fs::fstat(&fd).ok()?;
    if FileType::from_raw_mode(stat.st_mode as RawMode) != FileType::RegularFile {
        return None;
    }
    let mut text = String::new();
    std::fs::File::from(fd)
        .take(MAX_GITIGNORE_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let mut builder = GitignoreBuilder::new(path);
    for line in text.lines() {
        // A line the matcher cannot parse is skipped, as git skips it.
        let _ = builder.add_line(None, line);
    }
    builder.build().ok()
}

/// Whether `path` (an entry under the walked folder) is ignored: the deepest
/// `.gitignore` that has a say wins, and an ignored parent folder ignores it too.
fn is_ignored(ignores: &[Gitignore], path: &Path, is_dir: bool) -> bool {
    for gi in ignores.iter().rev() {
        match gi.matched_path_or_any_parents(path, is_dir) {
            Match::Ignore(_) => return true,
            Match::Whitelist(_) => return false,
            Match::None => {}
        }
    }
    false
}

fn kind_of(dir: &impl AsFd, name: &OsStr, d_type: FileType) -> (EntryKind, Option<u64>) {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok();
    let file_type = match d_type {
        FileType::Unknown => stat.map(|s| FileType::from_raw_mode(s.st_mode as RawMode)),
        known => Some(known),
    };
    match file_type {
        Some(FileType::RegularFile) => (EntryKind::File, stat.map(|s| s.st_size as u64)),
        Some(FileType::Directory) => (EntryKind::Dir, None),
        Some(FileType::Symlink) => (EntryKind::Link, None),
        _ => (EntryKind::Other, None),
    }
}

/// Lists the folder `rel` of `root` (`""` for the root itself). A root that
/// gives out a single file lists just that file.
///
/// Errors:
///     [`FilesError::Refused`] for a path the guard refuses or a symlinked
///     folder on the way, [`FilesError::NotFound`] if it does not exist.
pub fn list_dir(root: &Root, rel: &str) -> Result<Listing, FilesError> {
    if let Some(only) = root.only_file() {
        if !rel.trim().is_empty() {
            return Err(refused(rel, "a single picked file has no folders"));
        }
        let name = only.to_string_lossy().into_owned();
        let meta = std::fs::symlink_metadata(root.path().join(only)).ok();
        let entry = DirEntry {
            name,
            kind: EntryKind::File,
            size: meta.map(|m| m.len()),
            ignored: false,
        };
        return Ok(Listing {
            entries: vec![entry],
            truncated: false,
        });
    }
    let walked = walk_to(root, rel)?;
    let reader = rustix::fs::Dir::read_from(&walked.dir).map_err(errno(rel, &walked.path))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for item in reader {
        let item = item.map_err(errno(rel, &walked.path))?;
        let raw = item.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        let Ok(name) = std::str::from_utf8(raw) else {
            continue;
        };
        if entries.len() >= MAX_LISTING {
            truncated = true;
            break;
        }
        let (kind, size) = kind_of(&walked.dir, OsStr::new(name), item.file_type());
        let ignored = is_ignored(
            &walked.ignores,
            &walked.path.join(name),
            kind == EntryKind::Dir,
        );
        entries.push(DirEntry {
            name: name.to_owned(),
            kind,
            size,
            ignored,
        });
    }
    entries.sort_by(|a, b| {
        let dir_first = (b.kind == EntryKind::Dir).cmp(&(a.kind == EntryKind::Dir));
        dir_first.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(Listing { entries, truncated })
}

/// Version-control data the tree never changes: an agent's diffs, commits and
/// take-overs (M7.3) stand on it, and nothing here could put it back.
const VCS_DIRS: [&str; 3] = [".git", ".hg", ".jj"];

/// The parent of `rel` pinned, and the entry's name in it — for acting on the entry itself.
fn entry_of<'a>(
    root: &Root,
    rel: &'a str,
    full: &'a Path,
) -> Result<crate::pinned::Pinned<'a>, FilesError> {
    if root.only_file().is_some() {
        return Err(refused(rel, "a single picked file has no folders"));
    }
    let names = normal_components(rel)?;
    if names.is_empty() {
        return Err(refused(rel, "the root itself cannot be changed"));
    }
    if names
        .iter()
        .any(|n| VCS_DIRS.iter().any(|v| *n == OsStr::new(v)))
    {
        return Err(refused(
            rel,
            "version-control data is not changed from the file tree",
        ));
    }
    pin(root, rel, full)
}

/// Makes the folder `rel`; its parent must exist.
///
/// Errors:
///     [`FilesError::Refused`] if the guard refuses the path or something is
///     already there.
pub fn make_dir(root: &Root, rel: &str) -> Result<(), FilesError> {
    let full = root.path().join(rel);
    let pinned = entry_of(root, rel, &full)?;
    rustix::fs::mkdirat(&pinned.dir, pinned.name, Mode::from_raw_mode(0o755)).map_err(|err| {
        match err {
            Errno::EXIST => refused(rel, "something with this name is already there"),
            other => errno(rel, &full)(other),
        }
    })
}

/// Renames (or moves, inside the same root) the entry `from` to `to`. Never
/// overwrites: an existing `to` refuses the rename.
///
/// Errors:
///     [`FilesError::Refused`] if either path is refused or `to` exists,
///     [`FilesError::NotFound`] if `from` does not exist.
pub fn rename_entry(root: &Root, from: &str, to: &str) -> Result<(), FilesError> {
    let from_full = root.path().join(from);
    let to_full = root.path().join(to);
    let source = entry_of(root, from, &from_full)?;
    let target = entry_of(root, to, &to_full)?;
    rustix::fs::renameat_with(
        &source.dir,
        source.name,
        &target.dir,
        target.name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|err| match err {
        Errno::EXIST => refused(to, "something with this name is already there"),
        other => errno(from, &from_full)(other),
    })
}

/// How many entries (files, links, folders) lie under the folder `rel`, for
/// the question before [`delete_tree`] (W13). Stops at [`MAX_COUNT`].
///
/// Errors:
///     as [`list_dir`].
pub fn count_tree(root: &Root, rel: &str) -> Result<usize, FilesError> {
    let walked = walk_to(root, rel)?;
    let mut count = 0;
    count_in(&walked.dir, rel, &walked.path, 0, &mut count)?;
    Ok(count.min(MAX_COUNT))
}

/// Counts into `count`; the whole walk — every level, not just this one — stops at [`MAX_COUNT`].
fn count_in(
    dir: &OwnedFd,
    rel: &str,
    path: &Path,
    depth: usize,
    count: &mut usize,
) -> Result<(), FilesError> {
    if depth > MAX_DEPTH {
        return Err(refused(rel, "folder nesting is too deep"));
    }
    for item in rustix::fs::Dir::read_from(dir).map_err(errno(rel, path))? {
        let item = item.map_err(errno(rel, path))?;
        let raw = item.file_name();
        if raw.to_bytes() == b"." || raw.to_bytes() == b".." {
            continue;
        }
        *count += 1;
        if *count >= MAX_COUNT {
            return Ok(());
        }
        let name = OsStr::from_bytes(raw.to_bytes());
        if kind_of(dir, name, item.file_type()).0 == EntryKind::Dir {
            let child = rustix::fs::openat(dir, name, DIR_FLAGS | OFlags::NOFOLLOW, Mode::empty())
                .map_err(errno(rel, path))?;
            count_in(&child, rel, &path.join(name), depth + 1, count)?;
            if *count >= MAX_COUNT {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Removes the folder `rel` with everything in it (W13 — the view asks
/// first, naming [`count_tree`]'s number). Links are removed, never followed.
/// A plain file is removed too. Never the root itself.
///
/// Errors:
///     [`FilesError::Refused`] for the root, a refused path, or nesting
///     deeper than the limit; [`FilesError::NotFound`] if it does not exist.
pub fn delete_tree(root: &Root, rel: &str) -> Result<(), FilesError> {
    let full = root.path().join(rel);
    let pinned = entry_of(root, rel, &full)?;
    let stat = rustix::fs::statat(&pinned.dir, pinned.name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(errno(rel, &full))?;
    if FileType::from_raw_mode(stat.st_mode as RawMode) != FileType::Directory {
        return rustix::fs::unlinkat(&pinned.dir, pinned.name, AtFlags::empty())
            .map_err(errno(rel, &full));
    }
    let child = rustix::fs::openat(
        &pinned.dir,
        pinned.name,
        DIR_FLAGS | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(errno(rel, &full))?;
    empty_dir(&child, rel, &full, 0)?;
    rustix::fs::unlinkat(&pinned.dir, pinned.name, AtFlags::REMOVEDIR).map_err(|err| match err {
        // Something was written into the folder while it was emptied: it stays, with what is new.
        Errno::NOTEMPTY => refused(rel, "the folder changed while it was being deleted"),
        other => errno(rel, &full)(other),
    })
}

fn empty_dir(dir: &OwnedFd, rel: &str, path: &Path, depth: usize) -> Result<(), FilesError> {
    if depth > MAX_DEPTH {
        return Err(refused(rel, "folder nesting is too deep"));
    }
    // Names first, then removal: a directory stream is not read while it is changed.
    let mut names = Vec::new();
    for item in rustix::fs::Dir::read_from(dir).map_err(errno(rel, path))? {
        let item = item.map_err(errno(rel, path))?;
        let raw = item.file_name().to_bytes();
        if raw != b"." && raw != b".." {
            names.push((raw.to_vec(), item.file_type()));
        }
    }
    for (raw, d_type) in names {
        let name = OsStr::from_bytes(&raw);
        if kind_of(dir, name, d_type).0 == EntryKind::Dir {
            let child = rustix::fs::openat(dir, name, DIR_FLAGS | OFlags::NOFOLLOW, Mode::empty())
                .map_err(errno(rel, path))?;
            empty_dir(&child, rel, &path.join(name), depth + 1)?;
            rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR).map_err(errno(rel, path))?;
        } else {
            rustix::fs::unlinkat(dir, name, AtFlags::empty()).map_err(errno(rel, path))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::root::LinkPolicy;

    fn temp_dir(prefix: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{seq}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project-like root: `src/main.rs`, `README.md`, an ignored `target/`, a `.gitignore`.
    fn project() -> (PathBuf, Root) {
        let dir = temp_dir("axiomata-dir");
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("src/deep/x.rs"), "").unwrap();
        fs::write(dir.join("README.md"), "# Hi\n").unwrap();
        fs::write(dir.join("target/debug/app"), "bin").unwrap();
        fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(dir.join("src/.gitignore"), "!keep.log\n").unwrap();
        fs::write(dir.join("src/keep.log"), "").unwrap();
        fs::write(dir.join("debug.log"), "").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        (dir, root)
    }

    fn names(listing: &Listing) -> Vec<String> {
        listing
            .entries
            .iter()
            .map(|e| format!("{}{}", e.name, if e.ignored { " (ignored)" } else { "" }))
            .collect()
    }

    #[test]
    fn lists_folders_first_and_marks_what_gitignore_ignores() {
        let (dir, root) = project();
        let top = list_dir(&root, "").unwrap();
        assert_eq!(
            names(&top),
            vec![
                "src",
                "target (ignored)",
                ".gitignore",
                "debug.log (ignored)",
                "README.md"
            ]
        );
        let readme = top.entries.iter().find(|e| e.name == "README.md").unwrap();
        assert_eq!((readme.kind, readme.size), (EntryKind::File, Some(5)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_nested_gitignore_can_take_back_what_a_parent_ignores() {
        let (dir, root) = project();
        let src = list_dir(&root, "src").unwrap();
        assert_eq!(
            names(&src),
            vec!["deep", ".gitignore", "keep.log", "main.rs"]
        );
        // Everything under an ignored folder is ignored too.
        assert_eq!(
            names(&list_dir(&root, "target").unwrap()),
            vec!["debug (ignored)"]
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_climbing_out_and_symlinked_folders() {
        let (dir, root) = project();
        assert!(matches!(
            list_dir(&root, "../"),
            Err(FilesError::Refused { .. })
        ));
        assert!(matches!(
            list_dir(&root, "/etc"),
            Err(FilesError::Refused { .. })
        ));
        let outside = temp_dir("axiomata-dir-outside");
        std::os::unix::fs::symlink(&outside, dir.join("out")).unwrap();
        let top = list_dir(&root, "").unwrap();
        assert_eq!(
            top.entries.iter().find(|e| e.name == "out").unwrap().kind,
            EntryKind::Link
        );
        assert!(matches!(
            list_dir(&root, "out"),
            Err(FilesError::Refused { .. })
        ));
        assert!(matches!(
            list_dir(&root, "nope"),
            Err(FilesError::NotFound { .. })
        ));
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn makes_folders_and_renames_without_overwriting() {
        let (dir, root) = project();
        make_dir(&root, "docs").unwrap();
        assert!(dir.join("docs").is_dir());
        assert!(matches!(
            make_dir(&root, "docs"),
            Err(FilesError::Refused { .. })
        ));
        rename_entry(&root, "README.md", "docs/README.md").unwrap();
        assert!(dir.join("docs/README.md").is_file());
        assert!(matches!(
            rename_entry(&root, "src/main.rs", "docs/README.md"),
            Err(FilesError::Refused { .. })
        ));
        assert_eq!(
            fs::read_to_string(dir.join("docs/README.md")).unwrap(),
            "# Hi\n"
        );
        assert!(matches!(
            rename_entry(&root, "gone.md", "x.md"),
            Err(FilesError::NotFound { .. })
        ));
        assert!(matches!(
            rename_entry(&root, "", "x"),
            Err(FilesError::Refused { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn counts_and_deletes_a_whole_folder_but_never_the_root() {
        let (dir, root) = project();
        // src: deep, deep/x.rs, .gitignore, keep.log, main.rs
        assert_eq!(count_tree(&root, "src").unwrap(), 5);
        delete_tree(&root, "src").unwrap();
        assert!(!dir.join("src").exists());
        delete_tree(&root, "README.md").unwrap();
        assert!(!dir.join("README.md").exists());
        assert!(matches!(
            delete_tree(&root, ""),
            Err(FilesError::Refused { .. })
        ));
        assert!(matches!(
            delete_tree(&root, "."),
            Err(FilesError::Refused { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn deletes_a_link_inside_a_folder_without_touching_its_target() {
        let (dir, root) = project();
        let outside = temp_dir("axiomata-dir-keep");
        fs::write(outside.join("precious.txt"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("src/link")).unwrap();
        delete_tree(&root, "src").unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("precious.txt")).unwrap(),
            "keep"
        );
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn a_single_file_root_lists_its_file_and_changes_nothing() {
        let dir = temp_dir("axiomata-dir-single");
        fs::write(dir.join("one.md"), "x").unwrap();
        let root = Root::single_file(&dir.join("one.md")).unwrap();
        assert_eq!(names(&list_dir(&root, "").unwrap()), vec!["one.md"]);
        assert!(list_dir(&root, "sub").is_err());
        assert!(make_dir(&root, "sub").is_err());
        assert!(rename_entry(&root, "one.md", "two.md").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rel_paths_normalize_dot_and_slash_variants() {
        let (dir, root) = project();
        assert_eq!(list_dir(&root, ".").unwrap(), list_dir(&root, "").unwrap());
        assert_eq!(
            list_dir(&root, "./src").unwrap(),
            list_dir(&root, "src").unwrap()
        );
        assert_eq!(
            list_dir(&root, "src/").unwrap(),
            list_dir(&root, "src").unwrap()
        );
        assert_eq!(
            list_dir(&root, "src//deep").unwrap(),
            list_dir(&root, "src/deep").unwrap()
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn renaming_a_folder_into_its_own_subfolder_fails_without_looping() {
        let (dir, root) = project();
        // `src/inner` would live inside `src` itself — refused by the kernel's
        // rename(2) (a single syscall attempt), not an infinite descent.
        let result = rename_entry(&root, "src", "src/inner");
        assert!(result.is_err(), "expected an error, got {result:?}");
        assert!(dir.join("src").is_dir());
        assert!(dir.join("src/main.rs").is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn renames_across_two_different_folders() {
        let (dir, root) = project();
        make_dir(&root, "docs").unwrap();
        make_dir(&root, "archive").unwrap();
        rename_entry(&root, "src/main.rs", "docs/main.rs").unwrap();
        assert!(!dir.join("src/main.rs").exists());
        assert!(dir.join("docs/main.rs").is_file());
        rename_entry(&root, "docs/main.rs", "archive/main.rs").unwrap();
        assert!(!dir.join("docs/main.rs").exists());
        assert_eq!(
            fs::read_to_string(dir.join("archive/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn make_dir_with_a_missing_parent_is_not_found() {
        let (dir, root) = project();
        assert!(matches!(
            make_dir(&root, "missing/sub"),
            Err(FilesError::NotFound { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn make_dir_whose_parent_is_a_file_is_refused() {
        let (dir, root) = project();
        assert!(matches!(
            make_dir(&root, "README.md/sub"),
            Err(FilesError::Refused { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn listing_a_file_path_is_refused() {
        let (dir, root) = project();
        assert!(matches!(
            list_dir(&root, "README.md"),
            Err(FilesError::Refused { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_listing_over_the_cap_is_truncated() {
        let dir = temp_dir("axiomata-dir-big");
        for i in 0..(MAX_LISTING + 1) {
            fs::write(dir.join(format!("f{i:05}.txt")), "").unwrap();
        }
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let listing = list_dir(&root, "").unwrap();
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), MAX_LISTING);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_symlinked_gitignore_is_not_read() {
        let dir = temp_dir("axiomata-dir-gitignore-link");
        fs::write(dir.join("debug.log"), "").unwrap();
        let real = temp_dir("axiomata-dir-gitignore-real");
        fs::write(real.join("real-gitignore"), "*.log\n").unwrap();
        std::os::unix::fs::symlink(real.join("real-gitignore"), dir.join(".gitignore")).unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let listing = list_dir(&root, "").unwrap();
        let log = listing
            .entries
            .iter()
            .find(|e| e.name == "debug.log")
            .unwrap();
        assert!(
            !log.ignored,
            "a symlinked .gitignore must not be followed and read"
        );
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(real);
    }

    #[test]
    fn a_fifo_gitignore_is_not_read_and_does_not_block() {
        let dir = temp_dir("axiomata-dir-gitignore-fifo");
        fs::write(dir.join("debug.log"), "").unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(dir.join(".gitignore"))
            .status()
            .expect("mkfifo must be on PATH for this test");
        assert!(status.success());
        // Would hang forever if a FIFO `.gitignore` were opened for a blocking read.
        let listing = list_dir(&root_of(&dir), "").unwrap();
        let log = listing
            .entries
            .iter()
            .find(|e| e.name == "debug.log")
            .unwrap();
        assert!(
            !log.ignored,
            "a FIFO `.gitignore` must not be read as ignore rules"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// `Root::dir` for a plain [`LinkPolicy::Contained`] directory, for tests
    /// that build their own fixture rather than [`project`].
    fn root_of(dir: &Path) -> Root {
        Root::dir(dir, LinkPolicy::Contained).unwrap()
    }

    #[test]
    fn lists_and_renames_unicode_names() {
        // CJK ideographs and an emoji have no canonical decomposition, unlike
        // Latin diacritics — which macOS's APFS silently re-normalizes (NFD)
        // on the way back from a listing, breaking a byte-equal comparison.
        let dir = temp_dir("axiomata-dir-unicode");
        fs::create_dir_all(dir.join("笔记")).unwrap();
        fs::write(dir.join("笔记/日本語.txt"), "").unwrap();
        fs::write(dir.join("note⭐.md"), "").unwrap();
        let root = root_of(&dir);
        assert_eq!(
            names(&list_dir(&root, "").unwrap()),
            vec!["笔记", "note⭐.md"]
        );
        assert_eq!(names(&list_dir(&root, "笔记").unwrap()), vec!["日本語.txt"]);
        rename_entry(&root, "note⭐.md", "笔记/note⭐.md").unwrap();
        assert!(dir.join("笔记/note⭐.md").is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_tree_on_a_top_level_symlink_removes_only_the_link() {
        let (dir, root) = project();
        let outside = temp_dir("axiomata-dir-link-target");
        fs::write(outside.join("precious.txt"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
        delete_tree(&root, "linked").unwrap();
        assert!(!dir.join("linked").exists());
        assert_eq!(
            fs::read_to_string(outside.join("precious.txt")).unwrap(),
            "keep"
        );
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn count_tree_does_not_descend_into_a_symlinked_folder() {
        let (dir, root) = project();
        let before = count_tree(&root, "src").unwrap();
        let outside = temp_dir("axiomata-dir-count-link");
        fs::create_dir_all(outside.join("nested")).unwrap();
        fs::write(outside.join("a.txt"), "").unwrap();
        fs::write(outside.join("nested/b.txt"), "").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("src/linked")).unwrap();
        let after = count_tree(&root, "src").unwrap();
        assert_eq!(
            after,
            before + 1,
            "the symlink itself counts once; nothing inside its target does"
        );
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn a_strict_root_treats_folders_the_same_as_contained() {
        let dir = temp_dir("axiomata-dir-strict");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/a.txt"), "hi").unwrap();
        let outside = temp_dir("axiomata-dir-strict-outside");
        std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
        let root = Root::dir(&dir, LinkPolicy::Strict).unwrap();
        let top = list_dir(&root, "").unwrap();
        assert_eq!(
            top.entries
                .iter()
                .find(|e| e.name == "linked")
                .unwrap()
                .kind,
            EntryKind::Link
        );
        assert!(matches!(
            list_dir(&root, "linked"),
            Err(FilesError::Refused { .. })
        ));
        assert_eq!(names(&list_dir(&root, "sub").unwrap()), vec!["a.txt"]);
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn version_control_data_is_never_changed_from_the_tree() {
        let (dir, root) = project();
        fs::create_dir_all(dir.join(".git/objects")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        for rel in [".git", ".git/HEAD", "src/.jj", "a/.hg/x"] {
            assert!(
                matches!(delete_tree(&root, rel), Err(FilesError::Refused { .. })),
                "{rel}"
            );
        }
        assert!(matches!(
            rename_entry(&root, ".git", "git-old"),
            Err(FilesError::Refused { .. })
        ));
        assert!(matches!(
            rename_entry(&root, "README.md", ".git/README.md"),
            Err(FilesError::Refused { .. })
        ));
        assert!(matches!(
            make_dir(&root, ".git/new"),
            Err(FilesError::Refused { .. })
        ));
        assert_eq!(
            fs::read_to_string(dir.join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        // Listing it stays possible (the tree shows it with "hidden" on).
        assert!(list_dir(&root, ".git").is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn pin_refuses_a_climbing_path_on_its_own() {
        let (dir, root) = project();
        let climbing = root.path().join("src/../../etc/passwd");
        assert!(matches!(
            crate::pinned::pin(&root, "x", &climbing),
            Err(FilesError::Refused { .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn creating_a_file_never_overwrites() {
        let (dir, root) = project();
        crate::file::create_text(&root, "src/new.rs").unwrap();
        assert_eq!(fs::read_to_string(dir.join("src/new.rs")).unwrap(), "");
        fs::write(dir.join("src/new.rs"), "kept").unwrap();
        assert!(matches!(
            crate::file::create_text(&root, "src/new.rs"),
            Err(FilesError::Refused { .. })
        ));
        assert!(matches!(
            crate::file::create_text(&root, "src"),
            Err(FilesError::Refused { .. })
        ));
        assert_eq!(fs::read_to_string(dir.join("src/new.rs")).unwrap(), "kept");
        let _ = fs::remove_dir_all(dir);
    }
}
