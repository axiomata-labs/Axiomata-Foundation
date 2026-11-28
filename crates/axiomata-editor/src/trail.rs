//! An agent's trail in an open file (Axiomata-OS `editor/trail.ts`, `UX6c`, G8): when the file changes on disk and
//! the editor takes the new text, the lines that came out different glow for a moment, and the agent's flag stands
//! at the last of them. Honest, not acted: the lines are a diff of the text before and after — nothing is typed out.

use crate::diff::{DiffKind, diff};

/// What changed between two texts, in the lines of the new one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trail {
    /// The new text's lines that are new or different (zero-based, ascending, each once).
    pub lines: Vec<usize>,
    /// Where the flag stands: the last changed line — or, after a pure deletion, the line where text went missing.
    pub last: Option<usize>,
}

/// The new lines of `after` against `before`; a deletion marks the line that now stands in its place.
#[must_use]
pub fn trail_of(before: &str, after: &str) -> Trail {
    let a: Vec<&str> = before.split('\n').collect();
    let b: Vec<&str> = after.split('\n').collect();
    let mut lines = Vec::new();
    let mut at = 0;
    let mut last = None;
    for op in diff(&a, &b) {
        match op.kind {
            DiffKind::Equal => at += op.count,
            DiffKind::Insert => {
                lines.extend(at..at + op.count);
                at += op.count;
                last = Some(at - 1);
            }
            DiffKind::Delete => {
                // Removed lines leave no line of their own: the gap is marked where it is now.
                let gap = at.min(b.len() - 1);
                if lines.last() != Some(&gap) {
                    lines.push(gap);
                }
                last = Some(gap);
            }
        }
    }
    lines.sort_unstable();
    lines.dedup();
    Trail { lines, last }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trail(lines: &[usize], last: Option<usize>) -> Trail {
        Trail {
            lines: lines.to_vec(),
            last,
        }
    }

    #[test]
    fn marks_a_changed_line_and_puts_the_flag_there() {
        assert_eq!(trail_of("a\nb\nc", "a\nB\nc"), trail(&[1], Some(1)));
    }

    #[test]
    fn marks_inserted_lines_the_flag_at_the_last_of_the_last_block() {
        assert_eq!(
            trail_of("a\nd", "a\nb\nc\nd\ne"),
            trail(&[1, 2, 4], Some(4))
        );
    }

    #[test]
    fn marks_the_line_that_stands_where_lines_were_removed() {
        assert_eq!(trail_of("a\nb\nc\nd", "a\nd"), trail(&[1], Some(1)));
    }

    #[test]
    fn keeps_a_removal_at_the_end_inside_the_text() {
        assert_eq!(trail_of("a\nb\nc", "a\nb"), trail(&[1], Some(1)));
    }

    #[test]
    fn marks_lines_inserted_or_removed_at_the_very_start() {
        assert_eq!(trail_of("b\nc", "a\nb\nc"), trail(&[0], Some(0)));
        assert_eq!(trail_of("a\nb\nc", "b\nc"), trail(&[0], Some(0)));
    }

    #[test]
    fn marks_a_whole_text_that_came_into_an_empty_file() {
        assert_eq!(trail_of("", "a\nb"), trail(&[0, 1], Some(1)));
    }

    #[test]
    fn puts_the_flag_on_the_new_line_of_a_replacement() {
        assert_eq!(trail_of("a\nold\nc", "a\nnew one\nc"), trail(&[1], Some(1)));
    }

    #[test]
    fn finds_nothing_when_nothing_changed() {
        assert_eq!(trail_of("same\ntext", "same\ntext"), trail(&[], None));
    }
}
