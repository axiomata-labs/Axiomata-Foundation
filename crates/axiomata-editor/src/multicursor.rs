//! Several cursors at once (Axiomata-OS `editor/multicursor.ts`, ED5, T6): running a command at every one of them,
//! and the ways to get more — ⌥-click, ⌥⌘↑/↓, ⌘D / ⌘U, ⇧⌘L, a column drawn with ⌥-drag.
//!
//! **One command, every cursor**: the cursors are visited from the last in the text to the first, each time with the
//! document's main selection set to that cursor, so the single-cursor command runs unchanged. Every other cursor —
//! done or still waiting — follows the text through each change the visit makes (a [`Follower`] of the document),
//! and all of it is one undo step: typing keeps merging into the typing step before it, as with one cursor. Cursors
//! that end up touching are merged. A visit that fails stops the run; what it changed so far stays as that one step
//! (`docs/plans/rust-core.md` S5).
//!
//! [`Follower`]: crate::Follower

use std::collections::HashSet;

use crate::document::{Document, EditKind, TextChange};
use crate::error::EditError;
use crate::position::{Pos, Selection};
use crate::rope::Rope;
use crate::text::{col_for_display_column, display_column, word_at};

/// Up or down, for the commands that add cursors or move lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    /// Towards the start of the text.
    Up,
    /// Towards its end.
    Down,
}

/// Every cursor, the main one first.
#[must_use]
pub fn all_selections(doc: &Document) -> Vec<Selection> {
    std::iter::once(doc.selection())
        .chain(doc.extra().iter().copied())
        .collect()
}

/// Whether there is more than one cursor.
#[must_use]
pub fn has_multiple(doc: &Document) -> bool {
    !doc.extra().is_empty()
}

/// Where `p` is after `change`: before it unmoved, inside it at its new end, after it shifted.
#[must_use]
pub fn map_pos(p: Pos, change: &TextChange) -> Pos {
    if p <= change.start {
        p
    } else if p < change.old_end {
        change.new_end
    } else if p.line == change.old_end.line {
        Pos::new(
            change.new_end.line,
            change.new_end.col + (p.col - change.old_end.col),
        )
    } else {
        Pos::new(p.line + change.new_end.line - change.old_end.line, p.col)
    }
}

fn map_selection(sel: Selection, change: &TextChange) -> Selection {
    Selection {
        anchor: map_pos(sel.anchor, change),
        head: map_pos(sel.head, change),
    }
}

/// `list` merged: in text order, every pair that overlaps or meets merged into one, each with the indices of the
/// selections it grew from.
fn merged_with_sources(list: &[Selection]) -> Vec<(Selection, Vec<usize>)> {
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by_key(|&i| list[i].range().start);
    let mut out: Vec<(Selection, Vec<usize>)> = Vec::new();
    for i in order {
        let sel = list[i];
        let r = sel.range();
        if let Some((prev, sources)) = out.last_mut() {
            let p = prev.range();
            let touches =
                r.start < p.end || (r.start == p.end && (sel.is_cursor() || prev.is_cursor()));
            if touches {
                let end = r.end.max(p.end);
                // A merged selection keeps the direction of the one it grew from.
                let forward = prev.anchor <= prev.head;
                *prev = if forward {
                    Selection {
                        anchor: p.start,
                        head: end,
                    }
                } else {
                    Selection {
                        anchor: end,
                        head: p.start,
                    }
                };
                sources.push(i);
                continue;
            }
        }
        out.push((sel, vec![i]));
    }
    out
}

/// `list` in text order with every pair that overlaps or meets merged into one, and where the selection at
/// `main_index` ended up.
#[must_use]
pub fn merge_selections(list: &[Selection], main_index: usize) -> (Vec<Selection>, usize) {
    let merged = merged_with_sources(list);
    let main = merged
        .iter()
        .position(|(_, sources)| sources.contains(&main_index))
        .unwrap_or(0);
    (merged.into_iter().map(|(sel, _)| sel).collect(), main)
}

/// Puts `list` back as the document's cursors, `main` being the main one, the others in order.
fn put_back(doc: &mut Document, list: &[Selection], main: usize, continues: bool) {
    let Some(&main_sel) = list.get(main) else {
        return;
    };
    let others: Vec<Selection> = list
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != main)
        .map(|(_, s)| *s)
        .collect();
    doc.set_selections(main_sel, &others, continues);
}

/// Runs `work` once per cursor (see the module header), with the index of the cursor in [`all_selections`]. `kind`
/// is the edit kind the work makes, so a typed character with several cursors still merges into the typing step
/// before it; `None` for work that only moves. `once_per_line` skips a cursor on a line an earlier visit already
/// took (outdent, comment): it just follows.
///
/// # Errors
/// The first error a visit returns. The run stops there; the cursors are put back where the text took them, and
/// what changed so far is one undo step.
pub fn each_cursor(
    doc: &mut Document,
    kind: Option<EditKind>,
    once_per_line: bool,
    mut work: impl FnMut(&mut Document, usize) -> Result<(), EditError>,
) -> Result<(), EditError> {
    let start = all_selections(doc);
    // Every cursor keeps its own goal column for ↑/↓.
    let mut goals: Vec<Option<usize>> = std::iter::once(doc.goal_column())
        .chain((0..start.len() - 1).map(|i| doc.extra_goals().get(i).copied().flatten()))
        .collect();
    // Visited last-in-text first, so a change never moves a cursor still to come.
    let mut order: Vec<usize> = (0..start.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(start[i].range().start));
    let mut current = start;
    let follower = doc.follow();
    let grouped = !doc.in_undo_group();
    if grouped {
        doc.begin_undo_group(kind);
    }
    let revision = doc.revision();
    let mut taken_lines = HashSet::new();
    let mut outcome = Ok(());
    for i in order {
        let r = current[i].range();
        if once_per_line {
            if (r.start.line..=r.end.line).any(|l| taken_lines.contains(&l)) {
                continue;
            }
            taken_lines.extend(r.start.line..=r.end.line);
        }
        doc.focus_cursor(current[i], goals[i]);
        outcome = work(doc, i);
        for change in doc.changes_since(&follower) {
            for sel in &mut current {
                *sel = map_selection(*sel, &change);
            }
        }
        if outcome.is_err() {
            break;
        }
        current[i] = doc.selection();
        goals[i] = doc.goal_column();
    }
    doc.unfollow(follower);
    if grouped {
        doc.end_undo_group();
    }
    let merged = merged_with_sources(&current);
    let main = merged
        .iter()
        .position(|(_, sources)| sources.contains(&0))
        .unwrap_or(0);
    let list: Vec<Selection> = merged.iter().map(|(sel, _)| *sel).collect();
    // Only a visit that changed the text has a step of its own to put the cursors on.
    put_back(
        doc,
        &list,
        main,
        kind.is_some() && doc.revision() != revision,
    );
    // Goal columns go with the cursors that came through unmerged; a merged one starts afresh.
    let list_goals: Vec<Option<usize>> = merged
        .iter()
        .map(|(_, sources)| match sources.as_slice() {
            [only] => goals[*only],
            _ => None,
        })
        .collect();
    doc.set_goal_column(list_goals[main]);
    doc.set_extra_goals(
        list_goals
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != main)
            .map(|(_, g)| *g)
            .collect(),
    );
    outcome
}

/// The lines a selection takes: every line it touches, except a last line it merely reaches at column 0.
pub(crate) fn selected_lines(sel: Selection) -> (usize, usize) {
    let r = sel.range();
    let last = if r.end.line > r.start.line && r.end.col == 0 {
        r.end.line - 1
    } else {
        r.end.line
    };
    (r.start.line, last)
}

/// Runs a command that moves whole lines (⌥↑/↓) once per block of cursors on touching lines — the block as one
/// selection, as if those lines were selected — and puts each cursor back at its place in the block wherever the
/// command took it. Blocks are visited from the bottom up, so a block's change never reaches the lines of one still
/// to come (they are not touching, so at least one line lies between them).
///
/// # Errors
/// The first error `work` returns; the run stops there, as [`each_cursor`] does.
pub fn each_line_block(
    doc: &mut Document,
    mut work: impl FnMut(&mut Document) -> Result<(), EditError>,
) -> Result<(), EditError> {
    let all = all_selections(doc);
    let mut order: Vec<(usize, usize, usize)> = all
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (first, last) = selected_lines(*s);
            (i, first, last)
        })
        .collect();
    order.sort_by_key(|&(_, first, _)| first);
    let mut blocks: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for (i, first, last) in order {
        match blocks.last_mut() {
            Some((_, block_last, members)) if first <= *block_last + 1 => {
                *block_last = (*block_last).max(last);
                members.push(i);
            }
            _ => blocks.push((first, last, vec![i])),
        }
    }
    let mut result = all.clone();
    let grouped = !doc.in_undo_group();
    if grouped {
        doc.begin_undo_group(None);
    }
    let revision = doc.revision();
    let mut outcome = Ok(());
    for (first, last, members) in blocks.iter().rev() {
        let end = match doc.rope().line(*last) {
            Ok(line) => Pos::new(*last, line.len()),
            Err(err) => {
                outcome = Err(err);
                break;
            }
        };
        doc.focus_cursor(
            Selection {
                anchor: Pos::new(*first, 0),
                head: end,
            },
            None,
        );
        if let Err(err) = work(doc) {
            outcome = Err(err);
            break;
        }
        let moved_to = doc.selection().range().start.line;
        for &i in members {
            let s = all[i];
            let shift = |p: Pos| Pos::new((p.line + moved_to).saturating_sub(*first), p.col);
            result[i] = Selection {
                anchor: shift(s.anchor),
                head: shift(s.head),
            };
        }
    }
    if grouped {
        doc.end_undo_group();
    }
    let (list, main) = merge_selections(&result, 0);
    put_back(doc, &list, main, doc.revision() != revision);
    outcome
}

/// ⌥-click: a cursor at `at` added (or, if one is there, taken away).
pub fn toggle_cursor(doc: &mut Document, at: Pos) {
    let all = all_selections(doc);
    if all.len() > 1
        && let Some(hit) = all.iter().position(|sel| sel.is_cursor() && sel.head == at)
    {
        let rest: Vec<Selection> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != hit)
            .map(|(_, s)| *s)
            .collect();
        put_back(doc, &rest, rest.len() - 1, false);
        return;
    }
    // The newest cursor is the main one; the old main joins the others at the end.
    let mut with_new = vec![Selection::cursor(at)];
    with_new.extend_from_slice(&all);
    let (list, main) = merge_selections(&with_new, 0);
    put_back(doc, &list, main, false);
    reorder_newest_last(doc, &all);
}

/// Keeps the other cursors in the order they were added, the old main one last — merging sorts by position; ⌘U
/// needs the adding order.
fn reorder_newest_last(doc: &mut Document, before: &[Selection]) {
    // Older extras first, then the old main, as they were.
    let rank: Vec<Selection> = before[1..]
        .iter()
        .copied()
        .chain(std::iter::once(before[0]))
        .collect();
    let rank_of = |s: &Selection| rank.iter().position(|r| r == s);
    let mut extra = doc.extra().to_vec();
    // A cursor new to the list (`None`) goes first, as the TS original's rank of -1 put it.
    extra.sort_by_key(|s| rank_of(s).map_or(0, |at| at + 1));
    let main = doc.selection();
    doc.set_selections(main, &extra, false);
}

/// ⌥⌘↑ / ⌥⌘↓: one more cursor a line above (below) the topmost (bottommost), at the display column of the cursor
/// this began from (the oldest) — so a column passing a short line comes back out at its own column.
pub fn add_cursor_vertical(doc: &mut Document, dir: Dir, tab_size: usize) {
    let all = all_selections(doc);
    let edge = match dir {
        Dir::Up => all.iter().min_by_key(|s| s.head),
        Dir::Down => all.iter().max_by_key(|s| s.head),
    };
    let Some(edge) = edge else {
        return;
    };
    let line = match dir {
        Dir::Up => match edge.head.line.checked_sub(1) {
            Some(line) => line,
            None => return,
        },
        Dir::Down => edge.head.line + 1,
    };
    let origin = doc.extra().first().copied().unwrap_or(doc.selection());
    let (Ok(origin_text), Ok(target)) = (doc.rope().line(origin.head.line), doc.rope().line(line))
    else {
        return;
    };
    let column = display_column(origin_text, origin.head.col, tab_size);
    let at = Pos::new(line, col_for_display_column(target, column, tab_size));
    let mut with_new = vec![Selection::cursor(at)];
    with_new.extend_from_slice(&all);
    let (list, main) = merge_selections(&with_new, 0);
    put_back(doc, &list, main, false);
    reorder_newest_last(doc, &all);
}

/// The word around a cursor, as ⌘D and ⇧⌘L take it when nothing is selected.
fn word_selection(rope: &Rope, at: Pos) -> Option<Selection> {
    let w = word_at(rope.line(at.line).ok()?, at.col);
    (w.end > w.start).then(|| Selection {
        anchor: Pos::new(at.line, w.start),
        head: Pos::new(at.line, w.end),
    })
}

/// Every place `needle` occurs in `rope`, case and all, in text order (no overlaps).
#[must_use]
pub fn occurrences(rope: &Rope, needle: &str) -> Vec<Selection> {
    if needle.is_empty() {
        return Vec::new();
    }
    rope.text()
        .match_indices(needle)
        .map(|(at, _)| Selection {
            anchor: rope.pos_at(at),
            head: rope.pos_at(at + needle.len()),
        })
        .collect()
}

/// ⌘D: with a bare cursor, selects the word it is in; with a selection, adds the next place its text occurs
/// (wrapping around), which becomes the main cursor. Nothing when every occurrence is already selected.
pub fn add_next_occurrence(doc: &mut Document) {
    let main = doc.selection();
    if main.is_cursor() {
        if let Some(word) = word_selection(doc.rope(), main.head) {
            let extra = doc.extra().to_vec();
            doc.set_selections(word, &extra, false);
        }
        return;
    }
    let Ok(needle) = doc.rope().slice(main.range()) else {
        return;
    };
    let taken: HashSet<Pos> = all_selections(doc)
        .iter()
        .map(|s| s.range().start)
        .collect();
    let found = occurrences(doc.rope(), &needle);
    let after = main.range().end;
    let free = |s: &&Selection| !taken.contains(&s.anchor);
    let next = found
        .iter()
        .filter(free)
        .find(|s| s.anchor >= after)
        .or_else(|| found.iter().find(free));
    let Some(&next) = next else {
        return;
    };
    let mut extra = doc.extra().to_vec();
    extra.push(main);
    doc.set_selections(next, &extra, false);
}

/// ⌘U: takes back the cursor added last; the one before it becomes the main one.
pub fn remove_last_cursor(doc: &mut Document) {
    let mut extra = doc.extra().to_vec();
    if let Some(main) = extra.pop() {
        doc.set_selections(main, &extra, false);
    }
}

/// ⇧⌘L: every occurrence of the selection's text (of the word at the cursor) selected.
pub fn select_all_occurrences(doc: &mut Document) {
    let sel = doc.selection();
    let main = if sel.is_cursor() {
        word_selection(doc.rope(), sel.head)
    } else {
        Some(sel)
    };
    let Some(main) = main else {
        return;
    };
    let Ok(needle) = doc.rope().slice(main.range()) else {
        return;
    };
    let found = occurrences(doc.rope(), &needle);
    if found.is_empty() {
        return;
    }
    let start = main.range().start;
    let index = found.iter().position(|s| s.anchor == start).unwrap_or(0);
    put_back(doc, &found, index, false);
}

/// Esc with several cursors: only the main one stays; `false` when there was only one.
pub fn single_cursor(doc: &mut Document) -> bool {
    if doc.extra().is_empty() {
        return false;
    }
    let main = doc.selection();
    doc.set_selection(main, false);
    true
}

/// ⌥-drag: a column from `anchor` to `head` — one selection per line between them, spanning the same display
/// columns (shorter lines get a cursor at their end) — added to the cursors in `keep` (those there before the drag).
/// The line the drag is on holds the main cursor.
pub fn column_selection(
    doc: &mut Document,
    anchor: Pos,
    head: Pos,
    tab_size: usize,
    keep: &[Selection],
) {
    let rope = doc.rope();
    let (Ok(anchor_text), Ok(head_text)) = (rope.line(anchor.line), rope.line(head.line)) else {
        return;
    };
    let from_col = display_column(anchor_text, anchor.col, tab_size);
    let to_col = display_column(head_text, head.col, tab_size);
    let lines: Box<dyn Iterator<Item = usize>> = if head.line >= anchor.line {
        Box::new(anchor.line..=head.line)
    } else {
        Box::new((head.line..=anchor.line).rev())
    };
    let mut list = Vec::new();
    for line in lines {
        let Ok(text) = rope.line(line) else {
            return;
        };
        list.push(Selection {
            anchor: Pos::new(line, col_for_display_column(text, from_col, tab_size)),
            head: Pos::new(line, col_for_display_column(text, to_col, tab_size)),
        });
    }
    let Some(on_the_drag) = list.pop() else {
        return;
    };
    let mut ordered = vec![on_the_drag];
    ordered.extend(list);
    ordered.extend_from_slice(keep);
    let (merged, main) = merge_selections(&ordered, 0);
    put_back(doc, &merged, main, false);
}

/// The text of every selection, in text order — for copying several at once.
#[must_use]
pub fn selection_texts(doc: &Document) -> Vec<String> {
    let mut ranges: Vec<_> = all_selections(doc).iter().map(Selection::range).collect();
    ranges.sort_by_key(|r| r.start);
    ranges
        .into_iter()
        .map(|r| doc.rope().slice(r).unwrap_or_default())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{BackUnit, ClipboardText, Command, Motion, copy_text, cut, paste, run};
    use crate::testing::{FOUR, ctx, cursor, doc_with, p, show_all};

    fn run_all(marked: &str, cmds: &[Command]) -> String {
        let mut doc = doc_with(marked, None);
        for cmd in cmds {
            run(&mut doc, cmd, &ctx()).unwrap();
        }
        show_all(&doc)
    }

    fn insert(text: &str) -> Command {
        Command::insert(text)
    }

    fn at(now: u64) -> crate::commands::CommandContext<'static> {
        crate::commands::CommandContext {
            now: crate::Millis(now),
            ..ctx()
        }
    }

    mod random {
        use super::*;
        use crate::commands::ForwardUnit;
        use proptest::prelude::*;

        fn text() -> impl Strategy<Value = String> {
            proptest::collection::vec(prop_oneof!["a", "b", "ä", " ", "\n", "  x"], 0..30)
                .prop_map(|p| p.concat())
        }

        fn command() -> impl Strategy<Value = Command> {
            prop_oneof![
                prop_oneof!["q", "ö", "\n", "zz"].prop_map(|t: String| Command::insert(&t)),
                Just(Command::Newline),
                Just(Command::DeleteBackward(BackUnit::Char)),
                Just(Command::DeleteBackward(BackUnit::Word)),
                Just(Command::DeleteForward(ForwardUnit::Char)),
                Just(Command::Indent),
                Just(Command::Outdent),
                Just(Command::ToggleComment),
                Just(Command::MoveLines(Dir::Down)),
                Just(Command::DuplicateLines(Dir::Up)),
                Just(Command::Move {
                    motion: Motion::CharLeft,
                    extend: true
                }),
            ]
        }

        /// Cursors from raw numbers: each picks a line, and a character boundary on it.
        fn cursors_in(doc: &Document, raw: &[(usize, usize)]) -> Vec<Selection> {
            raw.iter()
                .map(|&(line, col)| {
                    let line = line % doc.rope().line_count();
                    let text = doc.rope().line(line).unwrap();
                    let bounds: Vec<usize> = text
                        .char_indices()
                        .map(|(i, _)| i)
                        .chain([text.len()])
                        .collect();
                    Selection::cursor(Pos::new(line, bounds[col % bounds.len()]))
                })
                .collect()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            /// S6: editing with several cursors and undoing everything gives back the text it started from.
            #[test]
            fn editing_at_several_cursors_and_undoing_all_gives_the_start_back(
                start in text(),
                raw in proptest::collection::vec((0usize..8, 0usize..12), 1..5),
                cmds in proptest::collection::vec(command(), 1..12),
            ) {
                let mut doc = Document::new(&start, FOUR);
                let opened = doc.rope().text().to_owned();
                let cursors = cursors_in(&doc, &raw);
                doc.set_selections(cursors[0], &cursors[1..], false);
                for (step, cmd) in cmds.iter().enumerate() {
                    // Far apart in time, so no two commands merge into one step.
                    run(&mut doc, cmd, &at(step as u64 * 10_000)).unwrap();
                    prop_assert_eq!(doc.rope().check_invariants(), Ok(()));
                }
                while doc.undo().unwrap() {}
                prop_assert_eq!(doc.rope().text(), opened);
            }
        }
    }

    #[test]
    fn a_command_runs_at_every_cursor() {
        let mv = |motion, extend| Command::Move { motion, extend };
        let cases: [(&str, Vec<Command>, &str); 10] = [
            ("a|b\nc|d", vec![insert("X")], "aX|b\ncX|d"),
            ("a|b a|b", vec![insert("XY")], "aXY|b aXY|b"),
            (
                "ab|\ncd|",
                vec![Command::DeleteBackward(BackUnit::Char)],
                "a|\nc|",
            ),
            ("|ab\n|cd", vec![mv(Motion::LineEnd, false)], "ab|\ncd|"),
            ("|ab\n|cd", vec![mv(Motion::LineEnd, true)], "^ab|\n^cd|"),
            ("a|b\nc|d", vec![Command::Newline], "a\n|b\nc\n|d"),
            // Two cursors that meet become one.
            ("a|b|c", vec![Command::DeleteBackward(BackUnit::Char)], "|c"),
            ("x^ab|x ^ab|x", vec![insert("Q")], "xQ|x Q|x"),
            // A whole-line command takes a line once, however many cursors are on it.
            ("    a|b|\n    c|d", vec![Command::Outdent], "a|b|\nc|d"),
            // ⇥ at bare cursors puts indentation at each, to the next tab stop.
            ("a|b|\nc|d", vec![Command::Indent], "a   |b  |\nc   |d"),
        ];
        for (before, cmds, after) in cases {
            assert_eq!(run_all(before, &cmds), after, "{before:?} {cmds:?}");
        }
    }

    #[test]
    fn undoes_the_edit_at_every_cursor_at_once_and_puts_them_all_back() {
        let mut doc = doc_with("a|b\nc|d", None);
        run(&mut doc, &insert("X"), &at(0)).unwrap();
        run(&mut doc, &insert("Y"), &at(10)).unwrap();
        assert_eq!(show_all(&doc), "aXY|b\ncXY|d");
        // Typing with several cursors stays one step, as with one.
        run(&mut doc, &Command::Undo, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "a|b\nc|d");
        run(&mut doc, &Command::Redo, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "aXY|b\ncXY|d");
    }

    #[test]
    fn alt_click_adds_a_cursor_and_on_a_cursor_takes_it_away() {
        let mut doc = doc_with("|abc", None);
        toggle_cursor(&mut doc, p(0, 2));
        assert_eq!(show_all(&doc), "|ab|c");
        toggle_cursor(&mut doc, p(0, 2));
        assert_eq!(show_all(&doc), "|abc");
    }

    #[test]
    fn alt_cmd_down_adds_one_below_at_the_same_column_past_a_short_lines_end() {
        let mut doc = doc_with("ab|cd\nx\nabcd", None);
        add_cursor_vertical(&mut doc, Dir::Down, 4);
        assert_eq!(show_all(&doc), "ab|cd\nx|\nabcd");
        add_cursor_vertical(&mut doc, Dir::Down, 4);
        assert_eq!(show_all(&doc), "ab|cd\nx|\nab|cd");
        add_cursor_vertical(&mut doc, Dir::Up, 4);
        assert_eq!(show_all(&doc), "ab|cd\nx|\nab|cd");
        // Already on the first line, ⌥⌘↑ does nothing.
        let mut top = doc_with("ab|cd\nx\nabcd", None);
        add_cursor_vertical(&mut top, Dir::Up, 4);
        assert_eq!(show_all(&top), "ab|cd\nx\nabcd");
    }

    #[test]
    fn cmd_d_takes_the_word_then_its_next_occurrences_wrapping_and_cmd_u_takes_the_last_back() {
        let mut doc = doc_with("foo x f|oo y foo", None);
        add_next_occurrence(&mut doc);
        assert_eq!(show_all(&doc), "foo x ^foo| y foo");
        add_next_occurrence(&mut doc);
        assert_eq!(show_all(&doc), "foo x ^foo| y ^foo|");
        add_next_occurrence(&mut doc);
        assert_eq!(show_all(&doc), "^foo| x ^foo| y ^foo|");
        // The newest is the main one.
        assert_eq!(doc.selection().anchor, p(0, 0));
        add_next_occurrence(&mut doc);
        assert_eq!(all_selections(&doc).len(), 3);
        remove_last_cursor(&mut doc);
        assert_eq!(show_all(&doc), "foo x ^foo| y ^foo|");
    }

    #[test]
    fn cmd_d_spans_a_line_break_and_stops_when_all_are_taken() {
        let mut doc = doc_with("^ab\ncd|\nab\ncd", None);
        add_next_occurrence(&mut doc);
        assert_eq!(show_all(&doc), "^ab\ncd|\n^ab\ncd|");
        let mut all = doc_with("^foo| x ^foo|", None);
        add_next_occurrence(&mut all);
        assert_eq!(show_all(&all), "^foo| x ^foo|");
    }

    #[test]
    fn shift_cmd_l_selects_every_occurrence_of_the_word_at_the_cursor() {
        let mut doc = doc_with("ab x a|b y ab", None);
        select_all_occurrences(&mut doc);
        assert_eq!(show_all(&doc), "^ab| x ^ab| y ^ab|");
        assert_eq!(doc.selection().anchor, p(0, 5));
        // A word of several bytes per character.
        let mut wide = doc_with("grüß x grü|ß", None);
        select_all_occurrences(&mut wide);
        assert_eq!(show_all(&wide), "^grüß| x ^grüß|");
    }

    #[test]
    fn esc_keeps_only_the_main_cursor() {
        let mut doc = doc_with("|a|b|c", Some(1));
        assert!(single_cursor(&mut doc));
        assert_eq!(show_all(&doc), "a|bc");
        assert!(!single_cursor(&mut doc));
    }

    #[test]
    fn alt_drag_draws_a_column_a_short_line_getting_a_cursor_at_its_end() {
        let mut doc = doc_with("|abcd\nx\nabcd", None);
        column_selection(&mut doc, p(0, 1), p(2, 3), 4, &[]);
        assert_eq!(show_all(&doc), "a^bc|d\nx|\na^bc|d");
    }

    #[test]
    fn alt_drag_upward_keeps_display_columns_through_a_tab() {
        let mut doc = doc_with("|\tabcd\nx\n\tabcd", None);
        column_selection(&mut doc, p(2, 1), p(0, 5), 4, &[]);
        assert_eq!(show_all(&doc), "\t^abcd|\nx|\n\t^abcd|");
    }

    #[test]
    fn alt_drag_adds_the_column_to_the_cursors_that_were_there() {
        let mut doc = doc_with("|abcd\nabcd\nabcd\nzz|z", None);
        let before = all_selections(&doc);
        column_selection(&mut doc, p(0, 1), p(1, 2), 4, &before);
        assert_eq!(show_all(&doc), "|a^b|cd\na^b|cd\nabcd\nzz|z");
    }

    #[test]
    fn copies_each_selection_and_pastes_one_per_cursor_when_the_count_matches() {
        let doc = doc_with("^a|1 ^b|2", None);
        let clip = copy_text(&doc).unwrap();
        assert_eq!(clip.text, "a\nb");
        let mut target = doc_with("x| y|", None);
        paste(&mut target, &clip, &ctx()).unwrap();
        assert_eq!(show_all(&target), "xa| yb|");
    }

    #[test]
    fn pastes_the_whole_text_at_every_cursor_when_the_count_does_not_match() {
        let mut target = doc_with("x| y|", None);
        let clip = ClipboardText {
            text: "q".into(),
            whole_line: false,
            parts: None,
        };
        paste(&mut target, &clip, &ctx()).unwrap();
        assert_eq!(show_all(&target), "xq| yq|");
    }

    #[test]
    fn cuts_every_selection_as_one_undo_step() {
        let mut doc = doc_with("^a|1 ^b|2", None);
        cut(&mut doc, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "|1 |2");
        run(&mut doc, &Command::Undo, &ctx()).unwrap();
        assert_eq!(doc.rope().text(), "a1 b2");
    }

    #[test]
    fn cuts_a_whole_line_once_however_many_cursors_sit_on_it() {
        let mut doc = doc_with("a|b|\ncd", None);
        let clip = cut(&mut doc, &ctx()).unwrap();
        assert_eq!(clip.text, "ab\n");
        assert_eq!(doc.rope().text(), "cd");
    }

    #[test]
    fn pastes_by_the_texts_own_line_count_when_the_parts_count_differs() {
        let clip = ClipboardText {
            text: "ab\ncd\nef".into(),
            whole_line: false,
            parts: Some(vec!["ab\ncd".into(), "ef".into()]),
        };
        let mut target = doc_with("x| y| z|", None);
        paste(&mut target, &clip, &ctx()).unwrap();
        assert_eq!(show_all(&target), "xab| ycd| zef|");
    }

    #[test]
    fn moves_adjacent_line_cursors_as_one_block() {
        let mut down = doc_with("a|\nb|\nc", None);
        run(&mut down, &Command::MoveLines(Dir::Down), &ctx()).unwrap();
        assert_eq!(down.rope().text(), "c\na\nb");
        // Found in the ED5.3 review: the second cursor's move was swallowed. Lines now move per block.
        let mut up = doc_with("a\nb|\nc|", None);
        run(&mut up, &Command::MoveLines(Dir::Up), &ctx()).unwrap();
        assert_eq!(up.rope().text(), "b\nc\na");
        assert_eq!(show_all(&up), "b|\nc|\na");
        run(&mut up, &Command::Undo, &ctx()).unwrap();
        assert_eq!(up.rope().text(), "a\nb\nc");
        let mut two = doc_with("x\na|\ny\nb|", None);
        run(&mut two, &Command::MoveLines(Dir::Up), &ctx()).unwrap();
        assert_eq!(show_all(&two), "a|\nx\nb|\ny");
    }

    #[test]
    fn duplicates_comments_and_outdents_each_cursors_own_line() {
        let mut dup = doc_with("one|\ntwo|", None);
        run(&mut dup, &Command::DuplicateLines(Dir::Down), &ctx()).unwrap();
        assert_eq!(dup.rope().text(), "one\none\ntwo\ntwo");
        let mut comment = doc_with("one|\ntwo|", None);
        run(&mut comment, &Command::ToggleComment, &ctx()).unwrap();
        assert_eq!(comment.rope().text(), "// one\n// two");
        let mut outdent = doc_with("    a|b|\n        c|d", None);
        run(&mut outdent, &Command::Outdent, &ctx()).unwrap();
        assert_eq!(outdent.rope().text(), "ab\n    cd");
    }

    #[test]
    fn deletes_a_word_or_to_the_line_start_at_each_cursor() {
        let mut word = doc_with("foo bar|\nbaz qux|", None);
        run(&mut word, &Command::DeleteBackward(BackUnit::Word), &ctx()).unwrap();
        assert_eq!(show_all(&word), "foo |\nbaz |");
        let mut line = doc_with("foo b|ar|\nbaz qux|", None);
        run(&mut line, &Command::DeleteBackward(BackUnit::Line), &ctx()).unwrap();
        assert_eq!(show_all(&line), "|\n|");
    }

    #[test]
    fn newline_keeps_each_lines_own_indentation() {
        let mut doc = doc_with("    foo|\n        bar|", None);
        run(&mut doc, &Command::Newline, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "    foo\n    |\n        bar\n        |");
    }

    #[test]
    fn a_cursor_jump_seals_the_typing_step_even_with_several_cursors() {
        let mut doc = doc_with("a|b\nc|d", None);
        run(&mut doc, &insert("X"), &at(0)).unwrap();
        let left = Command::Move {
            motion: Motion::CharLeft,
            extend: false,
        };
        run(&mut doc, &left, &ctx()).unwrap();
        run(&mut doc, &insert("Y"), &at(5)).unwrap();
        assert!(doc.rope().text().contains('X') && doc.rope().text().contains('Y'));
        // One undo takes back only the typing after the move.
        run(&mut doc, &Command::Undo, &ctx()).unwrap();
        assert!(doc.rope().text().contains('X') && !doc.rope().text().contains('Y'));
        run(&mut doc, &Command::Undo, &ctx()).unwrap();
        assert_eq!(doc.rope().text(), "ab\ncd");
    }

    #[test]
    fn undo_and_redo_restore_every_cursor_across_cmd_d_and_a_keystroke() {
        let mut doc = doc_with("fo|o bar foo", None);
        add_next_occurrence(&mut doc);
        add_next_occurrence(&mut doc);
        assert_eq!(show_all(&doc), "^foo| bar ^foo|");
        run(&mut doc, &insert("X"), &at(0)).unwrap();
        assert_eq!(show_all(&doc), "X| bar X|");
        run(&mut doc, &Command::Undo, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "^foo| bar ^foo|");
        run(&mut doc, &Command::Redo, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "X| bar X|");
    }

    #[test]
    fn a_plain_set_selection_clears_the_extra_cursors() {
        let mut doc = Document::new("abc\ndef", FOUR);
        doc.set_selections(cursor(0, 0), &[cursor(1, 0)], false);
        assert_eq!(doc.extra().len(), 1);
        // A Vi machine moving the cursor calls the plain set_selection, as any single-cursor caller would.
        doc.set_selection(cursor(0, 1), false);
        assert_eq!(doc.extra(), &[] as &[Selection]);
    }

    #[test]
    fn keeps_every_cursors_column_through_down_also_past_a_short_line() {
        let down = Command::Move {
            motion: Motion::Down,
            extend: false,
        };
        let mut doc = doc_with("abcde|f ab|c\nx\nabcdefghijkl", None);
        run(&mut doc, &down, &ctx()).unwrap();
        assert_eq!(show_all(&doc), "abcdef abc\nx|\nabcdefghijkl");
        // Both landed at the short line's end and merged — typical: one cursor goes on.
        let mut two = doc_with("ab|cd\nx\nabcd\nab|cd\nx\nabcd", None);
        run(&mut two, &down, &ctx()).unwrap();
        run(&mut two, &down, &ctx()).unwrap();
        assert_eq!(show_all(&two), "abcd\nx\nab|cd\nabcd\nx\nab|cd");
    }

    #[test]
    fn a_failing_visit_stops_the_run_and_leaves_one_undo_step() {
        let mut doc = doc_with("a|b\nc|d", None);
        let mut visits = 0;
        let result = each_cursor(&mut doc, Some(EditKind::Typing), false, |doc, _| {
            visits += 1;
            if visits == 2 {
                return Err(EditError::NoSuchLine {
                    line: 9,
                    line_count: 2,
                });
            }
            let at = doc.selection().head;
            doc.edit(
                &[crate::Change {
                    range: crate::Range::at(at),
                    text: "X".into(),
                }],
                Selection::cursor(Pos::new(at.line, at.col + 1)),
                EditKind::Typing,
                crate::Millis(0),
            )
        });
        assert!(result.is_err());
        // The last cursor in the text went first; the run stopped before the other.
        assert_eq!(show_all(&doc), "a|b\ncX|d");
        doc.undo().unwrap();
        assert_eq!(show_all(&doc), "a|b\nc|d");
    }

    #[test]
    fn a_failing_line_block_stops_the_run_too() {
        let mut doc = doc_with("a|\nx\nb|\nc", None);
        let mut visits = 0;
        let result = each_line_block(&mut doc, |doc| {
            visits += 1;
            if visits == 2 {
                return Err(EditError::NoSuchLine {
                    line: 9,
                    line_count: 4,
                });
            }
            run(doc, &Command::MoveLines(Dir::Down), &ctx())
        });
        assert!(result.is_err());
        // The lower block moved first; the upper one was never visited.
        assert_eq!(doc.rope().text(), "a\nx\nc\nb");
        assert_eq!(show_all(&doc), "a|\nx\nc\nb|");
        doc.undo().unwrap();
        assert_eq!(doc.rope().text(), "a\nx\nb\nc");
    }
}
