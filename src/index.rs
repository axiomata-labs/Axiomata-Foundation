//! The file names of a root, for quick open (`docs/plans/editor.md`, ED4, W8,
//! W14): one walk that returns every file's path relative to the root, for
//! the view to match against as the owner types. Names only — nothing is
//! read — so the walk goes by path like the memory router's, not through
//! pinned descriptors: links are not followed, which keeps it inside the root.
//!
//! What is left out, like the tree hides it: dotfiles and dot-folders (`.git`
//! with them), `node_modules` and `target`, whatever a `.gitignore` ignores
//! (with or without a git repository), and names that are not UTF-8.
//!
//! Only ignore files inside the root count: a `.gitignore` above it (a project
//! that is a subfolder of a bigger repository) is not read, the way an editor
//! searching a folder does not consult its parents' rules either. That can
//! list a name the outer repository ignores — the owner's own file, opened
//! through the same guard as any other.
//!
//! The walk stops at [`MAX_INDEX`] files or [`MAX_VISITED`] entries, whichever
//! comes first, so a root full of ignored bulk (an agent's worktree, say) cannot
//! keep it busy for long; either way the index says `truncated`.

use std::ops::ControlFlow;

use ignore::WalkBuilder;
use serde::Serialize;

use crate::error::FilesError;
use crate::root::Root;

/// The most files one index holds; a root with more says so (`truncated`).
pub const MAX_INDEX: usize = 50_000;

/// The most entries (files, folders, ignored ones too) one walk looks at.
pub const MAX_VISITED: usize = 500_000;

/// Folders never walked into, gitignored or not: they are big and never what one looks for.
const SKIPPED_DIRS: [&str; 2] = ["node_modules", "target"];

/// Every file of a root by its relative path, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileIndex {
    pub files: Vec<String>,
    /// More than [`MAX_INDEX`] files: the rest is left out.
    pub truncated: bool,
}

/// Walks `root` for its files. A root that gives out a single file has just that one.
///
/// Errors:
///     none today — an unreadable entry is skipped rather than failing the walk;
///     the `Result` keeps room for a root that cannot be walked at all.
pub fn index_files(root: &Root) -> Result<FileIndex, FilesError> {
    if let Some(only) = root.only_file() {
        let files = only
            .to_str()
            .map(|name| vec![name.to_owned()])
            .unwrap_or_default();
        return Ok(FileIndex {
            files,
            truncated: false,
        });
    }
    let mut files = Vec::new();
    // Breaking off at a file past the limit — or at the visit limit — is what `truncated` means.
    let truncated = walk_files(root, |rel| {
        if files.len() >= MAX_INDEX {
            return ControlFlow::Break(());
        }
        files.push(rel.to_owned());
        ControlFlow::Continue(())
    });
    files.sort();
    Ok(FileIndex { files, truncated })
}

/// Walks the files of a directory root by the rules in the module docs,
/// calling `visit` with each file's `/`-separated relative path, in walk
/// order, until it breaks. Returns whether the walk stopped early — `visit`
/// broke off, or [`MAX_VISITED`] entries were looked at.
///
/// There is deliberately no way to pass the walk `ignore` overrides: an
/// override *whitelists* what it matches, past the hidden-file and
/// `.gitignore` rules — a search's include glob `.env` would have read the
/// secrets this walk exists to leave out (ED5.7 security review). A caller
/// narrows the walk by filtering what `visit` is given instead.
pub(crate) fn walk_files(root: &Root, mut visit: impl FnMut(&str) -> ControlFlow<()>) -> bool {
    let mut builder = WalkBuilder::new(root.path());
    builder
        .hidden(true)
        .follow_links(false)
        .parents(false)
        .git_global(false)
        .require_git(false)
        .filter_entry(|entry| {
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            !(is_dir && entry.depth() > 0 && SKIPPED_DIRS.iter().any(|d| entry.file_name() == *d))
        });
    for (visited, entry) in builder.build().enumerate() {
        if visited >= MAX_VISITED {
            return true;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root.path()) else {
            continue;
        };
        let Some(rel) = rel.to_str() else { continue };
        if visit(&rel.replace(std::path::MAIN_SEPARATOR, "/")).is_break() {
            return true;
        }
    }
    false
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

    #[test]
    fn lists_files_leaving_out_hidden_ignored_and_build_folders() {
        let dir = temp_dir("axiomata-index");
        for (path, text) in [
            ("src/main.rs", ""),
            ("src/deep/x.rs", ""),
            ("README.md", ""),
            (".env", "SECRET=1"),
            (".git/HEAD", ""),
            ("node_modules/pkg/index.js", ""),
            ("target/debug/app", ""),
            ("out/build.log", ""),
            (".gitignore", "out/\n"),
        ] {
            let full = dir.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let index = index_files(&root).unwrap();
        assert_eq!(
            index.files,
            vec!["README.md", "src/deep/x.rs", "src/main.rs"]
        );
        assert!(!index.truncated);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn does_not_follow_a_link_out_of_the_root() {
        let dir = temp_dir("axiomata-index-link");
        let outside = temp_dir("axiomata-index-outside");
        fs::write(outside.join("secret.md"), "x").unwrap();
        fs::write(dir.join("a.md"), "").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("elsewhere")).unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        assert_eq!(index_files(&root).unwrap().files, vec!["a.md"]);
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn a_single_file_root_has_its_file() {
        let dir = temp_dir("axiomata-index-single");
        fs::write(dir.join("one.md"), "").unwrap();
        let root = Root::single_file(&dir.join("one.md")).unwrap();
        assert_eq!(index_files(&root).unwrap().files, vec!["one.md"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn honours_a_gitignore_in_a_nested_subfolder() {
        let dir = temp_dir("axiomata-index-nested-gitignore");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/.gitignore"), "*.tmp\n").unwrap();
        fs::write(dir.join("src/keep.rs"), "").unwrap();
        fs::write(dir.join("src/scratch.tmp"), "").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let index = index_files(&root).unwrap();
        // The nested `.gitignore` itself is a dotfile and so is left out too (module docs).
        assert_eq!(index.files, vec!["src/keep.rs"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn honours_a_gitignore_negation() {
        let dir = temp_dir("axiomata-index-negation");
        fs::write(dir.join(".gitignore"), "*.log\n!keep.log\n").unwrap();
        fs::write(dir.join("debug.log"), "").unwrap();
        fs::write(dir.join("keep.log"), "").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let index = index_files(&root).unwrap();
        assert_eq!(index.files, vec!["keep.log"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_root_without_its_own_gitignore_inside_a_wider_git_repo_ignores_nothing_of_its_own() {
        // The `.git` and a `.gitignore` sit one level above the walked root; the
        // root itself (`child/`) has no `.gitignore` of its own. `parents(false)`
        // must keep the walk from reaching upward for that ancestor's rules.
        let outer = temp_dir("axiomata-index-outer-repo");
        fs::write(outer.join(".gitignore"), "*.log\n").unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&outer)
            .status()
            .unwrap();
        let dir = outer.join("child");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("debug.log"), "").unwrap();
        fs::write(dir.join("keep.rs"), "").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let index = index_files(&root).unwrap();
        // Documenting the actual behaviour: the ancestor's `.gitignore` is not
        // reached from a root rooted below it, so nothing of it is filtered here.
        assert_eq!(index.files, vec!["debug.log", "keep.rs"]);
        let _ = fs::remove_dir_all(outer);
    }

    #[test]
    fn a_symlinked_file_inside_the_root_is_not_listed() {
        // Links are never followed (module docs): a symlink's own `file_type()`
        // is a symlink, not a regular file, so it is skipped like a directory
        // would be — even when its target is a plain file inside the same root.
        let dir = temp_dir("axiomata-index-file-link");
        fs::write(dir.join("real.md"), "content").unwrap();
        std::os::unix::fs::symlink(dir.join("real.md"), dir.join("alias.md")).unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        assert_eq!(index_files(&root).unwrap().files, vec!["real.md"]);
        let _ = fs::remove_dir_all(dir);
    }
}
