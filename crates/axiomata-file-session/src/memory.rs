//! What an editor remembers across restarts besides the text: the recently opened files and the folds per file. The
//! store is the caller's (the Axiomata app keeps both in its database); here are their shape and bounds.

use serde::{Deserialize, Serialize};

/// How many files "recently opened" keeps.
pub const MAX_RECENT: usize = 12;

/// Files whose folds are kept at most; the least recently written go first.
pub const FOLD_FILES_LIMIT: usize = 100;

/// Folds kept for one file at most — far beyond any real file, a bound on what a damaged store or a bad caller can
/// make the app keep.
pub const MAX_FOLDS_PER_FILE: usize = 10_000;

/// A file by root id and path only: whoever opens it decides again whether it may be read, so a stale entry just
/// fails to open.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileRef {
    /// The root id (`workspace`, `project:<id>`, …).
    pub root: String,
    /// The path inside the root.
    pub rel: String,
}

/// `file` moved to the front of `list`, without duplicates, at most [`MAX_RECENT`].
#[must_use]
pub fn with_recent(list: &[FileRef], file: &FileRef) -> Vec<FileRef> {
    std::iter::once(file.clone())
        .chain(list.iter().filter(|f| *f != file).cloned())
        .take(MAX_RECENT)
        .collect()
}

/// The closed folds of one file, each `[start, end]` in lines (the editor's `FoldState.serialize`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Folds(pub Vec<[u32; 2]>);

impl Folds {
    /// Only the folds that span lines, at most [`MAX_FOLDS_PER_FILE`]. Whether they still fit the text is the
    /// editor's to check when it restores them.
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self(
            self.0
                .into_iter()
                .filter(|[start, end]| start < end)
                .take(MAX_FOLDS_PER_FILE)
                .collect(),
        )
    }

    /// Whether nothing is folded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(root: &str, rel: &str) -> FileRef {
        FileRef {
            root: root.into(),
            rel: rel.into(),
        }
    }

    #[test]
    fn puts_the_file_first_without_duplicating_it() {
        let list = [file("workspace", "a.md"), file("project:1", "b.rs")];
        assert_eq!(
            with_recent(&list, &file("project:1", "b.rs")),
            [file("project:1", "b.rs"), file("workspace", "a.md")]
        );
    }

    #[test]
    fn tells_the_same_path_under_different_roots_apart_and_caps_the_list() {
        let many: Vec<FileRef> = (0..MAX_RECENT)
            .map(|i| file("workspace", &format!("{i}.md")))
            .collect();
        let next = with_recent(&many, &file("project:1", "0.md"));
        assert_eq!(next.len(), MAX_RECENT);
        assert_eq!(next[0], file("project:1", "0.md"));
        assert!(next.contains(&file("workspace", "0.md")));
    }

    #[test]
    fn keeps_only_folds_that_span_lines() {
        let folds = Folds(vec![[1, 3], [4, 4], [9, 2]]).sanitized();
        assert_eq!(folds, Folds(vec![[1, 3]]));
        let many = Folds(vec![[0, 1]; MAX_FOLDS_PER_FILE + 5]).sanitized();
        assert_eq!(many.0.len(), MAX_FOLDS_PER_FILE);
    }
}
