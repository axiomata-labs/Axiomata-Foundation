//! What the editor knows about languages, in one place (`docs/plans/rust-core.md` S8): for now the line comment ⌘/
//! puts in front of a line, by the file's extension (Axiomata-OS `editor/commands.ts` `commentPrefixFor`). The
//! language table of the language servers (`lsp/languages.ts`) joins it with the LSP step.

/// Line-comment prefixes by file extension (lower case).
const LINE_COMMENTS: [(&str, &str); 27] = [
    ("rs", "//"),
    ("ts", "//"),
    ("tsx", "//"),
    ("js", "//"),
    ("jsx", "//"),
    ("mjs", "//"),
    ("cjs", "//"),
    ("svelte", "//"),
    ("c", "//"),
    ("h", "//"),
    ("cpp", "//"),
    ("hpp", "//"),
    ("java", "//"),
    ("kt", "//"),
    ("go", "//"),
    ("swift", "//"),
    ("scss", "//"),
    ("py", "#"),
    ("sh", "#"),
    ("zsh", "#"),
    ("bash", "#"),
    ("toml", "#"),
    ("yaml", "#"),
    ("yml", "#"),
    ("rb", "#"),
    ("sql", "--"),
    ("lua", "--"),
];

/// The line-comment prefix for a file named `file_name` (`//`, `#`, …), by its extension in any case; `None` when
/// the language has none or is not known.
#[must_use]
pub fn line_comment_for(file_name: &str) -> Option<&'static str> {
    let (_, ext) = file_name.rsplit_once('.')?;
    LINE_COMMENTS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(ext))
        .map(|(_, prefix)| *prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_comment_prefix_by_extension() {
        assert_eq!(line_comment_for("main.rs"), Some("//"));
        assert_eq!(line_comment_for("Cargo.TOML"), Some("#"));
        assert_eq!(line_comment_for("query.sql"), Some("--"));
        assert_eq!(line_comment_for("notes.md"), None);
        assert_eq!(line_comment_for("Makefile"), None);
    }
}
