//! Everything the normal (Mac) key map can do to a document (Axiomata-OS `editor/commands.ts`, ED1, F5/F6). No UI:
//! a command reads the document and asks it to change. Which key runs which command is the surface's business.
//!
//! Vertical motion and ⌘←/→ depend on how lines wrap on screen, which only the view knows. They ask a [`RowLayout`]
//! for the columns where a line's visual rows begin; without wrapping every line is one row ([`UNWRAPPED`]), and the
//! tests use exactly that.
//!
//! The clipboard itself is not in here — it reaches outside the model — but the pure halves ([`copy_text`], [`cut`],
//! [`paste`]) are, so their rules are tested like everything else.

use crate::document::{Change, Document, EditKind, Millis, end_after};
use crate::error::EditError;
use crate::multicursor::{
    Dir, add_cursor_vertical, add_next_occurrence, all_selections, each_cursor, each_line_block,
    remove_last_cursor, select_all_occurrences, selected_lines, selection_texts, single_cursor,
};
use crate::position::{Pos, Range, Selection, floor_char_boundary};
use crate::snippet::CompletionEdit;
use crate::text::{
    col_for_display_column, display_column, leading_whitespace, next_grapheme, next_word_end,
    prev_grapheme, prev_word_start,
};
use crate::text_edits::{last_first, map_position};
use crate::{IndentKind, Rope};

/// How a view lays lines out: where a line's visual rows start, and which lines a fold hides.
pub trait RowLayout {
    /// The byte columns where `line`'s visual rows start; `[0]` for an unwrapped line.
    fn row_starts(&self, line: usize) -> Vec<usize>;

    /// Folding (ED5, T7): the nearest line at `line` or past it in `dir` that is not folded away — `None` past the
    /// end. Without folds every line is shown.
    fn visible_line(&self, line: usize, dir: Dir) -> Option<usize> {
        let _ = dir;
        Some(line)
    }
}

/// No wrapping and no folds: every line is one row.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unwrapped;

impl RowLayout for Unwrapped {
    fn row_starts(&self, _line: usize) -> Vec<usize> {
        vec![0]
    }
}

/// The layout without wrapping, for callers that have no view.
pub const UNWRAPPED: Unwrapped = Unwrapped;

/// What a command needs to know beyond the document.
#[derive(Clone, Copy)]
pub struct CommandContext<'a> {
    /// How many columns a tab takes.
    pub tab_size: usize,
    /// How lines wrap and fold on screen.
    pub layout: &'a dyn RowLayout,
    /// Visual rows a page holds, for Bild↑/↓.
    pub page_rows: usize,
    /// Line-comment prefix for ⌘/ (`//`, `#`, …), or `None` if the language has none ([`crate::line_comment_for`]).
    pub comment_prefix: Option<&'a str>,
    /// Now, for merging typing into one undo step; tests pass their own.
    pub now: Millis,
}

impl std::fmt::Debug for CommandContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandContext")
            .field("tab_size", &self.tab_size)
            .field("page_rows", &self.page_rows)
            .field("comment_prefix", &self.comment_prefix)
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

/// Where a cursor moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Motion {
    /// ←: one grapheme back, over a line break to the end of the line before.
    CharLeft,
    /// →: one grapheme on.
    CharRight,
    /// ⌥←: to the start of the word before.
    WordLeft,
    /// ⌥→: to the end of the word after.
    WordRight,
    /// ⌘←: the row's start, then the first non-blank, then column 0.
    LineStart,
    /// ⌘→: the row's end, then the line's end.
    LineEnd,
    /// ↑: one visual row up, keeping the goal column.
    Up,
    /// ↓: one visual row down.
    Down,
    /// Bild↑: a page of rows up.
    PageUp,
    /// Bild↓: a page of rows down.
    PageDown,
    /// ⌘↑: the start of the text.
    DocStart,
    /// ⌘↓: the end of the text.
    DocEnd,
}

/// How much ⌫ takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackUnit {
    /// ⌫: a grapheme (or back to the previous indentation stop in leading spaces).
    Char,
    /// ⌥⌫: back to the start of the word.
    Word,
    /// ⌘⌫: back to the start of the line.
    Line,
}

/// How much ⌦ takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForwardUnit {
    /// ⌦: a grapheme.
    Char,
    /// ⌥⌦: to the end of the word.
    Word,
}

/// A command the key map (or the menu, or a completion) runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A cursor motion; `extend` (⇧) keeps the anchor.
    Move {
        /// Where to.
        motion: Motion,
        /// Whether the selection grows instead of collapsing.
        extend: bool,
    },
    /// Text in place of the selection.
    Insert {
        /// What is typed or pasted.
        text: String,
        /// How the step merges: typing merges with typing before it.
        kind: EditKind,
    },
    /// ↩: a new line keeping the current indentation.
    Newline,
    /// ⌫ and its wider forms.
    DeleteBackward(BackUnit),
    /// ⌦ and its wider form.
    DeleteForward(ForwardUnit),
    /// ⌘A.
    SelectAll,
    /// ⌘L: the line, again the next one too.
    SelectLine,
    /// ⇥.
    Indent,
    /// ⇧⇥.
    Outdent,
    /// ⌥↑/↓: the selected lines swap with the line above or below.
    MoveLines(Dir),
    /// ⇧⌥↑/↓: a copy of the selected lines above or below.
    DuplicateLines(Dir),
    /// ⌘/.
    ToggleComment,
    /// ⌘Z.
    Undo,
    /// ⇧⌘Z.
    Redo,
    /// ⌥⌘↑/↓: one more cursor above or below.
    AddCursorVertical(Dir),
    /// ⌘D.
    AddNextOccurrence,
    /// ⌘U.
    RemoveLastCursor,
    /// ⇧⌘L.
    SelectAllOccurrences,
    /// Esc with several cursors.
    SingleCursor,
    /// Taking a completion (ED6.4): the typed word replaced, extra edits (an import) applied, one step.
    Complete(CompletionEdit),
    /// Changes made elsewhere and handed in (ED6.5: a formatter's output, a rename's edits), non-overlapping and in
    /// the current text's coordinates — one step; the cursor keeps its place in the text around them. They are
    /// applied in [`crate::last_first`] order: at one place a replacement goes before the insertions there, and
    /// insertions land in the order given.
    ReplaceText(Vec<Change>),
}

impl Command {
    /// Typing `text`.
    #[must_use]
    pub fn insert(text: &str) -> Self {
        Self::Insert {
            text: text.to_owned(),
            kind: EditKind::Typing,
        }
    }
}

/// What a command does to the text at each cursor, for merging into the undo step before it.
fn edit_kind_of(cmd: &Command) -> Option<EditKind> {
    match cmd {
        Command::Move { .. } | Command::SelectLine => None,
        Command::Insert { kind, .. } => Some(*kind),
        Command::DeleteBackward(_) | Command::DeleteForward(_) => Some(EditKind::Deleting),
        _ => Some(EditKind::Other),
    }
}

/// Commands that act on whole lines: with several cursors on one line, the line is taken once. Not ⇥ — at a bare
/// cursor it puts indentation there.
fn is_line_command(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Outdent | Command::DuplicateLines(_) | Command::ToggleComment
    )
}

/// Runs `cmd` against `doc` — at every cursor when there are several (ED5, T6). ⌘A, undo and redo act on the
/// document as a whole.
///
/// # Errors
/// Only when the command names a place the text does not have, which the commands avoid by clamping; with several
/// cursors the run stops at the first one (`docs/plans/rust-core.md` S5).
pub fn run(doc: &mut Document, cmd: &Command, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    match cmd {
        Command::AddCursorVertical(dir) => {
            add_cursor_vertical(doc, *dir, ctx.tab_size);
            return Ok(());
        }
        Command::AddNextOccurrence => {
            add_next_occurrence(doc);
            return Ok(());
        }
        Command::RemoveLastCursor => {
            remove_last_cursor(doc);
            return Ok(());
        }
        Command::SelectAllOccurrences => {
            select_all_occurrences(doc);
            return Ok(());
        }
        Command::SingleCursor => {
            single_cursor(doc);
            return Ok(());
        }
        // A completion is taken at the main cursor only; the menu is not offered with several.
        Command::Complete(edit) => return complete(doc, edit, ctx),
        Command::ReplaceText(changes) => return replace_text(doc, changes, ctx),
        _ => {}
    }
    if !doc.extra().is_empty() && !matches!(cmd, Command::Undo | Command::Redo) {
        return match cmd {
            Command::SelectAll => {
                select_all(doc);
                Ok(())
            }
            // Moving lines takes each block of touching lines as a whole; copying gives each line its own copy.
            Command::MoveLines(_) => each_line_block(doc, |doc| run_one(doc, cmd, ctx)),
            _ => each_cursor(doc, edit_kind_of(cmd), is_line_command(cmd), |doc, _| {
                run_one(doc, cmd, ctx)
            }),
        };
    }
    run_one(doc, cmd, ctx)
}

fn run_one(doc: &mut Document, cmd: &Command, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    match cmd {
        Command::Move { motion, extend } => move_cursor(doc, *motion, *extend, ctx),
        Command::Insert { text, kind } => insert(doc, text, *kind, ctx),
        Command::Newline => newline(doc, ctx),
        Command::DeleteBackward(unit) => delete_backward(doc, *unit, ctx),
        Command::DeleteForward(unit) => delete_forward(doc, *unit, ctx),
        Command::SelectAll => {
            select_all(doc);
            Ok(())
        }
        Command::SelectLine => select_line(doc),
        Command::Indent => indent(doc, ctx),
        Command::Outdent => outdent(doc, ctx),
        Command::MoveLines(dir) => move_lines(doc, *dir, ctx),
        Command::DuplicateLines(dir) => duplicate_lines(doc, *dir, ctx),
        Command::ToggleComment => toggle_comment(doc, ctx),
        Command::Undo => doc.undo().map(|_| ()),
        Command::Redo => doc.redo().map(|_| ()),
        // Answered in `run` before a single cursor is visited.
        Command::AddCursorVertical(_)
        | Command::AddNextOccurrence
        | Command::RemoveLastCursor
        | Command::SelectAllOccurrences
        | Command::SingleCursor
        | Command::Complete(_)
        | Command::ReplaceText(_) => Ok(()),
    }
}

/// The text one indentation level inserts.
#[must_use]
pub fn indent_unit(doc: &Document) -> String {
    let indent = doc.indent();
    match indent.kind {
        IndentKind::Tabs => "\t".to_owned(),
        IndentKind::Spaces => " ".repeat(indent.size),
    }
}

fn select_all(doc: &mut Document) {
    let end = doc.rope().end();
    doc.set_selection(
        Selection {
            anchor: Pos::default(),
            head: end,
        },
        false,
    );
}

fn line_of(rope: &Rope, line: usize) -> Result<&str, EditError> {
    rope.line(line)
}

fn line_end(rope: &Rope, line: usize) -> Result<Pos, EditError> {
    Ok(Pos::new(line, rope.line(line)?.len()))
}

fn shown_line(layout: &dyn RowLayout, line: usize, dir: Dir) -> Option<usize> {
    layout.visible_line(line, dir)
}

// ---------------------------------------------------------------- motion

fn move_cursor(
    doc: &mut Document,
    motion: Motion,
    extend: bool,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let sel = doc.selection();
    // ←/→ on a selection without ⇧ collapse it to the side they point at.
    if !extend && !sel.is_cursor() && matches!(motion, Motion::CharLeft | Motion::CharRight) {
        let r = sel.range();
        let at = if motion == Motion::CharLeft {
            r.start
        } else {
            r.end
        };
        doc.set_selection(Selection::cursor(at), false);
        return Ok(());
    }
    let vertical = matches!(
        motion,
        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
    );
    let head = if vertical {
        vertical_target(doc, motion, ctx)?
    } else {
        target(doc.rope(), sel.head, motion, ctx)?
    };
    let anchor = if extend { sel.anchor } else { head };
    doc.set_selection(Selection { anchor, head }, vertical);
    Ok(())
}

fn target(rope: &Rope, p: Pos, motion: Motion, ctx: &CommandContext<'_>) -> Result<Pos, EditError> {
    match motion {
        Motion::CharLeft => left_within_line(rope, p, prev_grapheme, ctx.layout),
        Motion::CharRight => right_within_line(rope, p, next_grapheme, ctx.layout),
        Motion::WordLeft => left_within_line(rope, p, prev_word_start, ctx.layout),
        Motion::WordRight => right_within_line(rope, p, next_word_end, ctx.layout),
        Motion::LineStart => line_start_target(rope, p, ctx),
        Motion::LineEnd => line_end_target(rope, p, ctx),
        Motion::DocStart => Ok(Pos::default()),
        Motion::DocEnd => last_shown_end(rope, ctx),
        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => Ok(p),
    }
}

/// ←/⌥← share one line-crossing rule: step within the line while there is room, otherwise land at the end of the
/// previous shown line (or stay at the very start). A fold is stepped over as a whole.
fn left_within_line(
    rope: &Rope,
    p: Pos,
    within_line: fn(&str, usize) -> usize,
    layout: &dyn RowLayout,
) -> Result<Pos, EditError> {
    if p.col > 0 {
        return Ok(Pos::new(p.line, within_line(line_of(rope, p.line)?, p.col)));
    }
    match p
        .line
        .checked_sub(1)
        .and_then(|prev| shown_line(layout, prev, Dir::Up))
    {
        Some(prev) => line_end(rope, prev),
        None => Ok(p),
    }
}

/// →/⌥→'s mirror of [`left_within_line`].
fn right_within_line(
    rope: &Rope,
    p: Pos,
    within_line: fn(&str, usize) -> usize,
    layout: &dyn RowLayout,
) -> Result<Pos, EditError> {
    let line = line_of(rope, p.line)?;
    if p.col < line.len() {
        return Ok(Pos::new(p.line, within_line(line, p.col)));
    }
    let next = if p.line + 1 < rope.line_count() {
        shown_line(layout, p.line + 1, Dir::Down)
    } else {
        None
    };
    Ok(next.map_or(p, |next| Pos::new(next, 0)))
}

/// Index of the visual row that holds `col`. A column exactly at a wrap point belongs to the row it starts — the
/// cursor is drawn there, too.
#[must_use]
pub fn row_index(starts: &[usize], col: usize) -> usize {
    starts.iter().rposition(|start| *start <= col).unwrap_or(0)
}

/// The last column of row `row` before the next row starts: the start of the character the row ends with.
fn row_end(line: &str, starts: &[usize], row: usize) -> usize {
    match starts.get(row + 1) {
        Some(next) => floor_char_boundary(line, next.saturating_sub(1)),
        None => line.len(),
    }
}

/// ⌘←: first the start of the visual row, then — on the first row — the first non-blank character, then column 0,
/// toggling between the two (F6).
fn line_start_target(rope: &Rope, p: Pos, ctx: &CommandContext<'_>) -> Result<Pos, EditError> {
    let starts = ctx.layout.row_starts(p.line);
    let row = row_index(&starts, p.col);
    if row > 0 && p.col != starts[row] {
        return Ok(Pos::new(p.line, starts[row]));
    }
    let indent_end = leading_whitespace(line_of(rope, p.line)?).len();
    Ok(Pos::new(
        p.line,
        if p.col == indent_end { 0 } else { indent_end },
    ))
}

/// ⌘→: first the end of the visual row, then the end of the logical line (F6).
fn line_end_target(rope: &Rope, p: Pos, ctx: &CommandContext<'_>) -> Result<Pos, EditError> {
    let starts = ctx.layout.row_starts(p.line);
    let row = row_index(&starts, p.col);
    let line = line_of(rope, p.line)?;
    if row + 1 < starts.len() {
        let end = row_end(line, &starts, row);
        if p.col < end {
            return Ok(Pos::new(p.line, end));
        }
    }
    Ok(Pos::new(p.line, line.len()))
}

/// Where one row step lands.
struct RowPosition {
    line: usize,
    row: usize,
    starts: Vec<usize>,
}

/// One visual row step in `dir` from `at`, wrapping onto the previous/next line's rows and over folded lines; `None`
/// past the first or last row of the whole document.
fn step_row(
    rope: &Rope,
    ctx: &CommandContext<'_>,
    dir: Dir,
    at: RowPosition,
) -> Option<RowPosition> {
    match dir {
        Dir::Up => {
            if at.row > 0 {
                return Some(RowPosition {
                    row: at.row - 1,
                    ..at
                });
            }
            let line = shown_line(ctx.layout, at.line.checked_sub(1)?, Dir::Up)?;
            let starts = ctx.layout.row_starts(line);
            Some(RowPosition {
                line,
                row: starts.len().saturating_sub(1),
                starts,
            })
        }
        Dir::Down => {
            if at.row + 1 < at.starts.len() {
                return Some(RowPosition {
                    row: at.row + 1,
                    ..at
                });
            }
            if at.line + 1 >= rope.line_count() {
                return None;
            }
            let line = shown_line(ctx.layout, at.line + 1, Dir::Down)?;
            Some(RowPosition {
                line,
                row: 0,
                starts: ctx.layout.row_starts(line),
            })
        }
    }
}

/// Where ↓ past the last row goes: the end of the text, or of the last shown line when a fold hides the end.
fn last_shown_end(rope: &Rope, ctx: &CommandContext<'_>) -> Result<Pos, EditError> {
    let last = rope.line_count() - 1;
    line_end(rope, shown_line(ctx.layout, last, Dir::Up).unwrap_or(last))
}

/// ↑/↓ and Bild↑/↓: walk visual rows, keeping the goal display column the first vertical move set. Past the first
/// or last row the cursor goes to the very start or end of the text. Also Vi's `gj`/`gk` (ED3). Sets the goal column.
///
/// # Errors
/// Only when the cursor stands on a line the text does not have.
pub fn vertical_target(
    doc: &mut Document,
    motion: Motion,
    ctx: &CommandContext<'_>,
) -> Result<Pos, EditError> {
    let head = doc.selection().head;
    let dir = if matches!(motion, Motion::Up | Motion::PageUp) {
        Dir::Up
    } else {
        Dir::Down
    };
    let steps = if matches!(motion, Motion::PageUp | Motion::PageDown) {
        ctx.page_rows.max(1)
    } else {
        1
    };
    let rope = doc.rope().clone();
    let text = line_of(&rope, head.line)?;
    let starts = ctx.layout.row_starts(head.line);
    let row = row_index(&starts, head.col);
    let goal = doc.goal_column().unwrap_or_else(|| {
        display_column(text, head.col, ctx.tab_size).saturating_sub(display_column(
            text,
            starts.get(row).copied().unwrap_or(0),
            ctx.tab_size,
        ))
    });
    let mut at = RowPosition {
        line: head.line,
        row,
        starts,
    };
    for _ in 0..steps {
        let Some(next) = step_row(&rope, ctx, dir, at) else {
            doc.set_goal_column(Some(goal));
            return match dir {
                Dir::Up => Ok(Pos::default()),
                Dir::Down => last_shown_end(&rope, ctx),
            };
        };
        at = next;
    }
    let line_text = line_of(&rope, at.line)?;
    let row_start = at
        .starts
        .get(at.row)
        .copied()
        .unwrap_or(0)
        .min(line_text.len());
    let end = row_end(line_text, &at.starts, at.row).max(row_start);
    let col = row_start + col_for_display_column(&line_text[row_start..end], goal, ctx.tab_size);
    doc.set_goal_column(Some(goal));
    Ok(Pos::new(at.line, col))
}

// ---------------------------------------------------------------- editing

fn edit_one(
    doc: &mut Document,
    change: Change,
    after: Selection,
    kind: EditKind,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    doc.edit(&[change], after, kind, ctx.now)
}

fn insert(
    doc: &mut Document,
    text: &str,
    kind: EditKind,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let r = doc.selection().range();
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let after = Selection::cursor(end_after(r.start, &normalised));
    edit_one(
        doc,
        Change {
            range: r,
            text: normalised,
        },
        after,
        kind,
        ctx,
    )
}

/// Takes a completion: replaces `before`/`after` bytes around the cursor with its text and applies the extra edits,
/// as one change from the last in the text to the first — so each is still in the coordinates it was given in.
fn complete(
    doc: &mut Document,
    edit: &CompletionEdit,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let targets = completion_targets(doc, edit)?;
    doc.edit(
        &targets.changes,
        targets.selection,
        EditKind::Other,
        ctx.now,
    )
}

/// What the [`Command::Complete`] command will do to `doc` as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTargets {
    /// The changes, in the order they are applied.
    pub changes: Vec<Change>,
    /// The selection after them: a snippet's first placeholder selected, so typing replaces it.
    pub selection: Selection,
    /// Where every tab stop will be (board card #36), for a [`crate::SnippetSession`].
    pub stops: Vec<Range>,
}

/// What [`Command::Complete`] does to `doc` as it is now — asked before running the command, so the caller can
/// follow the stops.
///
/// # Errors
/// Only when the cursor stands on a line the text does not have.
pub fn completion_targets(
    doc: &Document,
    edit: &CompletionEdit,
) -> Result<CompletionTargets, EditError> {
    let at = doc.selection().head;
    let line = line_of(doc.rope(), at.line)?;
    let start = floor_char_boundary(line, at.col.saturating_sub(edit.before));
    let end = floor_char_boundary(line, at.col + edit.after);
    let main = Change {
        range: Range {
            start: Pos::new(at.line, start),
            end: Pos::new(at.line, end.max(start)),
        },
        text: edit.text.clone(),
    };
    // A place inside the completion, moved by the edits before it (it is in the completion's own coordinates, so
    // only edits wholly before the replaced word count).
    let before: Vec<Change> = edit
        .extra
        .iter()
        .filter(|e| e.range.end <= main.range.start)
        .cloned()
        .collect();
    let place = |offset: usize| {
        let offset = floor_char_boundary(&edit.text, offset);
        map_position(end_after(main.range.start, &edit.text[..offset]), &before)
    };
    let stops: Vec<Range> = edit
        .stops
        .iter()
        .map(|s| Range {
            start: place(s.start),
            end: place(s.end),
        })
        .collect();
    let selection = stops.first().map_or_else(
        || Selection::cursor(place(edit.cursor)),
        |first| Selection {
            anchor: first.start,
            head: first.end,
        },
    );
    // The extras first: one at the word's very start goes in front of the completion, as `before` above takes it.
    let mut all = edit.extra.clone();
    all.push(main);
    Ok(CompletionTargets {
        changes: last_first(&all),
        selection,
        stops,
    })
}

/// Applies `changes` last to first (so each is where it was given) and keeps the cursor in its text.
fn replace_text(
    doc: &mut Document,
    changes: &[Change],
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    if changes.is_empty() {
        return Ok(());
    }
    let head = map_position(doc.selection().head, changes);
    let line = head
        .line
        .min(line_count_after(doc.rope(), changes).saturating_sub(1));
    // The document clamps a column past its line's end, or inside a character, into the line.
    doc.edit(
        &last_first(changes),
        Selection::cursor(Pos::new(line, head.col)),
        EditKind::Other,
        ctx.now,
    )
}

fn line_count_after(rope: &Rope, changes: &[Change]) -> usize {
    let mut count = rope.line_count();
    for c in changes {
        let added = end_after(c.range.start, &c.text).line - c.range.start.line;
        let removed = c.range.end.line.saturating_sub(c.range.start.line);
        count = (count + added).saturating_sub(removed);
    }
    count.max(1)
}

/// ↩: a new line that keeps the current line's indentation (up to the cursor).
fn newline(doc: &mut Document, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    let r = doc.selection().range();
    let line = line_of(doc.rope(), r.start.line)?;
    let lead = leading_whitespace(line);
    let indent = &lead[..r.start.col.min(lead.len())];
    let text = format!("\n{indent}");
    insert(doc, &text, EditKind::Typing, ctx)
}

fn delete_range(
    doc: &mut Document,
    from: Pos,
    to: Pos,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let r = Range::new(from, to);
    if r.is_empty() {
        return Ok(());
    }
    edit_one(
        doc,
        Change {
            range: r,
            text: String::new(),
        },
        Selection::cursor(r.start),
        EditKind::Deleting,
        ctx,
    )
}

/// ⌫: a selection goes as a whole. In leading spaces of a space-indented file it removes back to the previous
/// indentation stop, so one ⌫ undoes one ⇥.
fn delete_backward(
    doc: &mut Document,
    unit: BackUnit,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let sel = doc.selection();
    if !sel.is_cursor() {
        let r = sel.range();
        return delete_range(doc, r.start, r.end, ctx);
    }
    let head = sel.head;
    let rope = doc.rope().clone();
    match unit {
        BackUnit::Line => {
            let from = if head.col > 0 {
                Pos::new(head.line, 0)
            } else {
                target(&rope, head, Motion::CharLeft, ctx)?
            };
            delete_range(doc, from, head, ctx)
        }
        BackUnit::Word => delete_range(doc, target(&rope, head, Motion::WordLeft, ctx)?, head, ctx),
        BackUnit::Char => {
            let before = &line_of(&rope, head.line)?[..head.col];
            let indent = doc.indent();
            if indent.kind == IndentKind::Spaces
                && head.col > 0
                && before.bytes().all(|b| b == b' ')
            {
                let size = indent.size.max(1);
                let stop = (head.col - 1) / size * size;
                return delete_range(doc, Pos::new(head.line, stop), head, ctx);
            }
            delete_range(doc, target(&rope, head, Motion::CharLeft, ctx)?, head, ctx)
        }
    }
}

fn delete_forward(
    doc: &mut Document,
    unit: ForwardUnit,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let sel = doc.selection();
    if !sel.is_cursor() {
        let r = sel.range();
        return delete_range(doc, r.start, r.end, ctx);
    }
    let motion = match unit {
        ForwardUnit::Char => Motion::CharRight,
        ForwardUnit::Word => Motion::WordRight,
    };
    let to = target(doc.rope(), sel.head, motion, ctx)?;
    delete_range(doc, sel.head, to, ctx)
}

/// ⌘L: the whole line; again, the next line too.
fn select_line(doc: &mut Document) -> Result<(), EditError> {
    let sel = doc.selection();
    let (first, last) = selected_lines(sel);
    let r = sel.range();
    let already_whole =
        r.start.col == 0 && !sel.is_cursor() && r.end.col == 0 && r.end.line == last + 1;
    let end = if already_whole { last + 1 } else { last };
    let head = if end + 1 < doc.rope().line_count() {
        Pos::new(end + 1, 0)
    } else {
        line_end(doc.rope(), end.min(doc.rope().line_count() - 1))?
    };
    doc.set_selection(
        Selection {
            anchor: Pos::new(first, 0),
            head,
        },
        false,
    );
    Ok(())
}

/// Moves a position on `line` by `delta` bytes if it sits at or after `from`.
fn shift_col(p: Pos, line: usize, from: usize, delta: isize) -> Pos {
    if p.line == line && p.col >= from {
        Pos::new(line, p.col.saturating_add_signed(delta).max(from))
    } else {
        p
    }
}

fn shift_selection(sel: Selection, line: usize, from: usize, delta: isize) -> Selection {
    Selection {
        anchor: shift_col(sel.anchor, line, from, delta),
        head: shift_col(sel.head, line, from, delta),
    }
}

/// A byte length as a signed shift; text lengths never come near `isize::MAX`.
fn signed(len: usize) -> isize {
    isize::try_from(len).unwrap_or(isize::MAX)
}

/// ⇥: with a plain cursor, indentation up to the next stop at the cursor; with a selection, one level more on every
/// selected non-empty line.
fn indent(doc: &mut Document, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    let sel = doc.selection();
    if sel.is_cursor() {
        let text = line_of(doc.rope(), sel.head.line)?;
        let indent = doc.indent();
        let unit = match indent.kind {
            IndentKind::Tabs => "\t".to_owned(),
            IndentKind::Spaces => {
                let size = indent.size.max(1);
                " ".repeat(size - display_column(text, sel.head.col, ctx.tab_size) % size)
            }
        };
        return insert(doc, &unit, EditKind::Other, ctx);
    }
    let unit = indent_unit(doc);
    let (first, last) = selected_lines(sel);
    let mut changes = Vec::new();
    let mut after = sel;
    for line in first..=last {
        if line_of(doc.rope(), line)?.is_empty() {
            continue;
        }
        changes.push(Change {
            range: Range::at(Pos::new(line, 0)),
            text: unit.clone(),
        });
        after = shift_selection(after, line, 0, signed(unit.len()));
    }
    doc.edit(&changes, after, EditKind::Other, ctx.now)
}

/// ⇧⇥: at most one level less on every selected line.
fn outdent(doc: &mut Document, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    let sel = doc.selection();
    let (first, last) = selected_lines(sel);
    let size = doc.indent().size;
    let mut changes = Vec::new();
    let mut after = sel;
    for line in first..=last {
        let lead = leading_whitespace(line_of(doc.rope(), line)?);
        let spaces = lead.bytes().take_while(|b| *b == b' ').count();
        let remove = if lead.starts_with('\t') {
            1
        } else {
            spaces.min(size)
        };
        if remove == 0 {
            continue;
        }
        changes.push(Change {
            range: Range {
                start: Pos::new(line, 0),
                end: Pos::new(line, remove),
            },
            text: String::new(),
        });
        after = shift_selection(after, line, 0, -signed(remove));
    }
    doc.edit(&changes, after, EditKind::Other, ctx.now)
}

fn shift_lines(sel: Selection, up: usize, down: usize) -> Selection {
    let shift = |p: Pos| Pos::new((p.line + down).saturating_sub(up), p.col);
    Selection {
        anchor: shift(sel.anchor),
        head: shift(sel.head),
    }
}

fn lines_text(rope: &Rope, first: usize, last: usize) -> Result<String, EditError> {
    rope.slice(Range {
        start: Pos::new(first, 0),
        end: line_end(rope, last)?,
    })
}

/// ⌥↑/↓: the selected lines swap places with the line above or below.
fn move_lines(doc: &mut Document, dir: Dir, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    let sel = doc.selection();
    let (first, last) = selected_lines(sel);
    let rope = doc.rope().clone();
    let block = lines_text(&rope, first, last)?;
    match dir {
        Dir::Up => {
            if first == 0 {
                return Ok(());
            }
            let above = line_of(&rope, first - 1)?;
            let change = Change {
                range: Range {
                    start: Pos::new(first - 1, 0),
                    end: line_end(&rope, last)?,
                },
                text: format!("{block}\n{above}"),
            };
            edit_one(doc, change, shift_lines(sel, 1, 0), EditKind::Other, ctx)
        }
        Dir::Down => {
            if last + 1 >= rope.line_count() {
                return Ok(());
            }
            let below = line_of(&rope, last + 1)?;
            let change = Change {
                range: Range {
                    start: Pos::new(first, 0),
                    end: line_end(&rope, last + 1)?,
                },
                text: format!("{below}\n{block}"),
            };
            edit_one(doc, change, shift_lines(sel, 0, 1), EditKind::Other, ctx)
        }
    }
}

/// ⇧⌥↑/↓: a copy of the selected lines above or below; the cursor goes with the copy.
fn duplicate_lines(
    doc: &mut Document,
    dir: Dir,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let sel = doc.selection();
    let (first, last) = selected_lines(sel);
    let block = lines_text(doc.rope(), first, last)?;
    let end = line_end(doc.rope(), last)?;
    let after = match dir {
        Dir::Up => sel,
        Dir::Down => shift_lines(sel, 0, last - first + 1),
    };
    edit_one(
        doc,
        Change {
            range: Range::at(end),
            text: format!("\n{block}"),
        },
        after,
        EditKind::Other,
        ctx,
    )
}

/// ⌘/: comments the selected lines out, or back in if every non-blank one is commented. The prefix goes at the
/// smallest indentation among them, so a commented block keeps its shape.
fn toggle_comment(doc: &mut Document, ctx: &CommandContext<'_>) -> Result<(), EditError> {
    let Some(prefix) = ctx.comment_prefix else {
        return Ok(());
    };
    let sel = doc.selection();
    let (first, last) = selected_lines(sel);
    let mut lines = Vec::new();
    for line in first..=last {
        let text = line_of(doc.rope(), line)?;
        if !text.trim().is_empty() {
            lines.push((line, text.to_owned(), leading_whitespace(text).len()));
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    let all_commented = lines
        .iter()
        .all(|(_, text, lead)| text[*lead..].starts_with(prefix));
    let mut changes = Vec::new();
    let mut after = sel;
    if all_commented {
        for (line, text, lead) in &lines {
            let rest = &text[lead + prefix.len()..];
            let len = prefix.len() + usize::from(rest.starts_with(' '));
            changes.push(Change {
                range: Range {
                    start: Pos::new(*line, *lead),
                    end: Pos::new(*line, lead + len),
                },
                text: String::new(),
            });
            after = shift_selection(after, *line, *lead, -signed(len));
        }
    } else {
        let at = lines.iter().map(|(_, _, lead)| *lead).min().unwrap_or(0);
        for (line, _, _) in &lines {
            changes.push(Change {
                range: Range::at(Pos::new(*line, at)),
                text: format!("{prefix} "),
            });
            after = shift_selection(after, *line, at, signed(prefix.len() + 1));
        }
    }
    doc.edit(&changes, after, EditKind::Other, ctx.now)
}

// ---------------------------------------------------------------- clipboard

/// What ⌘C put on the clipboard, with what pasting needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardText {
    /// The text.
    pub text: String,
    /// Copied without a selection: the whole line, pasted back as a line.
    pub whole_line: bool,
    /// Copied from several cursors (ED5, T6): each one's text, in text order — pasted back one per cursor when the
    /// count matches.
    pub parts: Option<Vec<String>>,
}

/// ⌘C: the selection, or without one the whole current line (with its break).
///
/// # Errors
/// Only when a cursor stands on a line the text does not have.
pub fn copy_text(doc: &Document) -> Result<ClipboardText, EditError> {
    if !doc.extra().is_empty() {
        let parts = selection_texts(doc);
        if parts.iter().any(|t| !t.is_empty()) {
            return Ok(ClipboardText {
                text: parts.join("\n"),
                whole_line: false,
                parts: Some(parts),
            });
        }
        // Only bare cursors: their lines, as one whole-line copy each.
        let mut lines: Vec<usize> = all_selections(doc).iter().map(|s| s.head.line).collect();
        lines.sort_unstable();
        lines.dedup();
        let mut text = String::new();
        for line in lines {
            text.push_str(line_of(doc.rope(), line)?);
            text.push('\n');
        }
        return Ok(ClipboardText {
            text,
            whole_line: true,
            parts: None,
        });
    }
    let sel = doc.selection();
    if !sel.is_cursor() {
        return Ok(ClipboardText {
            text: doc.rope().slice(sel.range())?,
            whole_line: false,
            parts: None,
        });
    }
    Ok(ClipboardText {
        text: format!("{}\n", line_of(doc.rope(), sel.head.line)?),
        whole_line: true,
        parts: None,
    })
}

/// ⌘X: [`copy_text`], and then the selection — or the whole line — is gone.
///
/// # Errors
/// Only when a cursor stands on a line the text does not have.
pub fn cut(doc: &mut Document, ctx: &CommandContext<'_>) -> Result<ClipboardText, EditError> {
    let copied = copy_text(doc)?;
    let whole_line = copied.whole_line;
    if doc.extra().is_empty() {
        cut_one(doc, whole_line, ctx)?;
    } else {
        each_cursor(doc, Some(EditKind::Other), whole_line, |doc, _| {
            cut_one(doc, whole_line, ctx)
        })?;
    }
    Ok(copied)
}

/// One cursor's part of ⌘X: its selection, or its whole line.
fn cut_one(
    doc: &mut Document,
    whole_line: bool,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let sel = doc.selection();
    if !whole_line {
        let r = sel.range();
        if !sel.is_cursor() {
            let change = Change {
                range: r,
                text: String::new(),
            };
            edit_one(
                doc,
                change,
                Selection::cursor(r.start),
                EditKind::Other,
                ctx,
            )?;
        }
        return Ok(());
    }
    // The line goes with its break; the cursor lands in the line that takes its place (the next one, or for the
    // last line the one before), at the same display column as far as that line reaches.
    let rope = doc.rope().clone();
    let line = sel.head.line;
    let column = display_column(line_of(&rope, line)?, sel.head.col, ctx.tab_size);
    let landing_in = |l: usize| -> Result<Pos, EditError> {
        let text = line_of(&rope, l)?;
        Ok(Pos::new(
            l,
            col_for_display_column(text, column, ctx.tab_size),
        ))
    };
    let (r, landing) = if line + 1 < rope.line_count() {
        let next = landing_in(line + 1)?;
        (
            Range {
                start: Pos::new(line, 0),
                end: Pos::new(line + 1, 0),
            },
            Pos::new(line, next.col),
        )
    } else if line > 0 {
        (
            Range {
                start: line_end(&rope, line - 1)?,
                end: line_end(&rope, line)?,
            },
            landing_in(line - 1)?,
        )
    } else {
        (
            Range {
                start: Pos::default(),
                end: line_end(&rope, 0)?,
            },
            Pos::default(),
        )
    };
    let change = Change {
        range: r,
        text: String::new(),
    };
    edit_one(
        doc,
        change,
        Selection::cursor(landing),
        EditKind::Other,
        ctx,
    )
}

/// ⌘V: a whole line copied without a selection goes in above the current line, the cursor keeping its column;
/// anything else replaces the selection.
///
/// # Errors
/// Only when a cursor stands on a line the text does not have.
pub fn paste(
    doc: &mut Document,
    clip: &ClipboardText,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    if doc.extra().is_empty() {
        return paste_one(doc, clip, ctx);
    }
    // With several cursors (T6): one piece per cursor, in text order, when the copy had as many pieces (or the
    // text as many lines) as there are cursors; otherwise the whole text at every one.
    let all = all_selections(doc);
    let trimmed = clip.text.strip_suffix('\n').unwrap_or(&clip.text);
    let lines: Vec<String> = trimmed.split('\n').map(str::to_owned).collect();
    let pieces = match &clip.parts {
        Some(parts) if parts.len() == all.len() => Some(parts.clone()),
        _ if !clip.whole_line && lines.len() == all.len() => Some(lines),
        _ => None,
    };
    // Which piece each cursor gets: by its place in the text, not in the adding order.
    let mut by_place: Vec<usize> = (0..all.len()).collect();
    by_place.sort_by_key(|&i| all[i].range().start);
    let mut rank = vec![0; all.len()];
    for (k, &i) in by_place.iter().enumerate() {
        rank[i] = k;
    }
    each_cursor(doc, Some(EditKind::Other), false, |doc, i| match &pieces {
        Some(pieces) => {
            let piece = ClipboardText {
                text: pieces.get(rank[i]).cloned().unwrap_or_default(),
                whole_line: false,
                parts: None,
            };
            paste_one(doc, &piece, ctx)
        }
        None => paste_one(doc, clip, ctx),
    })
}

fn paste_one(
    doc: &mut Document,
    clip: &ClipboardText,
    ctx: &CommandContext<'_>,
) -> Result<(), EditError> {
    let sel = doc.selection();
    if clip.whole_line && sel.is_cursor() {
        let at = Pos::new(sel.head.line, 0);
        let text = if clip.text.ends_with('\n') {
            clip.text.clone()
        } else {
            format!("{}\n", clip.text)
        };
        let lines = text.matches('\n').count();
        let after = Selection::cursor(Pos::new(sel.head.line + lines, sel.head.col));
        return edit_one(
            doc,
            Change {
                range: Range::at(at),
                text,
            },
            after,
            EditKind::Other,
            ctx,
        );
    }
    insert(doc, &clip.text, EditKind::Other, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FOUR, ctx, doc_from, show};

    fn after_with(before: &str, cmd: &Command, ctx: &CommandContext<'_>) -> String {
        let mut doc = doc_from(before);
        run(&mut doc, cmd, ctx).unwrap();
        show(&doc)
    }

    fn after(before: &str, cmd: &Command) -> String {
        after_with(before, cmd, &ctx())
    }

    /// As `after`, with the indentation fixed at four spaces whatever the text suggests.
    fn after_indented(before: &str, cmd: &Command) -> String {
        let mut doc = doc_from(before);
        doc.set_indent(FOUR);
        run(&mut doc, cmd, &ctx()).unwrap();
        show(&doc)
    }

    fn moved(before: &str, motion: Motion, extend: bool) -> String {
        after(before, &Command::Move { motion, extend })
    }

    fn moved_in(before: &str, motion: Motion, layout: &dyn RowLayout) -> String {
        let ctx = CommandContext { layout, ..ctx() };
        after_with(
            before,
            &Command::Move {
                motion,
                extend: false,
            },
            &ctx,
        )
    }

    struct Rows(fn(usize) -> Vec<usize>);

    impl RowLayout for Rows {
        fn row_starts(&self, line: usize) -> Vec<usize> {
            (self.0)(line)
        }
    }

    #[test]
    fn moves_by_grapheme_and_across_line_ends() {
        // 😀 is four bytes (two UTF-16 units in the TS original).
        assert_eq!(moved("a|😀b", Motion::CharRight, false), "a😀|b");
        assert_eq!(moved("ab|\ncd", Motion::CharRight, false), "ab\n|cd");
        assert_eq!(moved("ab\n|cd", Motion::CharLeft, false), "ab|\ncd");
        assert_eq!(moved("|ab", Motion::CharLeft, false), "|ab");
    }

    #[test]
    fn collapses_a_selection_to_the_side_the_arrows_point_at() {
        assert_eq!(moved("a^bc|d", Motion::CharLeft, false), "a|bcd");
        assert_eq!(moved("a|bc^d", Motion::CharRight, false), "abc|d");
    }

    #[test]
    fn extends_with_shift() {
        assert_eq!(moved("a|bc", Motion::CharRight, true), "a^b|c");
        assert_eq!(moved("a^b|c", Motion::CharRight, true), "a^bc|");
    }

    #[test]
    fn jumps_words_stopping_at_dots_in_code() {
        assert_eq!(
            moved("|let foo.bar", Motion::WordRight, false),
            "let| foo.bar"
        );
        assert_eq!(
            moved("let| foo.bar", Motion::WordRight, false),
            "let foo|.bar"
        );
        assert_eq!(
            moved("let foo.bar|", Motion::WordLeft, false),
            "let foo.|bar"
        );
        assert_eq!(moved("|x\nnext", Motion::WordLeft, false), "|x\nnext");
        assert_eq!(moved("x|\nnext", Motion::WordRight, false), "x\n|next");
    }

    #[test]
    fn cmd_left_goes_to_the_first_non_blank_then_to_column_0_and_back() {
        assert_eq!(moved("    code|", Motion::LineStart, false), "    |code");
        assert_eq!(moved("    |code", Motion::LineStart, false), "|    code");
        assert_eq!(moved("|    code", Motion::LineStart, false), "    |code");
        assert_eq!(moved("    |code", Motion::LineEnd, false), "    code|");
    }

    #[test]
    fn goes_to_the_start_and_end_of_the_document() {
        assert_eq!(moved("a\nb|c\nd", Motion::DocStart, false), "|a\nbc\nd");
        assert_eq!(moved("a\nb|c\nd", Motion::DocEnd, false), "a\nbc\nd|");
    }

    #[test]
    fn keeps_the_goal_column_across_a_short_line() {
        let mut doc = doc_from("long line|\nab\nanother line");
        let down = Command::Move {
            motion: Motion::Down,
            extend: false,
        };
        run(&mut doc, &down, &ctx()).unwrap();
        assert_eq!(show(&doc), "long line\nab|\nanother line");
        run(&mut doc, &down, &ctx()).unwrap();
        assert_eq!(show(&doc), "long line\nab\nanother l|ine");
    }

    #[test]
    fn goes_to_the_very_start_or_end_past_the_first_or_last_line() {
        assert_eq!(moved("ab|c\nd", Motion::Up, false), "|abc\nd");
        assert_eq!(moved("a\nd|ef", Motion::Down, false), "a\ndef|");
    }

    #[test]
    fn counts_a_tab_by_its_width_and_wide_characters_by_theirs() {
        assert_eq!(
            moved("\tx|\n12345678", Motion::Down, false),
            "\tx\n12345|678"
        );
        // "ä" is two bytes but one column; "日" three bytes and two columns.
        assert_eq!(moved("ää|\nabcd", Motion::Down, false), "ää\nab|cd");
        assert_eq!(moved("abcd|\n日日日", Motion::Down, false), "abcd\n日日|日");
    }

    #[test]
    fn pages_by_the_rows_a_page_holds() {
        assert_eq!(
            moved("|0\n1\n2\n3\n4", Motion::PageDown, false),
            "0\n1\n2\n|3\n4"
        );
        assert_eq!(
            moved("0\n1\n2\n3\n|4", Motion::PageUp, false),
            "0\n|1\n2\n3\n4"
        );
    }

    #[test]
    fn walks_visual_rows_when_lines_wrap() {
        // Line 0 "aaaa bbbb cccc" wraps into rows starting at 0, 5 and 10.
        let layout = Rows(|line| if line == 0 { vec![0, 5, 10] } else { vec![0] });
        assert_eq!(
            moved_in("aa|aa bbbb cccc\nxyz", Motion::Down, &layout),
            "aaaa bb|bb cccc\nxyz"
        );
        assert_eq!(
            moved_in("aaaa bbbb cc|cc\nxyz", Motion::Up, &layout),
            "aaaa bb|bb cccc\nxyz"
        );
        assert_eq!(
            moved_in("aaaa bbbb cc|cc\nxyz", Motion::Down, &layout),
            "aaaa bbbb cccc\nxy|z"
        );
        assert_eq!(
            moved_in("aaaa bbbb cccc\nx|yz", Motion::Up, &layout),
            "aaaa bbbb c|ccc\nxyz"
        );
    }

    #[test]
    fn cmd_arrows_stop_at_the_visual_row_first_then_at_the_logical_line() {
        let layout = Rows(|_| vec![0, 5, 10]);
        assert_eq!(
            moved_in("aaaa bb|bb cccc", Motion::LineStart, &layout),
            "aaaa |bbbb cccc"
        );
        assert_eq!(
            moved_in("aaaa |bbbb cccc", Motion::LineStart, &layout),
            "|aaaa bbbb cccc"
        );
        assert_eq!(
            moved_in("aaaa bb|bb cccc", Motion::LineEnd, &layout),
            "aaaa bbbb| cccc"
        );
        assert_eq!(
            moved_in("aaaa bbbb| cccc", Motion::LineEnd, &layout),
            "aaaa bbbb cccc|"
        );
    }

    #[test]
    fn steps_over_a_fold() {
        // Lines 1 and 2 are folded away under line 0.
        struct Folded;
        impl RowLayout for Folded {
            fn row_starts(&self, _: usize) -> Vec<usize> {
                vec![0]
            }
            fn visible_line(&self, line: usize, dir: Dir) -> Option<usize> {
                match (line, dir) {
                    (1 | 2, Dir::Up) => Some(0),
                    (1 | 2, Dir::Down) => Some(3),
                    _ => Some(line),
                }
            }
        }
        assert_eq!(
            moved_in("a|\nb\nc\nd", Motion::CharRight, &Folded),
            "a\nb\nc\n|d"
        );
        assert_eq!(
            moved_in("a\nb\nc\n|d", Motion::CharLeft, &Folded),
            "a|\nb\nc\nd"
        );
        assert_eq!(
            moved_in("|a\nb\nc\nd", Motion::Down, &Folded),
            "a\nb\nc\n|d"
        );
    }

    #[test]
    fn replaces_the_selection_with_what_is_typed() {
        assert_eq!(after("a^bc|d", &Command::insert("X")), "aX|d");
    }

    #[test]
    fn keeps_the_indentation_on_a_new_line_up_to_the_cursor() {
        assert_eq!(after("    foo|", &Command::Newline), "    foo\n    |");
        assert_eq!(after("  |  foo", &Command::Newline), "  \n  |  foo");
    }

    #[test]
    fn backspace_removes_a_grapheme_a_line_break_or_the_selection() {
        let back = Command::DeleteBackward(BackUnit::Char);
        assert_eq!(after("a😀|b", &back), "a|b");
        assert_eq!(after("a\n|b", &back), "a|b");
        assert_eq!(after("a^bc|d", &back), "a|d");
    }

    #[test]
    fn backspace_in_leading_spaces_goes_back_to_the_previous_indentation_stop() {
        let back = Command::DeleteBackward(BackUnit::Char);
        assert_eq!(after_indented("      |x", &back), "    |x");
        assert_eq!(after_indented("    |x", &back), "|x");
    }

    #[test]
    fn alt_and_cmd_backspace_remove_a_word_and_the_line_up_to_the_cursor() {
        assert_eq!(
            after("let foo|", &Command::DeleteBackward(BackUnit::Word)),
            "let |"
        );
        assert_eq!(
            after("let foo|", &Command::DeleteBackward(BackUnit::Line)),
            "|"
        );
        assert_eq!(
            after("a\n|b", &Command::DeleteBackward(BackUnit::Line)),
            "a|b"
        );
    }

    #[test]
    fn delete_removes_forwards() {
        assert_eq!(
            after("a|bc", &Command::DeleteForward(ForwardUnit::Char)),
            "a|c"
        );
        assert_eq!(
            after("|foo bar", &Command::DeleteForward(ForwardUnit::Word)),
            "| bar"
        );
        assert_eq!(
            after("a|\nb", &Command::DeleteForward(ForwardUnit::Char)),
            "a|b"
        );
    }

    #[test]
    fn cmd_a_selects_everything() {
        assert_eq!(after("a\nb|c", &Command::SelectAll), "^a\nbc|");
    }

    #[test]
    fn cmd_l_selects_the_line_then_grows_by_one() {
        let mut doc = doc_from("one\ntw|o\nthree");
        run(&mut doc, &Command::SelectLine, &ctx()).unwrap();
        assert_eq!(show(&doc), "one\n^two\n|three");
        run(&mut doc, &Command::SelectLine, &ctx()).unwrap();
        assert_eq!(show(&doc), "one\n^two\nthree|");
    }

    #[test]
    fn tab_at_a_cursor_fills_up_to_the_next_stop() {
        assert_eq!(after("ab|", &Command::Indent), "ab  |");
        assert_eq!(after("|x", &Command::Indent), "    |x");
    }

    #[test]
    fn tab_and_shift_tab_on_a_selection_work_line_by_line_and_skip_empty_lines() {
        assert_eq!(after("^a\n\nb|", &Command::Indent), "    ^a\n\n    b|");
        assert_eq!(
            after_indented("    ^a\n  b\nc|", &Command::Outdent),
            "^a\nb\nc|"
        );
    }

    #[test]
    fn uses_tabs_in_a_tab_indented_file() {
        assert_eq!(after("\tone\n\t|two", &Command::Indent), "\tone\n\t\t|two");
    }

    #[test]
    fn leaves_a_last_line_out_that_the_selection_only_reaches_at_column_0() {
        assert_eq!(after("^a\nb\n|c", &Command::Indent), "    ^a\n    b\n|c");
    }

    #[test]
    fn alt_arrows_move_the_selected_lines() {
        assert_eq!(
            after("one\ntw|o\nthree", &Command::MoveLines(Dir::Up)),
            "tw|o\none\nthree"
        );
        assert_eq!(
            after("one\ntw|o\nthree", &Command::MoveLines(Dir::Down)),
            "one\nthree\ntw|o"
        );
        assert_eq!(
            after("on|e\ntwo", &Command::MoveLines(Dir::Up)),
            "on|e\ntwo"
        );
        assert_eq!(
            after("^one\ntwo|\nthree", &Command::MoveLines(Dir::Down)),
            "three\n^one\ntwo|"
        );
    }

    #[test]
    fn shift_alt_arrows_duplicate_them_the_cursor_going_with_the_copy() {
        assert_eq!(
            after("a|b\nc", &Command::DuplicateLines(Dir::Down)),
            "ab\na|b\nc"
        );
        assert_eq!(
            after("a|b\nc", &Command::DuplicateLines(Dir::Up)),
            "a|b\nab\nc"
        );
    }

    #[test]
    fn cmd_slash_comments_at_the_smallest_indentation_and_back() {
        assert_eq!(
            after("^  a\n    b|", &Command::ToggleComment),
            "^  // a\n  //   b|"
        );
        assert_eq!(
            after("^  // a\n  //   b|", &Command::ToggleComment),
            "^  a\n    b|"
        );
        let none = CommandContext {
            comment_prefix: None,
            ..ctx()
        };
        assert_eq!(after_with("  a|", &Command::ToggleComment, &none), "  a|");
    }

    #[test]
    fn copies_the_selection_or_the_whole_line_without_one() {
        let copied = copy_text(&doc_from("a^bc|d")).unwrap();
        assert_eq!((copied.text.as_str(), copied.whole_line), ("bc", false));
        let line = copy_text(&doc_from("one\ntw|o")).unwrap();
        assert_eq!((line.text.as_str(), line.whole_line), ("two\n", true));
    }

    #[test]
    fn cuts_a_whole_line_with_its_break_and_lands_in_the_next_one() {
        let mut doc = doc_from("one\ntw|o\nthree");
        let clip = cut(&mut doc, &ctx()).unwrap();
        assert_eq!((clip.text.as_str(), clip.whole_line), ("two\n", true));
        assert_eq!(show(&doc), "one\nth|ree");
        let mut last = doc_from("one\ntw|o");
        cut(&mut last, &ctx()).unwrap();
        assert_eq!(show(&last), "on|e");
        // The cursor keeps its display column, not its byte column: after two "é" (four bytes) it lands after two
        // characters; on a shorter line at its end.
        let mut wide = doc_from("éé|x\nabcdef");
        cut(&mut wide, &ctx()).unwrap();
        assert_eq!(show(&wide), "ab|cdef");
        let mut short = doc_from("abc|\näé");
        cut(&mut short, &ctx()).unwrap();
        assert_eq!(show(&short), "äé|");
    }

    #[test]
    fn line_commands_keep_multi_byte_text_whole() {
        assert_eq!(
            after("^  é\n  // ö|", &Command::ToggleComment),
            "^  // é\n  // // ö|"
        );
        assert_eq!(
            after("^  // é\n  // ö|", &Command::ToggleComment),
            "^  é\n  ö|"
        );
        assert_eq!(after("    日本|", &Command::Outdent), "日本|");
        assert_eq!(after("^é\nö|", &Command::Indent), "    ^é\n    ö|");
        // A wrapped row whose last character is "é": ⌘→ stops before it, as at any row end.
        let layout = Rows(|_| vec![0, 4]);
        assert_eq!(moved_in("|aaébbb", Motion::LineEnd, &layout), "aa|ébbb");
        assert_eq!(moved_in("aa|ébbb", Motion::LineEnd, &layout), "aaébbb|");
    }

    #[test]
    fn a_completion_whose_word_ends_inside_a_character_takes_the_whole_character() {
        let mut doc = doc_from("xé|");
        let edit = CompletionEdit {
            before: 1,
            text: "yz".into(),
            cursor: 2,
            ..CompletionEdit::default()
        };
        run(&mut doc, &Command::Complete(edit), &ctx()).unwrap();
        assert_eq!(show(&doc), "xyz|");
    }

    #[test]
    fn pastes_a_whole_line_above_the_current_one() {
        let mut doc = doc_from("one\nt|wo");
        let clip = ClipboardText {
            text: "new\n".into(),
            whole_line: true,
            parts: None,
        };
        paste(&mut doc, &clip, &ctx()).unwrap();
        assert_eq!(show(&doc), "one\nnew\nt|wo");
    }

    #[test]
    fn pastes_other_text_in_place_of_the_selection_as_one_undo_step() {
        let mut doc = doc_from("a^b|c");
        let clip = ClipboardText {
            text: "X\nY".into(),
            whole_line: false,
            parts: None,
        };
        paste(&mut doc, &clip, &ctx()).unwrap();
        assert_eq!(show(&doc), "aX\nY|c");
        doc.undo().unwrap();
        assert_eq!(show(&doc), "a^b|c");
    }

    #[test]
    fn keeps_the_cursor_on_its_line_through_a_format() {
        let mut doc = doc_from("fn x(){\nlet  long_name=1;\n}");
        doc.set_selection(Selection::cursor(Pos::new(1, 16)), false);
        let changes = crate::text_changes(doc.rope().text(), "fn x() {\n    let long_name = 1;\n}");
        run(&mut doc, &Command::ReplaceText(changes), &ctx()).unwrap();
        assert_eq!(doc.selection().head.line, 1);
        doc.undo().unwrap();
        assert_eq!(doc.rope().text(), "fn x(){\nlet  long_name=1;\n}");
    }

    #[test]
    fn replace_text_gives_the_formatted_text() {
        // The store keeps a file's final line break as its shape: "aa\n\n\n" holds "aa\n\n", whose empty lines get
        // two insertions at one place (the order `last_first` must keep).
        for (file, after) in [
            ("a\nb\nc", "a\nB\nc"),
            ("aa\n\n\n", "\na\n\na"),
            ("a", ""),
            ("", "new"),
        ] {
            let mut doc = doc_from(file);
            let before = doc.rope().text().to_owned();
            run(
                &mut doc,
                &Command::ReplaceText(crate::text_changes(&before, after)),
                &ctx(),
            )
            .unwrap();
            assert_eq!(doc.rope().text(), after, "{before:?} → {after:?}");
        }
    }
}
