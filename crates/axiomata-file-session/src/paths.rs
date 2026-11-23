//! Where a path goes when the file tree renames or deletes an entry — a file itself or a folder above it.

/// Where `rel` is after `from` was renamed to `to`: `to` itself, or the same place under it when `from` was a folder
/// above `rel`; `None` when the rename does not concern `rel`.
#[must_use]
pub fn renamed_path(rel: &str, from: &str, to: &str) -> Option<String> {
    if rel == from {
        return Some(to.to_string());
    }
    rel.strip_prefix(from)
        .filter(|rest| rest.starts_with('/'))
        .map(|rest| format!("{to}{rest}"))
}

/// Whether `rel` is `gone` or lies under it.
#[must_use]
pub fn is_under(rel: &str, gone: &str) -> bool {
    rel == gone
        || rel
            .strip_prefix(gone)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_takes_the_file_itself_and_everything_under_a_folder() {
        assert_eq!(
            renamed_path("a.md", "a.md", "b.md").as_deref(),
            Some("b.md")
        );
        assert_eq!(
            renamed_path("docs/x/a.md", "docs", "notes").as_deref(),
            Some("notes/x/a.md")
        );
        assert_eq!(renamed_path("docs2/a.md", "docs", "notes"), None);
        assert_eq!(renamed_path("other.md", "a.md", "b.md"), None);
    }

    #[test]
    fn a_path_lies_under_itself_and_its_folders_only() {
        assert!(is_under("a.md", "a.md"));
        assert!(is_under("docs/a.md", "docs"));
        assert!(!is_under("docs2/a.md", "docs"));
        assert!(!is_under("doc", "docs"));
    }
}
