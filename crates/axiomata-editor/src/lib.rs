//! The text core of a code editor (Axiomata-OS `docs/plans/rust-core.md`, E4 steps 1 and 2, R1–R7 and S1–S8): the
//! text as a rope of lines, positions, the file's own shape (line ending, final line break, indentation), an open
//! document with its selections and undo history, and characters as a person sees them (step 1); the editing
//! [`commands`] and several cursors ([`multicursor`]), a formatter's or server's changes as small edits
//! ([`text_changes`], [`apply_text_edits`] over the line [`diff`]), snippets and their tab stops ([`snippet`]), and an
//! agent's trail ([`trail_of`]) (step 2). No UI: Lohe draws it on Schimmer.
//!
//! It is a port of the editor engine in Axiomata-OS (`apps/axiomata/src/editor/`), case by case, with three changes
//! the move to Rust brings:
//!
//! * **Positions count UTF-8 bytes** ([`Pos::col`]), never UTF-16 units, and always lie on a character boundary: a
//!   range that would cut a character in two is refused ([`EditError::NotACharBoundary`]). Where a person sees
//!   characters — moving, deleting, selecting a word — the editor steps by grapheme instead ([`text`]).
//! * **A versioned text is a cheap value.** A [`Rope`] shares its nodes with the version it came from; a clone is a
//!   snapshot another thread may read (search, syntax, saving) while the document goes on changing.
//! * **Nothing panics on what a caller passes.** A range outside the text is an [`EditError`], and an edit that fails
//!   halfway changes nothing.

#![forbid(unsafe_code)]

pub mod commands;
mod detect;
pub mod diff;
mod document;
mod error;
mod language;
pub mod multicursor;
mod position;
mod rope;
pub mod snippet;
#[cfg(test)]
mod testing;
pub mod text;
mod text_edits;
mod trail;

pub use commands::{Command, CommandContext, completion_targets, run};
pub use detect::{
    FileShape, Indent, IndentKind, LineEnding, body_for_store, detect_indent, detect_shape,
};
pub use detect::{join_for_save, split_lines};
pub use document::{
    Change, Document, EditKind, Follower, MERGE_WINDOW, Millis, TextChange, end_after,
};
pub use error::EditError;
pub use language::line_comment_for;
pub use position::{Pos, Range, Selection};
pub use rope::{Replaced, Rope};
pub use snippet::SnippetSession;
pub use text_edits::{
    TextEdit, apply_text_edits, last_first, map_position, store_body, text_changes,
};
pub use trail::{Trail, trail_of};
