//! One open document: its text, its selections, and its undo history (Axiomata-OS `editor/document.ts`).
//!
//! Every change goes through [`Document::edit`], which applies a list of replacements and records them as one undo
//! step. Undo is linear and unbounded for as long as the file is open. Consecutive steps of the same kind merge the
//! way a person expects them to:
//!
//! * typing stays one step until a 1 s pause, a cursor jump, or a switch between typing and deleting;
//! * everything else — paste, cut, indent, moving a line — is a step of its own;
//! * undo and redo restore the selection the step started or ended with.
//!
//! "Unsaved" is not a flag that could drift: the document remembers which undo step was on top when it was last
//! saved, and is dirty whenever that is not the step on top now — so undoing back to the save point is clean again,
//! while undoing past it and typing something else is dirty until the next save.
//!
//! Two things differ from the TypeScript original. An edit that fails halfway (a range outside the text) changes
//! nothing: the changes are applied to a snapshot first, which costs nothing with a [`Rope`]. And a follower of the
//! text does not register a callback: it asks [`Document::follow`] for a bookmark and reads what happened since with
//! [`Document::changes_since`] — several at once (a syntax tree, the other cursors of a multi-cursor run, a snippet's
//! tab stops), none taking anything away from another (`docs/plans/rust-core.md` S3).

use std::borrow::Cow;

use crate::detect::{
    FileShape, Indent, body_for_store, detect_indent, detect_shape, join_for_save,
};
use crate::error::EditError;
use crate::position::{Pos, Range, Selection};
use crate::rope::Rope;

/// How a step may merge with the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditKind {
    /// Typed text: merges with typing right before it.
    Typing,
    /// Deleted text, a key at a time: merges with deleting right before it.
    Deleting,
    /// Anything else — paste, cut, indent: a step of its own.
    Other,
}

/// A moment, in milliseconds on any clock the caller keeps steady; only the distance between two moments counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Millis(pub u64);

/// Longest pause that still continues a typing (or deleting) step.
pub const MERGE_WINDOW: Millis = Millis(1000);

/// One replacement as it hit the text, in both coordinate systems a syntax tree needs (tree-sitter's `InputEdit`):
/// positions and byte offsets, before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextChange {
    /// Where the replaced text started.
    pub start: Pos,
    /// Where the replaced text ended, before the change.
    pub old_end: Pos,
    /// Where the inserted text ends, after the change.
    pub new_end: Pos,
    /// `start` as a byte offset.
    pub start_byte: usize,
    /// `old_end` as a byte offset into the text before the change.
    pub old_end_byte: usize,
    /// `new_end` as a byte offset into the text after the change.
    pub new_end_byte: usize,
}

/// One replacement, in the coordinates of the text at the moment it is applied.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Change {
    /// What it replaces.
    pub range: Range,
    /// What it puts there; line breaks of any kind become `\n`.
    pub text: String,
}

/// A change as it happened, with what it removed — enough to undo it.
#[derive(Debug, Clone)]
struct AppliedChange {
    start: Pos,
    removed: String,
    inserted: String,
}

#[derive(Debug, Clone)]
struct Step {
    /// Names the step for the save point and an open undo group, which must know whether the top is still theirs.
    id: u64,
    changes: Vec<AppliedChange>,
    before: Selection,
    after: Selection,
    /// The other cursors before and after; empty with one cursor.
    extra_before: Vec<Selection>,
    extra_after: Vec<Selection>,
    kind: EditKind,
    time: Millis,
}

/// An open undo group: every edit until [`Document::end_undo_group`] joins one step.
#[derive(Debug, Clone)]
struct Group {
    /// The step the group's first edit made or merged into.
    step: Option<u64>,
    before: Selection,
    extra_before: Vec<Selection>,
    /// A typing or deleting group may still join the step before it, and leaves its own open afterwards.
    merges: bool,
}

/// The position after `text` when it is inserted at `start`; `text`'s line breaks are `\n`.
#[must_use]
pub fn end_after(start: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        None => Pos::new(start.line, start.col + text.len()),
        Some(at) => Pos::new(start.line + text.matches('\n').count(), text.len() - at - 1),
    }
}

/// `text` with every CRLF and lone CR as `\n`, as the rope will hold it.
fn unix_breaks(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// `rope` with `r` replaced by `text` (already with `\n` breaks); with `record`, the change as a syntax tree reads it
/// is pushed onto `recorded` (its byte offsets cost two walks of the tree, so only a follower pays for them).
fn replace_on(
    rope: &Rope,
    r: Range,
    text: &str,
    record: bool,
    recorded: &mut Vec<TextChange>,
) -> Result<Rope, EditError> {
    if !record {
        return Ok(rope.replace(r, text)?.rope);
    }
    let start_byte = rope.offset_at(r.start)?;
    let old_end_byte = rope.offset_at(r.end)?;
    let next = rope.replace(r, text)?;
    recorded.push(TextChange {
        start: r.start,
        old_end: r.end,
        new_end: next.end,
        start_byte,
        old_end_byte,
        new_end_byte: start_byte + text.len(),
    });
    Ok(next.rope)
}

/// A bookmark in a document's changes, from [`Document::follow`]. Not `Clone`: each follower has its own place, and
/// giving it back ([`Document::unfollow`]) lets the document drop what only it still had to read.
/// Until then the document keeps every change for it.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a follower keeps every change until it is given back with `Document::unfollow`"]
pub struct Follower {
    id: u64,
}

/// The replacements of the text not yet read by every follower, numbered in order.
#[derive(Debug, Clone, Default)]
struct ChangeLog {
    /// The number of `changes[0]`; the next change gets `base + changes.len()`.
    base: u64,
    changes: std::collections::VecDeque<TextChange>,
    /// Each follower's id and the number of the first change it has not read.
    readers: Vec<(u64, u64)>,
}

/// The next follower's id.
static NEXT_FOLLOWER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ChangeLog {
    fn is_followed(&self) -> bool {
        !self.readers.is_empty()
    }

    fn end(&self) -> u64 {
        self.base + self.changes.len() as u64
    }

    fn follow(&mut self) -> Follower {
        // Unique across every document, so a follower handed to another document finds no place there.
        let id = NEXT_FOLLOWER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.readers.push((id, self.end()));
        Follower { id }
    }

    fn extend(&mut self, recorded: &[TextChange]) {
        if self.is_followed() {
            self.changes.extend(recorded.iter().copied());
        }
    }

    fn read(&mut self, follower: &Follower) -> Vec<TextChange> {
        let end = self.end();
        let Some(reader) = self.readers.iter_mut().find(|(id, _)| *id == follower.id) else {
            return Vec::new();
        };
        // Within the log: `reader.1` lies in `base..=end` while the reader is known (`trim` keeps it so).
        let from = usize::try_from(reader.1 - self.base).unwrap_or(usize::MAX);
        reader.1 = end;
        let read = self.changes.iter().skip(from).copied().collect();
        self.trim();
        read
    }

    fn unfollow(&mut self, follower: u64) {
        self.readers.retain(|(id, _)| *id != follower);
        self.trim();
    }

    /// Drops the changes every follower has read.
    fn trim(&mut self) {
        let oldest = self
            .readers
            .iter()
            .map(|(_, at)| *at)
            .min()
            .unwrap_or_else(|| self.end());
        while self.base < oldest && self.changes.pop_front().is_some() {
            self.base += 1;
        }
    }
}

/// One open document.
#[derive(Debug, Clone)]
pub struct Document {
    rope: Rope,
    shape: FileShape,
    indent: Indent,
    indent_detected: bool,
    selection: Selection,
    extra: Vec<Selection>,
    extra_goals: Vec<Option<usize>>,
    goal_column: Option<usize>,
    revision: u64,
    undo_stack: Vec<Step>,
    redo_stack: Vec<Step>,
    /// The step on top when the document was last saved (`None` = none).
    saved_top: Option<u64>,
    /// Set when the next step must not merge into the current top.
    sealed: bool,
    group: Option<Group>,
    next_step: u64,
    log: ChangeLog,
}

impl Document {
    /// The document over a file's text, in the file's own shape; `indent_fallback` is used when the file shows no
    /// indentation of its own.
    #[must_use]
    pub fn new(file_text: &str, indent_fallback: Indent) -> Self {
        let shape = detect_shape(file_text);
        let detected = detect_indent(file_text);
        Self {
            rope: Rope::new(body_for_store(file_text, shape)),
            shape,
            indent: detected.unwrap_or(indent_fallback),
            indent_detected: detected.is_some(),
            selection: Selection::default(),
            extra: Vec::new(),
            extra_goals: Vec::new(),
            goal_column: None,
            revision: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            saved_top: None,
            sealed: true,
            group: None,
            next_step: 0,
            log: ChangeLog::default(),
        }
    }

    /// The current text. A clone is a snapshot that does not follow later edits.
    #[must_use]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// The file's line ending and final line break.
    #[must_use]
    pub fn shape(&self) -> FileShape {
        self.shape
    }

    /// The indentation new lines get.
    #[must_use]
    pub fn indent(&self) -> Indent {
        self.indent
    }

    /// Whether [`Document::indent`] came from the file rather than from the fallback.
    #[must_use]
    pub fn indent_detected(&self) -> bool {
        self.indent_detected
    }

    /// The main cursor: with several, the one added last.
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// The other cursors, in the order they were added (so the last one can be taken back). Empty for the usual
    /// single cursor; a plain [`Document::set_selection`] clears them.
    #[must_use]
    pub fn extra(&self) -> &[Selection] {
        &self.extra
    }

    /// Each other cursor's own goal column, parallel to [`Document::extra`].
    #[must_use]
    pub fn extra_goals(&self) -> &[Option<usize>] {
        &self.extra_goals
    }

    /// Sets each other cursor's goal column after a vertical move of every cursor; `goals` is cut or filled up with
    /// `None` to the number of other cursors.
    pub fn set_extra_goals(&mut self, mut goals: Vec<Option<usize>>) {
        goals.resize(self.extra.len(), None);
        self.extra_goals = goals;
    }

    /// Sets the indentation new lines and ⇥ use — the owner's choice in the status bar over what the file showed.
    pub fn set_indent(&mut self, indent: Indent) {
        self.indent = indent;
    }

    /// Sets the display column ↑/↓ try to keep, after a vertical move.
    pub fn set_goal_column(&mut self, goal: Option<usize>) {
        self.goal_column = goal;
    }

    /// The display column ↑/↓ try to keep, set by the first vertical move and cleared by anything else — so the
    /// cursor returns to column 40 after passing a short line.
    #[must_use]
    pub fn goal_column(&self) -> Option<usize> {
        self.goal_column
    }

    /// Grows with every change, undo, redo and reload; tells a view when to redraw.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The text to save, in the file's own line endings.
    #[must_use]
    pub fn text_for_save(&self) -> String {
        join_for_save(self.rope.text(), self.shape)
    }

    /// Whether the text differs from what was last saved, by undo step.
    #[must_use]
    pub fn dirty(&self) -> bool {
        self.top_id() != self.saved_top
    }

    /// Whether there is a step to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether there is a step to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Whether an undo group is open.
    #[must_use]
    pub fn in_undo_group(&self) -> bool {
        self.group.is_some()
    }

    /// Marks the current text as saved, and seals the step on top against merging.
    pub fn mark_saved(&mut self) {
        self.saved_top = self.top_id();
        self.sealed = true;
    }

    /// Moves the selection, clamped into the text, and drops the other cursors. A move that is not simply "the cursor
    /// stays where the last edit left it" seals the current step, so the next typing starts a new one.
    pub fn set_selection(&mut self, sel: Selection, keep_goal_column: bool) {
        let next = self.clamped(sel);
        if self.undo_stack.last().is_none_or(|top| top.after != next) {
            self.sealed = true;
        }
        self.selection = next;
        self.extra.clear();
        self.extra_goals.clear();
        if !keep_goal_column {
            self.goal_column = None;
        }
    }

    /// Sets every cursor at once: `main` and the others, kept in the order given. Seals the typing step like a cursor
    /// jump unless `continues` — a multi-cursor run putting back what its own edits made, which the step just made
    /// should then undo back to and redo to.
    pub fn set_selections(&mut self, main: Selection, others: &[Selection], continues: bool) {
        if !continues {
            self.sealed = true;
        }
        self.selection = self.clamped(main);
        self.extra = others.iter().map(|sel| self.clamped(*sel)).collect();
        self.extra_goals = vec![None; others.len()];
        self.goal_column = None;
        if continues && let Some(top) = self.undo_stack.last_mut() {
            top.after = self.selection;
            top.extra_after.clone_from(&self.extra);
        }
    }

    /// For a multi-cursor run: `sel` as the only cursor for the moment, with its own goal column, without sealing the
    /// typing step. The runner puts every cursor back afterwards.
    pub fn focus_cursor(&mut self, sel: Selection, goal_column: Option<usize>) {
        self.selection = self.clamped(sel);
        self.extra.clear();
        self.extra_goals.clear();
        self.goal_column = goal_column;
    }

    /// Applies `changes` in order (each in the coordinates left by the previous one), records them as one step, and
    /// puts the selection at `after`, clamped into the new text.
    ///
    /// # Errors
    /// When a change's range is reversed, or lies outside the text or inside a character, as the changes before it
    /// left it. Then nothing changed: not the text, the history, or the selection.
    pub fn edit(
        &mut self,
        changes: &[Change],
        after: Selection,
        kind: EditKind,
        now: Millis,
    ) -> Result<(), EditError> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut rope = self.rope.clone();
        let mut applied = Vec::with_capacity(changes.len());
        let mut recorded = Vec::new();
        for change in changes {
            let inserted = unix_breaks(&change.text);
            let removed = rope.slice(change.range)?;
            rope = replace_on(
                &rope,
                change.range,
                &inserted,
                self.log.is_followed(),
                &mut recorded,
            )?;
            applied.push(AppliedChange {
                start: change.range.start,
                removed,
                inserted: inserted.into_owned(),
            });
        }
        self.commit(rope, applied, &recorded, after, kind, now);
        Ok(())
    }

    /// Takes an applied edit into the text and the history.
    fn commit(
        &mut self,
        rope: Rope,
        applied: Vec<AppliedChange>,
        recorded: &[TextChange],
        after: Selection,
        kind: EditKind,
        now: Millis,
    ) {
        self.rope = rope;
        self.log.extend(recorded);
        let after = self.clamped(after);
        self.redo_stack.clear();
        let top_id = self.top_id();
        let grouped = self
            .group
            .as_ref()
            .is_some_and(|group| group.step.is_some() && group.step == top_id);
        let merges = grouped
            || self.undo_stack.last().is_some_and(|top| {
                !self.sealed
                    && kind != EditKind::Other
                    && top.kind == kind
                    && now.0.saturating_sub(top.time.0) <= MERGE_WINDOW.0
                    && Some(top.id) != self.saved_top
            });
        match self.undo_stack.last_mut() {
            Some(top) if merges => {
                top.changes.extend(applied);
                top.after = after;
                top.extra_after.clone_from(&self.extra);
                top.time = now;
            }
            _ => {
                let (before, extra_before) = match &self.group {
                    Some(group) => (group.before, group.extra_before.clone()),
                    None => (self.selection, self.extra.clone()),
                };
                let id = self.next_step;
                self.next_step += 1;
                self.undo_stack.push(Step {
                    id,
                    changes: applied,
                    before,
                    after,
                    extra_before,
                    extra_after: self.extra.clone(),
                    kind,
                    time: now,
                });
            }
        }
        // A group's edits all join the step its first edit made (or merged into).
        let top_id = self.top_id();
        if let Some(group) = &mut self.group {
            group.step = top_id;
        }
        self.sealed = kind == EditKind::Other;
        self.selection = after;
        self.goal_column = None;
        self.revision += 1;
    }

    /// Opens an undo group: the edits until [`Document::end_undo_group`] become one step, and undoing it returns to
    /// the selection before its first edit. With `merge` typing or deleting (a multi-cursor keystroke), the group may
    /// still join the step of that kind before it.
    pub fn begin_undo_group(&mut self, merge: Option<EditKind>) {
        let merges = matches!(merge, Some(EditKind::Typing | EditKind::Deleting));
        self.group = Some(Group {
            step: None,
            before: self.selection,
            extra_before: self.extra.clone(),
            merges,
        });
        if !merges {
            self.sealed = true;
        }
    }

    /// Closes the undo group. A typing group leaves its step open for the next keystroke, as a single typed
    /// character does.
    pub fn end_undo_group(&mut self) {
        if !self.group.as_ref().is_some_and(|group| group.merges) {
            self.sealed = true;
        }
        self.group = None;
    }

    /// Undoes the step on top; `false` when there is none.
    ///
    /// # Errors
    /// Only if the history no longer fits the text, which is a bug; then nothing changed.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        self.group = None;
        let Some(step) = self.undo_stack.last() else {
            return Ok(false);
        };
        let mut rope = self.rope.clone();
        let mut recorded = Vec::new();
        for change in step.changes.iter().rev() {
            let r = Range {
                start: change.start,
                end: end_after(change.start, &change.inserted),
            };
            rope = replace_on(
                &rope,
                r,
                &change.removed,
                self.log.is_followed(),
                &mut recorded,
            )?;
        }
        self.take_rope(rope, &recorded);
        if let Some(step) = self.undo_stack.pop() {
            self.after_history_move(step.before, &step.extra_before);
            self.redo_stack.push(step);
        }
        Ok(true)
    }

    /// Redoes the step undone last; `false` when there is none.
    ///
    /// # Errors
    /// Only if the history no longer fits the text, which is a bug; then nothing changed.
    pub fn redo(&mut self) -> Result<bool, EditError> {
        self.group = None;
        let Some(step) = self.redo_stack.last() else {
            return Ok(false);
        };
        let mut rope = self.rope.clone();
        let mut recorded = Vec::new();
        for change in &step.changes {
            let r = Range {
                start: change.start,
                end: end_after(change.start, &change.removed),
            };
            rope = replace_on(
                &rope,
                r,
                &change.inserted,
                self.log.is_followed(),
                &mut recorded,
            )?;
        }
        self.take_rope(rope, &recorded);
        if let Some(step) = self.redo_stack.pop() {
            self.after_history_move(step.after, &step.extra_after);
            self.undo_stack.push(step);
        }
        Ok(true)
    }

    /// Replaces the whole text with `file_text` (in the file's own shape) as one undoable step — restoring kept
    /// unsaved work, which the owner may want to take back with ⌘Z.
    pub fn replace_all(&mut self, file_text: &str, now: Millis) {
        let body = unix_breaks(body_for_store(file_text, self.shape)).into_owned();
        let (rope, change) = self.whole_text_replaced(&body);
        let removed = self.rope.text().to_owned();
        let applied = vec![AppliedChange {
            start: Pos::default(),
            removed,
            inserted: body,
        }];
        self.commit(
            rope,
            applied,
            &[change],
            Selection::default(),
            EditKind::Other,
            now,
        );
    }

    /// Replaces the whole text — a reload after an outside change. Clears the history (there is nothing to undo
    /// into) and counts as saved; the cursor stays as close to where it was as the new text allows.
    pub fn reset(&mut self, file_text: &str) {
        self.shape = detect_shape(file_text);
        let body = unix_breaks(body_for_store(file_text, self.shape)).into_owned();
        let (rope, change) = self.whole_text_replaced(&body);
        self.take_rope(rope, &[change]);
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.saved_top = None;
        self.sealed = true;
        self.group = None;
        self.selection = Selection::cursor(self.rope.clamp(self.selection.head));
        self.extra.clear();
        self.extra_goals.clear();
        self.goal_column = None;
        self.revision += 1;
    }

    /// A bookmark in the document's changes: from now on every replacement of the text — edits, undo, redo,
    /// reloads alike — is kept until this follower has read it with [`Document::changes_since`]. Give it back with
    /// [`Document::unfollow`]; while nobody follows, nothing is kept (and no byte offsets are computed).
    pub fn follow(&mut self) -> Follower {
        self.log.follow()
    }

    /// The replacements since `follower` last read, in order, and moves its bookmark to now. Other followers still
    /// read them; what every follower has read is dropped. A follower of another document reads nothing.
    pub fn changes_since(&mut self, follower: &Follower) -> Vec<TextChange> {
        self.log.read(follower)
    }

    /// Ends `follower`'s bookmark; what only it still had to read is dropped.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "taken by value so a follower given back cannot read again"
    )]
    pub fn unfollow(&mut self, follower: Follower) {
        self.log.unfollow(follower.id);
    }

    /// The text replaced as a whole by `body`, and that replacement as a syntax tree reads it.
    fn whole_text_replaced(&self, body: &str) -> (Rope, TextChange) {
        let rope = Rope::new(body);
        let change = TextChange {
            start: Pos::default(),
            old_end: self.rope.end(),
            new_end: rope.end(),
            start_byte: 0,
            old_end_byte: self.rope.len(),
            new_end_byte: rope.len(),
        };
        (rope, change)
    }

    fn take_rope(&mut self, rope: Rope, recorded: &[TextChange]) {
        self.rope = rope;
        self.log.extend(recorded);
    }

    fn after_history_move(&mut self, sel: Selection, extra: &[Selection]) {
        self.selection = self.clamped(sel);
        self.extra = extra.iter().map(|s| self.clamped(*s)).collect();
        self.extra_goals = vec![None; extra.len()];
        self.sealed = true;
        self.goal_column = None;
        self.revision += 1;
    }

    fn clamped(&self, sel: Selection) -> Selection {
        Selection {
            anchor: self.rope.clamp(sel.anchor),
            head: self.rope.clamp(sel.head),
        }
    }

    fn top_id(&self) -> Option<u64> {
        self.undo_stack.last().map(|step| step.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::IndentKind;
    use crate::testing::{FOUR, cursor, doc_from, p, show};
    use crate::text::prev_grapheme;

    /// What the editor's "insert" command does: the selection replaced by `text`, the cursor after it.
    fn insert(doc: &mut Document, text: &str, kind: EditKind, now: u64) {
        let r = doc.selection().range();
        let after = Selection::cursor(end_after(r.start, &unix_breaks(text)));
        doc.edit(
            &[Change {
                range: r,
                text: text.to_owned(),
            }],
            after,
            kind,
            Millis(now),
        )
        .unwrap();
    }

    fn type_text(doc: &mut Document, text: &str, now: u64) {
        for ch in text.chars() {
            insert(doc, ch.encode_utf8(&mut [0; 4]), EditKind::Typing, now);
        }
    }

    /// What ⌫ does on one cursor: the selection, else the character before it (or the line break at a line's start).
    fn delete_backward(doc: &mut Document, now: u64) {
        let sel = doc.selection();
        let r = if sel.is_cursor() {
            let head = sel.head;
            let from = if head.col > 0 {
                p(
                    head.line,
                    prev_grapheme(doc.rope().line(head.line).unwrap(), head.col),
                )
            } else if head.line > 0 {
                p(head.line - 1, doc.rope().line(head.line - 1).unwrap().len())
            } else {
                head
            };
            Range::new(from, head)
        } else {
            sel.range()
        };
        doc.edit(
            &[Change {
                range: r,
                text: String::new(),
            }],
            Selection::cursor(r.start),
            EditKind::Deleting,
            Millis(now),
        )
        .unwrap();
    }

    fn undo(doc: &mut Document) -> bool {
        doc.undo().unwrap()
    }

    fn redo(doc: &mut Document) -> bool {
        doc.redo().unwrap()
    }

    // ---- undo grouping ----

    #[test]
    fn merges_a_run_of_typing_into_one_step() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "hello", 0);
        assert_eq!(show(&doc), "hello|");
        undo(&mut doc);
        assert_eq!(show(&doc), "|");
        redo(&mut doc);
        assert_eq!(show(&doc), "hello|");
    }

    #[test]
    fn starts_a_new_step_after_a_pause_of_more_than_a_second() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "ab", 0);
        type_text(&mut doc, "cd", 1500);
        undo(&mut doc);
        assert_eq!(show(&doc), "ab|");
    }

    #[test]
    fn starts_a_new_step_after_a_cursor_jump() {
        let mut doc = doc_from("|xyz");
        type_text(&mut doc, "ab", 0);
        doc.set_selection(cursor(0, 5), false);
        type_text(&mut doc, "c", 10);
        undo(&mut doc);
        assert_eq!(show(&doc), "abxyz|");
        undo(&mut doc);
        assert_eq!(show(&doc), "|xyz");
    }

    #[test]
    fn keeps_typing_and_deleting_apart() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "abc", 0);
        delete_backward(&mut doc, 10);
        delete_backward(&mut doc, 20);
        assert_eq!(show(&doc), "a|");
        undo(&mut doc);
        assert_eq!(show(&doc), "abc|");
        undo(&mut doc);
        assert_eq!(show(&doc), "|");
    }

    #[test]
    fn never_merges_a_paste() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        insert(&mut doc, "PASTE", EditKind::Other, 1);
        type_text(&mut doc, "b", 2);
        undo(&mut doc);
        assert_eq!(show(&doc), "aPASTE|");
        undo(&mut doc);
        assert_eq!(show(&doc), "a|");
    }

    #[test]
    fn restores_the_selection_a_step_started_with() {
        let mut doc = doc_from("one ^two| three");
        insert(&mut doc, "2", EditKind::Typing, 0);
        assert_eq!(show(&doc), "one 2| three");
        undo(&mut doc);
        assert_eq!(show(&doc), "one ^two| three");
    }

    #[test]
    fn undoes_and_redoes_multi_line_changes_exactly() {
        let mut doc = doc_from("a|b");
        insert(&mut doc, "1\r\n2\n3", EditKind::Other, 0);
        assert_eq!(show(&doc), "a1\n2\n3|b");
        undo(&mut doc);
        assert_eq!(show(&doc), "a|b");
        redo(&mut doc);
        assert_eq!(show(&doc), "a1\n2\n3|b");
    }

    #[test]
    fn drops_the_redo_history_on_a_new_edit() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        undo(&mut doc);
        type_text(&mut doc, "b", 10);
        assert!(!redo(&mut doc));
        assert_eq!(show(&doc), "b|");
    }

    #[test]
    fn undoes_typed_umlauts_and_emoji_by_their_bytes() {
        let mut doc = doc_from("x|");
        type_text(&mut doc, "äö🙂", 0);
        assert_eq!(show(&doc), "xäö🙂|");
        delete_backward(&mut doc, 10);
        assert_eq!(show(&doc), "xäö|");
        undo(&mut doc);
        assert_eq!(show(&doc), "xäö🙂|");
        undo(&mut doc);
        assert_eq!(show(&doc), "x|");
    }

    // ---- dirty tracking ----

    #[test]
    fn is_clean_when_opened_dirty_after_an_edit_clean_after_save() {
        let mut doc = doc_from("|x");
        assert!(!doc.dirty());
        type_text(&mut doc, "a", 0);
        assert!(doc.dirty());
        doc.mark_saved();
        assert!(!doc.dirty());
    }

    #[test]
    fn is_clean_again_after_undoing_back_to_the_save_point() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.mark_saved();
        type_text(&mut doc, "b", 10);
        assert!(doc.dirty());
        undo(&mut doc);
        assert!(!doc.dirty());
        redo(&mut doc);
        assert!(doc.dirty());
    }

    #[test]
    fn does_not_merge_typing_into_the_step_that_was_saved() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.mark_saved();
        type_text(&mut doc, "b", 10);
        undo(&mut doc);
        assert_eq!(show(&doc), "a|");
    }

    #[test]
    fn stays_dirty_after_undoing_past_the_save_point_and_typing_something_else() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.mark_saved();
        undo(&mut doc);
        type_text(&mut doc, "b", 2000);
        assert!(doc.dirty());
    }

    // ---- saving and reloading ----

    #[test]
    fn saves_in_the_files_own_line_endings_with_its_final_newline() {
        let mut doc = Document::new("a\r\nb\r\n", FOUR);
        assert_eq!(doc.rope().line_count(), 2);
        let end = doc.rope().end();
        doc.set_selection(Selection::cursor(end), false);
        insert(&mut doc, "\n", EditKind::Typing, 0);
        assert_eq!(doc.text_for_save(), "a\r\nb\r\n\r\n");
    }

    #[test]
    fn reloads_as_clean_with_no_history_keeping_the_cursor_where_it_can() {
        let mut doc = doc_from("line one\nline |two");
        type_text(&mut doc, "x", 0);
        doc.reset("short\n");
        assert!(!doc.dirty());
        assert!(!doc.can_undo());
        assert_eq!(show(&doc), "short|");
    }

    #[test]
    fn uses_the_files_indentation_or_the_fallback_when_it_shows_none() {
        assert_eq!(
            Document::new("a\n\tb", FOUR).indent(),
            Indent {
                kind: IndentKind::Tabs,
                size: 4
            }
        );
        let two = Indent {
            kind: IndentKind::Spaces,
            size: 2,
        };
        let prose = Document::new("prose", two);
        assert_eq!(prose.indent(), two);
        assert!(!prose.indent_detected());
    }

    #[test]
    fn replaces_the_whole_text_as_one_undoable_step() {
        let mut doc = doc_from("old| text\n");
        doc.replace_all("new\r\ntext\n", Millis(0));
        assert_eq!(show(&doc), "|new\ntext");
        assert!(doc.dirty());
        undo(&mut doc);
        assert_eq!(show(&doc), "old| text");
        assert!(!doc.dirty());
    }

    // ---- undo groups ----

    #[test]
    fn joins_every_edit_until_the_group_ends_into_one_step_kinds_and_pauses_regardless() {
        let mut doc = Document::new("abc", FOUR);
        doc.begin_undo_group(None);
        let at = |line, col| Range::at(p(line, col));
        let change = |range, text: &str| {
            [Change {
                range,
                text: text.to_owned(),
            }]
        };
        doc.edit(
            &change(Range::new(p(0, 0), p(0, 1)), ""),
            cursor(0, 0),
            EditKind::Other,
            Millis(0),
        )
        .unwrap();
        doc.edit(
            &change(at(0, 0), "X"),
            cursor(0, 1),
            EditKind::Typing,
            Millis(5000),
        )
        .unwrap();
        doc.edit(
            &change(at(0, 1), "Y"),
            cursor(0, 2),
            EditKind::Deleting,
            Millis(9000),
        )
        .unwrap();
        doc.end_undo_group();
        assert_eq!(doc.rope().text(), "XYbc");
        undo(&mut doc);
        assert_eq!(doc.rope().text(), "abc");
        assert!(!doc.can_undo());
    }

    #[test]
    fn starts_a_new_step_after_the_group() {
        let mut doc = Document::new("", FOUR);
        doc.begin_undo_group(None);
        let typed = |col, text: &str| {
            [Change {
                range: Range::at(p(0, col)),
                text: text.to_owned(),
            }]
        };
        doc.edit(&typed(0, "a"), cursor(0, 1), EditKind::Typing, Millis(0))
            .unwrap();
        doc.end_undo_group();
        doc.edit(&typed(1, "b"), cursor(0, 2), EditKind::Typing, Millis(1))
            .unwrap();
        undo(&mut doc);
        assert_eq!(doc.rope().text(), "a");
    }

    #[test]
    fn a_typing_group_may_join_the_typing_before_it_and_stays_open_after() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.begin_undo_group(Some(EditKind::Typing));
        type_text(&mut doc, "b", 10);
        doc.end_undo_group();
        type_text(&mut doc, "c", 20);
        undo(&mut doc);
        assert_eq!(show(&doc), "|");
    }

    // ---- what Rust adds ----

    #[test]
    fn an_edit_that_fails_halfway_changes_nothing() {
        let mut doc = doc_from("ab|");
        let good = Change {
            range: Range::at(p(0, 2)),
            text: "c".into(),
        };
        let bad = Change {
            range: Range::at(p(5, 0)),
            text: "d".into(),
        };
        let result = doc.edit(&[good, bad], cursor(0, 3), EditKind::Typing, Millis(0));
        assert_eq!(
            result,
            Err(EditError::NoSuchLine {
                line: 5,
                line_count: 1
            })
        );
        assert_eq!(show(&doc), "ab|");
        assert!(!doc.can_undo());
        assert_eq!(doc.revision(), 0);
    }

    #[test]
    fn refuses_a_range_inside_a_character() {
        let mut doc = doc_from("é|");
        let inside = Change {
            range: Range::at(p(0, 1)),
            text: "x".into(),
        };
        let result = doc.edit(&[inside], cursor(0, 2), EditKind::Typing, Millis(0));
        assert_eq!(result, Err(EditError::NotACharBoundary { pos: p(0, 1) }));
        assert_eq!(doc.rope().text(), "é");
    }

    #[test]
    fn tells_a_follower_every_replacement_in_bytes() {
        let mut doc = doc_from("ä|b");
        type_text(&mut doc, "x", 0);
        assert!(
            !doc.log.is_followed() && doc.log.changes.is_empty(),
            "nothing is kept until a follower asks"
        );
        let follower = doc.follow();
        assert_eq!(
            doc.changes_since(&follower),
            [],
            "a follower reads from its bookmark on"
        );
        type_text(&mut doc, "ö", 10);
        let change = TextChange {
            start: p(0, 3),
            old_end: p(0, 3),
            new_end: p(0, 5),
            start_byte: 3,
            old_end_byte: 3,
            new_end_byte: 5,
        };
        assert_eq!(doc.changes_since(&follower), [change]);
        undo(&mut doc);
        let undone = doc.changes_since(&follower);
        assert_eq!(
            undone.len(),
            2,
            "undo replays the merged step's two changes backwards"
        );
        assert_eq!(
            (undone[0].start, undone[0].old_end, undone[0].new_end),
            (p(0, 3), p(0, 5), p(0, 3))
        );
        // "äb" is three bytes; "newer" five.
        doc.reset("newer\n");
        let reload = doc.changes_since(&follower);
        assert_eq!((reload[0].old_end_byte, reload[0].new_end_byte), (3, 5));
    }

    #[test]
    fn two_followers_read_the_same_changes_and_neither_takes_them_from_the_other() {
        let mut doc = doc_from("|");
        let early = doc.follow();
        type_text(&mut doc, "a", 0);
        let late = doc.follow();
        type_text(&mut doc, "b", 10);
        assert_eq!(
            doc.changes_since(&late).len(),
            1,
            "only what came after its bookmark"
        );
        assert_eq!(doc.changes_since(&early).len(), 2);
        assert!(doc.log.changes.is_empty(), "read by both: dropped");
        type_text(&mut doc, "c", 20);
        doc.unfollow(late);
        assert_eq!(
            doc.log.changes.len(),
            1,
            "still kept for the one who has not read it"
        );
        assert_eq!(doc.changes_since(&early).len(), 1);
        doc.unfollow(early);
        type_text(&mut doc, "d", 30);
        assert!(doc.log.changes.is_empty(), "nobody follows: nothing kept");
    }

    #[test]
    fn a_follower_of_another_document_reads_nothing_there_and_takes_nothing() {
        let mut a = doc_from("|");
        let mut b = doc_from("|");
        let from_a = a.follow();
        let from_b = b.follow();
        type_text(&mut b, "x", 0);
        assert_eq!(
            b.changes_since(&from_a),
            [],
            "a's follower has no place in b"
        );
        assert_eq!(
            b.changes_since(&from_b).len(),
            1,
            "and took nothing from b's own"
        );
        a.unfollow(from_a);
        b.unfollow(from_b);
    }

    #[test]
    fn clamps_a_selection_into_the_text() {
        let mut doc = doc_from("aé\nb|");
        doc.set_selection(cursor(0, 2), false);
        assert_eq!(
            doc.selection(),
            cursor(0, 1),
            "inside the é, back to its start"
        );
        doc.set_selection(cursor(9, 9), false);
        assert_eq!(doc.selection(), cursor(1, 1));
    }

    #[test]
    fn several_cursors_come_back_with_undo_and_redo() {
        let mut doc = doc_from("|a\nb");
        let others = [cursor(1, 0)];
        doc.set_selections(cursor(0, 0), &others, false);
        let changes = [
            Change {
                range: Range::at(p(1, 0)),
                text: "-".into(),
            },
            Change {
                range: Range::at(p(0, 0)),
                text: "-".into(),
            },
        ];
        doc.edit(&changes, cursor(0, 1), EditKind::Typing, Millis(0))
            .unwrap();
        doc.set_selections(cursor(0, 1), &[cursor(1, 1)], true);
        assert_eq!(doc.rope().text(), "-a\n-b");
        undo(&mut doc);
        assert_eq!((doc.selection(), doc.extra()), (cursor(0, 0), &others[..]));
        redo(&mut doc);
        assert_eq!(
            (doc.selection(), doc.extra()),
            (cursor(0, 1), &[cursor(1, 1)][..])
        );
        assert_eq!(doc.extra_goals(), [None]);
    }

    #[test]
    fn focusing_one_cursor_of_several_keeps_the_typing_step_open() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.focus_cursor(cursor(0, 1), Some(7));
        assert_eq!(doc.goal_column(), Some(7));
        type_text(&mut doc, "b", 10);
        undo(&mut doc);
        assert_eq!(show(&doc), "|");
    }

    #[test]
    fn end_after_counts_bytes_and_lines() {
        assert_eq!(end_after(p(2, 3), "äb"), p(2, 6));
        assert_eq!(end_after(p(2, 3), "x\nyz\n🙂"), p(4, 4));
        assert_eq!(end_after(p(0, 0), ""), p(0, 0));
    }

    #[test]
    fn a_deleting_group_may_join_the_deleting_before_it() {
        let mut doc = doc_from("abc|");
        delete_backward(&mut doc, 0);
        doc.begin_undo_group(Some(EditKind::Deleting));
        delete_backward(&mut doc, 10);
        doc.end_undo_group();
        assert_eq!(show(&doc), "a|");
        undo(&mut doc);
        assert_eq!(show(&doc), "abc|");
    }

    #[test]
    fn setting_every_cursor_anew_seals_the_typing_step() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.set_selections(cursor(0, 1), &[], false);
        type_text(&mut doc, "b", 10);
        undo(&mut doc);
        assert_eq!(show(&doc), "a|");
    }

    #[test]
    fn a_saved_step_takes_no_more_typing_even_inside_a_typing_group() {
        let mut doc = doc_from("|");
        type_text(&mut doc, "a", 0);
        doc.mark_saved();
        doc.begin_undo_group(Some(EditKind::Typing));
        type_text(&mut doc, "b", 10);
        doc.end_undo_group();
        undo(&mut doc);
        assert_eq!(show(&doc), "a|");
        assert!(!doc.dirty());
    }

    #[test]
    fn a_reload_keeps_the_cursor_off_the_middle_of_a_character() {
        let mut doc = doc_from("abcd|");
        doc.reset("aé\n");
        assert_eq!(doc.selection(), cursor(0, 3));
        doc.set_selection(cursor(0, 3), false);
        doc.reset("é");
        assert_eq!(
            doc.selection(),
            cursor(0, 2),
            "column 3 lies past the end, at it"
        );
        doc.reset("aé");
        doc.set_selection(cursor(0, 3), false);
        doc.reset("ééé");
        assert_eq!(
            doc.selection(),
            cursor(0, 2),
            "column 3 lies inside the second é, back to its start"
        );
    }

    #[test]
    fn keeps_the_goal_columns_of_every_cursor() {
        let mut doc = doc_from("|a\nb\nc");
        doc.set_selections(cursor(0, 0), &[cursor(1, 0), cursor(2, 0)], false);
        doc.set_extra_goals(vec![Some(4)]);
        assert_eq!(
            doc.extra_goals(),
            [Some(4), None],
            "filled up to one goal per other cursor"
        );
        doc.set_extra_goals(vec![Some(1), Some(2), Some(3)]);
        assert_eq!(
            doc.extra_goals(),
            [Some(1), Some(2)],
            "cut to one goal per other cursor"
        );
        doc.set_goal_column(Some(9));
        assert_eq!(doc.goal_column(), Some(9));
        doc.set_selection(cursor(0, 0), true);
        assert_eq!(doc.goal_column(), Some(9), "kept when asked");
        assert_eq!(doc.extra_goals(), []);
    }

    /// What a person does to a document, for the random test below.
    #[derive(Debug, Clone)]
    enum Act {
        Type(char),
        Delete,
        Paste(usize),
        Jump(u16, u16),
        Wait(u64),
        Save,
        Undo,
        Redo,
    }

    fn act() -> impl proptest::strategy::Strategy<Value = Act> {
        use proptest::prelude::*;
        prop_oneof![
            4 => prop::sample::select(vec!['a', 'é', '🙂', ' ', '\n']).prop_map(Act::Type),
            2 => Just(Act::Delete),
            1 => (0..3_usize).prop_map(Act::Paste),
            1 => any::<(u16, u16)>().prop_map(|(l, c)| Act::Jump(l, c)),
            1 => (0..2500_u64).prop_map(Act::Wait),
            1 => Just(Act::Save),
            1 => Just(Act::Undo),
            1 => Just(Act::Redo),
        ]
    }

    fn jump(doc: &mut Document, line: u16, col: u16) {
        let rope = doc.rope();
        let line = usize::from(line) * rope.line_count() / (usize::from(u16::MAX) + 1);
        let len = rope.line(line).unwrap().len();
        let col = usize::from(col) * (len + 1) / (usize::from(u16::MAX) + 1);
        let at = rope.clamp(p(line, col));
        doc.set_selection(Selection::cursor(at), false);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

        #[test]
        fn undoing_everything_gives_the_opened_text_back_and_redoing_everything_the_last(
            acts in proptest::collection::vec(act(), 1..120)
        ) {
            const OPENED: &str = "fn main() {\n    let größe = 1;\n}";
            const PASTES: [&str; 3] = ["x", "one\ntwo", "🙂\r\nä"];
            let mut doc = Document::new(OPENED, FOUR);
            let mut now = 0;
            for act in &acts {
                match *act {
                    Act::Type(ch) => type_text(&mut doc, ch.encode_utf8(&mut [0; 4]), now),
                    Act::Delete => delete_backward(&mut doc, now),
                    Act::Paste(i) => insert(&mut doc, PASTES[i], EditKind::Other, now),
                    Act::Jump(line, col) => jump(&mut doc, line, col),
                    Act::Wait(ms) => now += ms,
                    Act::Save => {
                        doc.mark_saved();
                        proptest::prop_assert!(!doc.dirty());
                    }
                    Act::Undo => { undo(&mut doc); }
                    Act::Redo => { redo(&mut doc); }
                }
                now += 10;
                proptest::prop_assert!(doc.rope().check_pos(doc.selection().head).is_ok());
            }
            // The newest text the history holds: what is left to redo, redone.
            while redo(&mut doc) {}
            let last = doc.rope().text().to_owned();
            while undo(&mut doc) {}
            proptest::prop_assert_eq!(doc.rope().text(), OPENED);
            while redo(&mut doc) {}
            proptest::prop_assert_eq!(doc.rope().text(), last);
            proptest::prop_assert_eq!(doc.rope().check_invariants(), Ok(()));
        }
    }
}
