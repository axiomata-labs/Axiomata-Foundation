//! Positions, ranges and the selection (Axiomata-OS `editor/position.ts`).

use std::fmt;

/// A place in the text: a zero-based line and a zero-based column in **UTF-8 bytes** of that line, so
/// `&line[..col]` is always the text before it. A position a text hands out lies on a character boundary.
///
/// Positions order by line, then column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    /// The line, zero-based.
    pub line: usize,
    /// The byte offset into the line.
    pub col: usize,
}

impl Pos {
    /// The position at `line`, `col`.
    #[must_use]
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// A stretch of text between two positions. [`Range::new`] puts them in order; a range built by hand with `start`
/// after `end` is refused by the text it is used on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Range {
    /// Where it starts.
    pub start: Pos,
    /// Where it ends, exclusive.
    pub end: Pos,
}

impl Range {
    /// The range between `a` and `b`, whichever comes first.
    #[must_use]
    pub fn new(a: Pos, b: Pos) -> Self {
        if a <= b {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    /// The empty range at `at`.
    #[must_use]
    pub const fn at(at: Pos) -> Self {
        Self { start: at, end: at }
    }

    /// Whether it holds no text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// The selection: `anchor` is where it started, `head` where the cursor is. They are equal for a plain cursor.
/// Unlike a [`Range`], the head may come first — a selection made with ⇧← grows to the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Selection {
    /// Where the selection started.
    pub anchor: Pos,
    /// Where the cursor is.
    pub head: Pos,
}

impl Selection {
    /// A plain cursor at `at`.
    #[must_use]
    pub const fn cursor(at: Pos) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    /// The selection as a range, whichever way round it was made.
    #[must_use]
    pub fn range(&self) -> Range {
        Range::new(self.anchor, self.head)
    }

    /// Whether it is a plain cursor, selecting nothing.
    #[must_use]
    pub fn is_cursor(&self) -> bool {
        self.anchor == self.head
    }
}

/// `col` moved into `line`: past the end to the end, inside a character back to where it starts.
pub(crate) fn floor_char_boundary(line: &str, col: usize) -> usize {
    let mut at = col.min(line.len());
    while !line.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_by_line_then_column() {
        assert!(Pos::new(0, 9) < Pos::new(1, 0));
        assert!(Pos::new(2, 3) < Pos::new(2, 4));
        let r = Range::new(Pos::new(3, 1), Pos::new(1, 5));
        assert_eq!((r.start, r.end), (Pos::new(1, 5), Pos::new(3, 1)));
        assert!(Range::at(Pos::new(1, 1)).is_empty());
    }

    #[test]
    fn a_selection_made_leftwards_is_the_same_range() {
        let sel = Selection {
            anchor: Pos::new(0, 5),
            head: Pos::new(0, 2),
        };
        assert_eq!(sel.range(), Range::new(Pos::new(0, 2), Pos::new(0, 5)));
        assert!(!sel.is_cursor());
        assert!(Selection::cursor(Pos::new(4, 4)).is_cursor());
    }
}
