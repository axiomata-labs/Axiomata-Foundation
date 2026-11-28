//! Replacing a document's text with a new version as small changes (Axiomata-OS `editor/textEdits.ts`, ED6.5): a
//! formatter's output, or a language server's edits (formatting, a rename), become the lines that actually differ —
//! so undo, folds and the cursor keep their place instead of the whole text being swapped.
//!
//! Positions are lines and UTF-8 byte columns, as everywhere in the crate; text is `\n`-separated (the store's form).
//! A server counts in UTF-16 units: converting its positions is the language client's job, before they reach here.

use crate::diff::{DiffKind, diff};
use crate::document::{Change, end_after};
use crate::error::EditError;
use crate::position::{Pos, Range};

/// One of a server's text edits: a range of the text it was given, and what goes there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextEdit {
    /// What it replaces, in the coordinates of the text the server was given.
    pub range: Range,
    /// What goes there; line breaks of any kind become `\n`.
    pub text: String,
}

/// The byte offset of `p` in `text`: a line past the end is the text's end, a column past its line's end is that
/// end (servers do send both), a column inside a character is refused.
fn offset_in(text: &str, starts: &[usize], p: Pos) -> Result<usize, EditError> {
    let Some(&start) = starts.get(p.line) else {
        return Ok(text.len());
    };
    let end = starts.get(p.line + 1).map_or(text.len(), |next| next - 1);
    let at = (start + p.col).min(end);
    if text.is_char_boundary(at) {
        Ok(at)
    } else {
        Err(EditError::NotACharBoundary { pos: p })
    }
}

/// Where each line of `text` starts, as byte offsets.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

/// `text` with `edits` applied — all in the coordinates of `text`, as the protocol states them; two insertions at
/// one place keep the order they were given in.
///
/// # Errors
/// An edit's range is reversed or names a column inside a character, or two edits overlap. Then nothing is applied
/// (the caller keeps the text as it was: no formatting rather than a wrong one).
pub fn apply_text_edits(text: &str, edits: &[TextEdit]) -> Result<String, EditError> {
    let starts = line_starts(text);
    let mut ordered = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        if edit.range.end < edit.range.start {
            return Err(EditError::Reversed {
                start: edit.range.start,
                end: edit.range.end,
            });
        }
        let from = offset_in(text, &starts, edit.range.start)?;
        let to = offset_in(text, &starts, edit.range.end)?;
        ordered.push((from, to, index, edit));
    }
    ordered.sort_by_key(|&(from, to, index, _)| (from, to, index));
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    let mut previous: Option<&TextEdit> = None;
    for (from, to, _, edit) in ordered {
        if from < done {
            let first = previous.map_or(edit.range, |p| p.range);
            return Err(EditError::Overlapping {
                first_start: first.start,
                first_end: first.end,
                second_start: edit.range.start,
                second_end: edit.range.end,
            });
        }
        out.push_str(&text[done..from]);
        out.push_str(&edit.text.replace("\r\n", "\n").replace('\r', "\n"));
        done = to;
        previous = Some(edit);
    }
    out.push_str(&text[done..]);
    Ok(out)
}

/// The changes that turn `before` into `after`, one per run of differing lines, each in the coordinates of `before`
/// (they do not overlap).
#[must_use]
pub fn text_changes(before: &str, after: &str) -> Vec<Change> {
    let old: Vec<&str> = before.split('\n').collect();
    let new: Vec<&str> = after.split('\n').collect();
    let mut changes = Vec::new();
    // Where the script stands in the old and the new lines.
    let (mut in_old, mut in_new) = (0, 0);
    let mut ops = diff(&old, &new).into_iter().peekable();
    while let Some(op) = ops.next() {
        if op.kind == DiffKind::Equal {
            in_old += op.count;
            in_new += op.count;
            continue;
        }
        // A run of removals and insertions (in either order) is one change.
        let (mut removed, mut added) = (0, 0);
        let mut take = |op: crate::diff::DiffOp| {
            if op.kind == DiffKind::Delete {
                removed += op.count;
            } else {
                added += op.count;
            }
        };
        take(op);
        while let Some(next) = ops.next_if(|next| next.kind != DiffKind::Equal) {
            take(next);
        }
        changes.push(lines_change(
            &old,
            in_old,
            removed,
            &new[in_new..in_new + added],
        ));
        in_old += removed;
        in_new += added;
    }
    changes
}

/// Replacing `count` lines of `lines` from `start` with `inserted`, as a change in character positions.
fn lines_change(lines: &[&str], start: usize, count: usize, inserted: &[&str]) -> Change {
    let last = lines.len() - 1;
    let end_of = |line: usize| Pos::new(line, lines[line].len());
    let change = |start: Pos, end: Pos, text: String| Change {
        range: Range { start, end },
        text,
    };
    if count > 0 && !inserted.is_empty() {
        let end = start + count - 1;
        return change(Pos::new(start, 0), end_of(end), inserted.join("\n"));
    }
    if count > 0 {
        // Whole lines go, with one of the line breaks around them.
        if start + count <= last {
            return change(
                Pos::new(start, 0),
                Pos::new(start + count, 0),
                String::new(),
            );
        }
        if start > 0 {
            return change(end_of(start - 1), end_of(last), String::new());
        }
        return change(Pos::new(0, 0), end_of(last), String::new());
    }
    if start <= last {
        return change(
            Pos::new(start, 0),
            Pos::new(start, 0),
            format!("{}\n", inserted.join("\n")),
        );
    }
    change(
        end_of(last),
        end_of(last),
        format!("\n{}", inserted.join("\n")),
    )
}

/// Where `p` is once `changes` (non-overlapping, in the coordinates `p` is in) are applied. A position inside a
/// changed stretch keeps its line within it where it can; its column is clamped by the caller's text.
#[must_use]
pub fn map_position(p: Pos, changes: &[Change]) -> Pos {
    let (mut line, mut col) = (p.line, p.col);
    // In the order they are applied (`last_first`): each in coordinates the ones after it have not touched, and ties at
    // one place as they really land.
    let ordered = last_first(changes);
    for c in ordered.iter().filter(|c| c.range.start <= p) {
        let new_end = end_after(c.range.start, &c.text);
        if c.range.end <= Pos::new(line, col) {
            if line == c.range.end.line {
                col = new_end.col + (col - c.range.end.col);
                line = new_end.line;
            } else {
                line = line + new_end.line - c.range.end.line;
            }
        } else {
            // Inside the change: the same line of it (as far as it reaches), same column.
            line = line.min(new_end.line);
        }
    }
    Pos::new(line, col)
}

/// `changes` (non-overlapping, all in one text's coordinates) in the order they can be applied one after another:
/// last in the text first, so each is still where it was given. At one place a replacement goes before the
/// insertions there (an insertion at a range's start lands in front of it), and insertions go in reverse of their
/// order, so the one given first ends up first — as the protocol has it, and as `text_changes` needs when it puts an
/// insertion before an empty line's end and one after it.
#[must_use]
pub fn last_first(changes: &[Change]) -> Vec<Change> {
    let mut ordered: Vec<(usize, &Change)> = changes.iter().enumerate().collect();
    ordered.sort_by(|(ia, a), (ib, b)| {
        b.range
            .start
            .cmp(&a.range.start)
            .then_with(|| a.range.is_empty().cmp(&b.range.is_empty()))
            .then_with(|| ib.cmp(ia))
    });
    ordered.into_iter().map(|(_, c)| c.clone()).collect()
}

/// A formatter's output as the text the store holds: line endings to `\n`, and one final line break dropped — the
/// store keeps that as the file's shape, never as an empty last line.
#[must_use]
pub fn store_body(output: &str) -> String {
    let unix = output.replace("\r\n", "\n").replace('\r', "\n");
    match unix.strip_suffix('\n') {
        Some(body) => body.to_owned(),
        None => unix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pos(line: usize, col: usize) -> Pos {
        Pos::new(line, col)
    }

    fn edit(start: Pos, end: Pos, text: &str) -> TextEdit {
        TextEdit {
            range: Range { start, end },
            text: text.into(),
        }
    }

    /// `changes` applied in the order the `replace_text` command takes them.
    fn applied(text: &str, changes: &[Change]) -> String {
        let mut rope = crate::Rope::new(text);
        for c in &last_first(changes) {
            rope = rope.replace(c.range, &c.text).unwrap().rope;
        }
        rope.text().to_owned()
    }

    #[test]
    fn applies_edits_given_in_the_original_texts_coordinates_in_any_order() {
        let edits = [
            edit(pos(1, 5), pos(1, 6), " = "),
            edit(pos(0, 5), pos(0, 6), " = "),
            edit(pos(2, 0), pos(2, 0), "\n"),
        ];
        assert_eq!(
            apply_text_edits("let a=1;\nlet b=2;", &edits).unwrap(),
            "let a = 1;\nlet b = 2;\n"
        );
    }

    #[test]
    fn keeps_two_insertions_at_one_place_in_their_order_and_turns_crlf_into_lf() {
        let edits = [
            edit(pos(0, 1), pos(0, 1), "x"),
            edit(pos(0, 1), pos(0, 1), "y\r\n"),
        ];
        assert_eq!(apply_text_edits("ab", &edits).unwrap(), "axy\nb");
    }

    #[test]
    fn clamps_past_the_end_but_refuses_a_cut_character_or_an_overlap() {
        // Past its line's end, and a line past the text: clamped (S4).
        assert_eq!(
            apply_text_edits(
                "ab\ncd",
                &[
                    edit(pos(0, 9), pos(0, 9), "!"),
                    edit(pos(7, 0), pos(7, 0), "?")
                ]
            )
            .unwrap(),
            "ab!\ncd?"
        );
        // "é" is two bytes: column 1 is inside it.
        assert!(matches!(
            apply_text_edits("é", &[edit(pos(0, 1), pos(0, 1), "x")]),
            Err(EditError::NotACharBoundary { .. })
        ));
        assert!(matches!(
            apply_text_edits(
                "abcdef",
                &[
                    edit(pos(0, 1), pos(0, 4), "x"),
                    edit(pos(0, 3), pos(0, 5), "y")
                ]
            ),
            Err(EditError::Overlapping { .. })
        ));
        assert!(matches!(
            apply_text_edits("abc", &[edit(pos(0, 2), pos(0, 1), "x")]),
            Err(EditError::Reversed { .. })
        ));
        // Touching is no overlap; an insertion at a replaced range's start goes before it.
        assert_eq!(
            apply_text_edits(
                "abcd",
                &[
                    edit(pos(0, 1), pos(0, 3), "X"),
                    edit(pos(0, 3), pos(0, 4), "Y"),
                    edit(pos(0, 1), pos(0, 1), "<")
                ]
            )
            .unwrap(),
            "a<XY"
        );
    }

    #[test]
    fn text_changes_turn_one_text_into_the_other() {
        for (before, after) in [
            ("a\nb\nc", "a\nB\nc"),
            ("a\nb\nc", "a\nc"),
            ("a\nb\nc", "a\nb"),
            ("a\nb", "x\na\nb"),
            ("a\nb", "a\nb\nc\nd"),
            ("a", ""),
            ("", "new"),
            ("fn x(){\n1\n}", "fn x() {\n    1\n}"),
            ("grüße\nä", "grüße\nö\nä"),
        ] {
            assert_eq!(
                applied(before, &text_changes(before, after)),
                after,
                "{before:?} → {after:?}"
            );
        }
    }

    #[test]
    fn touches_only_the_lines_that_differ() {
        assert_eq!(
            text_changes("a\nb\nc", "a\nB\nc"),
            [Change {
                range: Range {
                    start: pos(1, 0),
                    end: pos(1, 1)
                },
                text: "B".into()
            }]
        );
        assert_eq!(text_changes("same", "same"), []);
    }

    #[test]
    fn moves_a_position_by_the_lines_added_or_removed_above_it() {
        let changes = text_changes("x\na\nb\ncursor here", "import y\n\nx\na b\ncursor here");
        assert_eq!(map_position(pos(3, 7), &changes), pos(4, 7));
    }

    #[test]
    fn maps_a_position_through_changes_at_one_place_as_they_land() {
        let change = |start: Pos, end: Pos, text: &str| Change {
            range: Range { start, end },
            text: text.into(),
        };
        // "abcdef": an insertion at the start of a replaced range lands in front of it, "X\nYdef".
        let changes = [
            change(pos(0, 0), pos(0, 0), "X\n"),
            change(pos(0, 0), pos(0, 3), "Y"),
        ];
        assert_eq!(map_position(pos(0, 5), &changes), pos(1, 3));
        let other_way = [changes[1].clone(), changes[0].clone()];
        assert_eq!(map_position(pos(0, 5), &other_way), pos(1, 3));
        // Two insertions at one place land in the order given: "ab\n…", the position after them on line 1.
        let two = [
            change(pos(0, 0), pos(0, 0), "a"),
            change(pos(0, 0), pos(0, 0), "b\n"),
        ];
        assert_eq!(map_position(pos(0, 5), &two), pos(1, 5));
    }

    #[test]
    fn a_diff_past_its_bound_still_gives_the_new_text() {
        // Every other of 2400 lines changed: 2400 differences, past the bound of 1000 — one block, still right.
        let lines = |shout: fn(usize) -> bool| {
            (0..2400)
                .map(|i| {
                    if shout(i) {
                        format!("LINE {i}")
                    } else {
                        format!("line {i}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let before = lines(|_| false);
        let after = lines(|i| i % 2 == 1);
        let changes = text_changes(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(applied(&before, &changes), after);
    }

    #[test]
    fn drops_the_formatters_final_line_break() {
        assert_eq!(store_body("a\r\nb\n"), "a\nb");
        assert_eq!(store_body("a\rb"), "a\nb");
    }

    fn text() -> impl Strategy<Value = String> {
        proptest::collection::vec(prop_oneof!["a", "b", "ä", "\n", "xy", "🙂", "日"], 0..24)
            .prop_map(|parts| parts.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// S6: the changes from two texts, applied to the first, give the second.
        #[test]
        fn changes_from_two_texts_give_the_second(before in text(), after in text()) {
            prop_assert_eq!(applied(&before, &text_changes(&before, &after)), after);
        }
    }
}
