//! What can go wrong when a caller names a place in the text.

use crate::position::Pos;

/// A position or range a text cannot take. Each one names the place, so a caller's log says where it went wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// The range ends before it starts.
    #[error("the range {start}–{end} ends before it starts")]
    Reversed {
        /// Where the range was said to start.
        start: Pos,
        /// Where it was said to end, before `start`.
        end: Pos,
    },
    /// The line does not exist.
    #[error("line {line} is not in the text, which has {line_count} lines")]
    NoSuchLine {
        /// The line asked for, zero-based.
        line: usize,
        /// How many lines the text has.
        line_count: usize,
    },
    /// The column lies beyond the end of its line.
    #[error("{pos} lies beyond the end of its line, which is {line_len} bytes long")]
    PastLineEnd {
        /// The position asked for.
        pos: Pos,
        /// The line's length in bytes.
        line_len: usize,
    },
    /// The column falls inside a character's UTF-8 bytes.
    #[error("{pos} falls inside a character")]
    NotACharBoundary {
        /// The position asked for.
        pos: Pos,
    },
}
