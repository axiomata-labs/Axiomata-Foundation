//! The rope (Axiomata-OS `editor/rope.ts`): the text store for files of any size the editor edits.
//!
//! A balanced tree whose leaves hold runs of whole lines. Every node knows how many lines it holds and how many bytes
//! they have (line breaks not counted), so finding line `i`, or turning a position into an offset, walks one path
//! from the root: O(log n). An edit rebuilds only the path to the lines it touches; everything else is shared with
//! the previous version.
//!
//! Three decisions carry it (rust-core.md R3, R4):
//!
//! * **Lines, not characters, are the unit.** The whole editor (positions, drawing, tree-sitter's input) asks by line
//!   and column; a character rope would have to find line breaks for every one of those questions. A very long line
//!   is a single string in a leaf, and an edit inside it copies that string.
//! * **Nodes never change once built.** A [`Rope`] is a value: [`Rope::replace`] returns a new one and leaves the old
//!   one intact, sharing what did not change (`Arc`). A clone is a snapshot another thread may read while the
//!   document goes on changing. Lines are shared too, so rebuilding a leaf never copies the text of its other lines.
//! * **A B-tree keeps it balanced**: all leaves sit at the same depth, leaves hold up to [`MAX_LEAF_LINES`] lines,
//!   branches up to [`MAX_CHILDREN`] children. An under-filled node is merged into a neighbour on the way back up, an
//!   over-full one split; the root grows or shrinks by one level at a time.

use std::fmt;
use std::sync::{Arc, OnceLock};

use crate::detect::split_lines;
use crate::error::EditError;
use crate::position::{Pos, Range, floor_char_boundary};

/// Most lines one leaf holds; a leaf built fresh gets half of it.
const MAX_LEAF_LINES: usize = 64;
/// Most children one branch holds.
const MAX_CHILDREN: usize = 16;
const LEAF_TARGET: usize = MAX_LEAF_LINES / 2;
const MIN_LEAF_LINES: usize = MAX_LEAF_LINES / 4;
const MIN_CHILDREN: usize = MAX_CHILDREN / 4;

/// One line, without its line break, shared between the versions that hold it.
type Line = Arc<str>;

enum Node {
    Leaf {
        lines: Vec<Line>,
        /// Bytes in all lines, breaks not counted.
        bytes: usize,
    },
    Branch {
        children: Vec<Arc<Node>>,
        line_count: usize,
        bytes: usize,
        /// Levels below this one; every leaf under it is this far down.
        height: usize,
    },
}

impl Node {
    fn leaf(lines: Vec<Line>) -> Arc<Self> {
        let bytes = lines.iter().map(|line| line.len()).sum();
        Arc::new(Self::Leaf { lines, bytes })
    }

    /// A branch over same-height `children`, at least one.
    fn branch(children: Vec<Arc<Self>>) -> Arc<Self> {
        let line_count = children.iter().map(|child| child.line_count()).sum();
        let bytes = children.iter().map(|child| child.bytes()).sum();
        let height = children.first().map_or(0, |child| child.height()) + 1;
        Arc::new(Self::Branch {
            children,
            line_count,
            bytes,
            height,
        })
    }

    fn line_count(&self) -> usize {
        match self {
            Self::Leaf { lines, .. } => lines.len(),
            Self::Branch { line_count, .. } => *line_count,
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Leaf { bytes, .. } | Self::Branch { bytes, .. } => *bytes,
        }
    }

    fn height(&self) -> usize {
        match self {
            Self::Leaf { .. } => 0,
            Self::Branch { height, .. } => *height,
        }
    }

    fn underfull(&self) -> bool {
        match self {
            Self::Leaf { lines, .. } => lines.len() < MIN_LEAF_LINES,
            Self::Branch { children, .. } => children.len() < MIN_CHILDREN,
        }
    }
}

/// `items` cut into runs of `target`, the last run taking a short remainder rather than standing alone.
fn chunk<T: Clone>(items: &[T], max: usize, target: usize) -> Vec<Vec<T>> {
    if items.len() <= max {
        return vec![items.to_vec()];
    }
    let mut out: Vec<Vec<T>> = items.chunks(target).map(<[T]>::to_vec).collect();
    if out.len() > 1 && out.last().is_some_and(|last| last.len() < target / 2) {
        let last = out.pop().unwrap_or_default();
        if let Some(before) = out.last_mut() {
            before.extend(last);
        }
    }
    out
}

/// Leaves for `lines`, each at most [`MAX_LEAF_LINES`].
fn leaves_of(lines: &[Line]) -> Vec<Arc<Node>> {
    if lines.is_empty() {
        return Vec::new();
    }
    chunk(lines, MAX_LEAF_LINES, LEAF_TARGET)
        .into_iter()
        .map(Node::leaf)
        .collect()
}

/// Branches over same-height `nodes`, each at most [`MAX_CHILDREN`] children.
fn branches_of(nodes: &[Arc<Node>]) -> Vec<Arc<Node>> {
    if nodes.is_empty() {
        return Vec::new();
    }
    chunk(nodes, MAX_CHILDREN, MAX_CHILDREN / 2)
        .into_iter()
        .map(Node::branch)
        .collect()
}

/// Two same-height neighbours as one node, or as two again when that overflows.
fn merge_pair(a: &Node, b: &Node) -> Vec<Arc<Node>> {
    match (a, b) {
        (Node::Leaf { lines: first, .. }, Node::Leaf { lines: second, .. }) => {
            leaves_of(&[first.as_slice(), second.as_slice()].concat())
        }
        (
            Node::Branch {
                children: first, ..
            },
            Node::Branch {
                children: second, ..
            },
        ) => branches_of(&rebalance([first.as_slice(), second.as_slice()].concat())),
        _ => unreachable!("siblings always sit at the same height"),
    }
}

/// Siblings with every under-filled one merged into a neighbour.
fn rebalance(nodes: Vec<Arc<Node>>) -> Vec<Arc<Node>> {
    if nodes.len() < 2 {
        return nodes;
    }
    let mut out: Vec<Arc<Node>> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let merges = out
            .last()
            .is_some_and(|prev| prev.underfull() || node.underfull());
        match out.pop() {
            Some(prev) if merges => out.extend(merge_pair(&prev, &node)),
            Some(prev) => {
                out.push(prev);
                out.push(node);
            }
            None => out.push(node),
        }
    }
    out
}

/// `node` with `count` lines from line `from` replaced by `insert`, as zero or more nodes of the same height (fewer
/// when lines went, more when it split). `from` names an existing line and `count` is at least 1:
/// [`Rope::replace`] always replaces the lines its range touches, so there is no pure append.
fn splice(node: &Node, from: usize, count: usize, insert: &[Line]) -> Vec<Arc<Node>> {
    let children = match node {
        Node::Leaf { lines, .. } => {
            return leaves_of(&[&lines[..from], insert, &lines[from + count..]].concat());
        }
        Node::Branch { children, .. } => children,
    };
    let end = from + count;
    let mut out = Vec::with_capacity(children.len() + 1);
    let mut start = 0;
    let mut inserted = false;
    for child in children {
        let child_end = start + child.line_count();
        if !inserted && from < child_end {
            // The child holding line `from` takes the insertion and the part of the deletion inside it.
            out.extend(splice(
                child,
                from - start,
                end.min(child_end) - from,
                insert,
            ));
            inserted = true;
        } else if inserted && start < end {
            // A later child the deletion still reaches into, from its first line.
            out.extend(splice(child, 0, end.min(child_end) - start, &[]));
        } else {
            out.push(Arc::clone(child));
        }
        start = child_end;
    }
    branches_of(&rebalance(out))
}

/// The line at index `i` of `node`, if there is one.
fn line_at(node: &Node, i: usize) -> Option<&str> {
    let mut current = node;
    let mut index = i;
    loop {
        match current {
            Node::Leaf { lines, .. } => return lines.get(index).map(|line| &**line),
            Node::Branch { children, .. } => {
                let mut next = None;
                for child in children {
                    let n = child.line_count();
                    if index < n {
                        next = Some(child);
                        break;
                    }
                    index -= n;
                }
                current = next?;
            }
        }
    }
}

/// Bytes in the lines before line `i`, breaks not counted.
fn bytes_before(node: &Node, i: usize) -> usize {
    let mut current = node;
    let mut index = i;
    let mut bytes = 0;
    loop {
        match current {
            Node::Leaf { lines, .. } => {
                return bytes
                    + lines
                        .iter()
                        .take(index)
                        .map(|line| line.len())
                        .sum::<usize>();
            }
            Node::Branch { children, .. } => {
                let mut next = None;
                for child in children {
                    let n = child.line_count();
                    if index < n {
                        next = Some(child);
                        break;
                    }
                    index -= n;
                    bytes += child.bytes();
                }
                match next {
                    Some(child) => current = child,
                    None => return bytes,
                }
            }
        }
    }
}

/// The line holding byte `offset` (at most the text's length) of the joined text, and how far into the text that
/// line starts. A line break belongs to the line it ends; the last child or line takes anything beyond.
fn line_at_offset(node: &Node, offset: usize) -> (usize, usize) {
    let mut current = node;
    let mut line = 0;
    let mut start = 0;
    loop {
        match current {
            Node::Branch { children, .. } => {
                let mut k = 0;
                // Every line in a child plus one break after each.
                while k + 1 < children.len() {
                    let span = children[k].bytes() + children[k].line_count();
                    if offset < start + span {
                        break;
                    }
                    start += span;
                    line += children[k].line_count();
                    k += 1;
                }
                current = &children[k];
            }
            Node::Leaf { lines, .. } => {
                let mut k = 0;
                while k + 1 < lines.len() && offset > start + lines[k].len() {
                    start += lines[k].len() + 1;
                    k += 1;
                }
                return (line + k, start);
            }
        }
    }
}

/// Calls `visit` with every line in `from..=to` of `node`, in order; bounds beyond the node are clamped.
fn each_line<'a>(node: &'a Node, from: usize, to: usize, visit: &mut impl FnMut(&'a str)) {
    match node {
        Node::Leaf { lines, .. } => {
            for line in lines.iter().take(to.saturating_add(1)).skip(from) {
                visit(line);
            }
        }
        Node::Branch { children, .. } => {
            let mut start = 0;
            for child in children {
                if start > to {
                    break;
                }
                let n = child.line_count();
                if start + n > from {
                    each_line(child, from.saturating_sub(start), to - start, visit);
                }
                start += n;
            }
        }
    }
}

/// What [`Rope::replace`] returns: the next version, and the position right after the inserted text.
#[derive(Debug, Clone)]
pub struct Replaced {
    /// The text after the replacement.
    pub rope: Rope,
    /// The position right after the inserted text, in `rope`.
    pub end: Pos,
}

/// One version of a text. Never changes; [`Rope::replace`] returns the next one. A clone is cheap and shares
/// everything with the original, including the joined text once one of them has built it.
#[derive(Clone)]
pub struct Rope {
    root: Arc<Node>,
    /// The whole text, built on the first [`Rope::text`] of this version.
    joined: Arc<OnceLock<String>>,
}

impl Rope {
    /// A rope over `text`, split on LF, CRLF or a lone CR.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let lines: Vec<Line> = split_lines(text).into_iter().map(Arc::from).collect();
        Self::from_level(leaves_of(&lines))
    }

    /// The rope over one level of same-height nodes, built up to a single root.
    fn from_level(mut level: Vec<Arc<Node>>) -> Self {
        while level.len() > 1 {
            level = branches_of(&level);
        }
        // An empty level cannot come from a text, which is at least one line; an empty line stands in all the same.
        let mut root = level
            .pop()
            .unwrap_or_else(|| Node::leaf(vec![Arc::from("")]));
        // A root left with one child is a level too many.
        while let Node::Branch { children, .. } = &*root
            && children.len() == 1
        {
            root = Arc::clone(&children[0]);
        }
        Self {
            root,
            joined: Arc::new(OnceLock::new()),
        }
    }

    /// How many lines the text has; at least 1, as an empty text is one empty line.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.root.line_count()
    }

    /// Every byte of the text, each line break counted as one.
    #[must_use]
    pub fn len(&self) -> usize {
        self.root.bytes() + self.line_count() - 1
    }

    /// Whether the text is empty: one empty line.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The text of line `i`, without its line break.
    ///
    /// # Errors
    /// [`EditError::NoSuchLine`] when the text has no line `i`.
    pub fn line(&self, i: usize) -> Result<&str, EditError> {
        line_at(&self.root, i).ok_or(EditError::NoSuchLine {
            line: i,
            line_count: self.line_count(),
        })
    }

    /// The text of lines `from` to `to` (inclusive), in order. Bounds beyond the text are clamped, and a reversed
    /// pair gives nothing — unlike [`Rope::line`], which refuses a line that does not exist.
    #[must_use]
    pub fn lines(&self, from: usize, to: usize) -> Vec<&str> {
        let mut out = Vec::new();
        if from <= to {
            each_line(&self.root, from, to, &mut |line| out.push(line));
        }
        out
    }

    /// The text in `r`, lines joined with `\n`.
    ///
    /// # Errors
    /// When `r` is reversed, or one of its ends lies outside the text or inside a character.
    pub fn slice(&self, r: Range) -> Result<String, EditError> {
        self.check_range(r)?;
        let (start, end) = (r.start, r.end);
        if start.line == end.line {
            return Ok(self.line(start.line)?[start.col..end.col].to_owned());
        }
        let lines = self.lines(start.line, end.line);
        let last = lines.len() - 1;
        let mut out = String::new();
        for (k, line) in lines.into_iter().enumerate() {
            let from = if k == 0 { start.col } else { 0 };
            let to = if k == last { end.col } else { line.len() };
            if k > 0 {
                out.push('\n');
            }
            out.push_str(&line[from..to]);
        }
        Ok(out)
    }

    /// Replaces the text in `r` with `text` (which may hold line breaks of any kind) and returns the next version.
    ///
    /// # Errors
    /// When `r` is reversed, or one of its ends lies outside the text or inside a character.
    pub fn replace(&self, r: Range, text: &str) -> Result<Replaced, EditError> {
        self.check_range(r)?;
        let (start, end) = (r.start, r.end);
        let first = self.line(start.line)?;
        let last = if start.line == end.line {
            first
        } else {
            self.line(end.line)?
        };
        let parts = split_lines(text);
        let tail = parts.len() - 1;
        let end_col = if tail == 0 { start.col } else { 0 } + parts[tail].len();
        let mut inserted: Vec<Line> = Vec::with_capacity(parts.len());
        for (k, part) in parts.iter().enumerate() {
            let before = if k == 0 { &first[..start.col] } else { "" };
            let after = if k == tail { &last[end.col..] } else { "" };
            inserted.push(Arc::from([before, part, after].concat()));
        }
        let level = splice(&self.root, start.line, end.line - start.line + 1, &inserted);
        Ok(Replaced {
            rope: Self::from_level(level),
            end: Pos::new(start.line + tail, end_col),
        })
    }

    /// The whole text, lines joined with `\n` — built once per version and shared by its clones.
    #[must_use]
    pub fn text(&self) -> &str {
        self.joined
            .get_or_init(|| self.lines(0, self.line_count() - 1).join("\n"))
    }

    /// `p` as a byte offset into [`Rope::text`], each line break counted as one.
    ///
    /// # Errors
    /// When `p` lies outside the text or inside a character.
    pub fn offset_at(&self, p: Pos) -> Result<usize, EditError> {
        self.check_pos(p)?;
        Ok(bytes_before(&self.root, p.line) + p.line + p.col)
    }

    /// The position of byte `offset` into [`Rope::text`], clamped into the text and, inside a character, moved back
    /// to where that character starts.
    #[must_use]
    pub fn pos_at(&self, offset: usize) -> Pos {
        let at = offset.min(self.len());
        let (line, start) = line_at_offset(&self.root, at);
        let text = self.line(line).unwrap_or_default();
        Pos::new(line, floor_char_boundary(text, at - start))
    }

    /// The position after the last character.
    #[must_use]
    pub fn end(&self) -> Pos {
        let last = self.line_count() - 1;
        Pos::new(last, self.line(last).map_or(0, str::len))
    }

    /// `p` moved into the text: the line clamped to what exists, the column to the line's end and, inside a
    /// character, back to where it starts.
    #[must_use]
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.line_count() - 1);
        let text = self.line(line).unwrap_or_default();
        Pos::new(line, floor_char_boundary(text, p.col.min(text.len())))
    }

    /// Whether `p` names a place in the text, on a character boundary.
    ///
    /// # Errors
    /// [`EditError::NoSuchLine`], [`EditError::PastLineEnd`] or [`EditError::NotACharBoundary`].
    pub fn check_pos(&self, p: Pos) -> Result<(), EditError> {
        let line = self.line(p.line)?;
        if p.col > line.len() {
            return Err(EditError::PastLineEnd {
                pos: p,
                line_len: line.len(),
            });
        }
        if !line.is_char_boundary(p.col) {
            return Err(EditError::NotACharBoundary { pos: p });
        }
        Ok(())
    }

    /// Whether `r` runs forwards between two places in the text.
    ///
    /// # Errors
    /// [`EditError::Reversed`], or what [`Rope::check_pos`] finds at either end.
    pub fn check_range(&self, r: Range) -> Result<(), EditError> {
        if r.start > r.end {
            return Err(EditError::Reversed {
                start: r.start,
                end: r.end,
            });
        }
        self.check_pos(r.start)?;
        self.check_pos(r.end)
    }

    /// Checks the tree's invariants and names the first one broken: every leaf at the same depth, no node over its
    /// limit, no node but the root under its minimum, cached counts matching what is below them.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) -> Result<(), String> {
        fn visit(node: &Node, is_root: bool) -> Result<usize, String> {
            match node {
                Node::Leaf { lines, bytes } => {
                    if lines.is_empty() || lines.len() > MAX_LEAF_LINES {
                        return Err(format!("leaf of {} lines", lines.len()));
                    }
                    if !is_root && lines.len() < MIN_LEAF_LINES {
                        return Err(format!("leaf under-filled: {} lines", lines.len()));
                    }
                    if *bytes != lines.iter().map(|line| line.len()).sum::<usize>() {
                        return Err("leaf bytes wrong".into());
                    }
                    if lines.iter().any(|line| line.contains(['\n', '\r'])) {
                        return Err("a line holds a line break".into());
                    }
                    Ok(0)
                }
                Node::Branch {
                    children,
                    line_count,
                    bytes,
                    height,
                } => {
                    if children.len() > MAX_CHILDREN {
                        return Err(format!("branch over-full: {}", children.len()));
                    }
                    if children.len() < if is_root { 2 } else { MIN_CHILDREN } {
                        return Err(format!("branch under-filled: {}", children.len()));
                    }
                    for child in children {
                        if visit(child, false)? + 1 != *height {
                            return Err("unbalanced".into());
                        }
                    }
                    if *line_count
                        != children
                            .iter()
                            .map(|child| child.line_count())
                            .sum::<usize>()
                    {
                        return Err("line count wrong".into());
                    }
                    if *bytes != children.iter().map(|child| child.bytes()).sum::<usize>() {
                        return Err("branch bytes wrong".into());
                    }
                    Ok(*height)
                }
            }
        }
        visit(&self.root, true).map(|_| ())
    }

    /// How many levels the tree has.
    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        self.root.height() + 1
    }
}

impl Default for Rope {
    fn default() -> Self {
        Self::new("")
    }
}

impl fmt::Debug for Rope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rope")
            .field("lines", &self.line_count())
            .field("bytes", &self.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_rope_is_a_value_any_thread_may_hold() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Rope>();
        send_sync::<crate::Document>();
    }

    /// A plain line store the rope is checked against: easy to reason about, slow on purpose.
    struct LineStore {
        lines: Vec<String>,
    }

    impl LineStore {
        fn new(text: &str) -> Self {
            Self {
                lines: split_lines(text).into_iter().map(str::to_owned).collect(),
            }
        }
    }

    /// What both stores answer, so one suite runs against each (`buffer.test.ts`).
    trait Store {
        fn line_count(&self) -> usize;
        fn line(&self, i: usize) -> Result<String, EditError>;
        fn slice(&self, r: Range) -> Result<String, EditError>;
        fn replace(&mut self, r: Range, text: &str) -> Result<Pos, EditError>;
        fn text(&self) -> String;
        fn offset_at(&self, p: Pos) -> Result<usize, EditError>;
        fn end(&self) -> Pos;
        fn clamp(&self, p: Pos) -> Pos;
    }

    impl Store for LineStore {
        fn line_count(&self) -> usize {
            self.lines.len()
        }

        fn line(&self, i: usize) -> Result<String, EditError> {
            self.lines.get(i).cloned().ok_or(EditError::NoSuchLine {
                line: i,
                line_count: self.lines.len(),
            })
        }

        fn slice(&self, r: Range) -> Result<String, EditError> {
            self.checked(r)?;
            if r.start.line == r.end.line {
                return Ok(self.lines[r.start.line][r.start.col..r.end.col].to_owned());
            }
            let mut parts = vec![&self.lines[r.start.line][r.start.col..]];
            parts.extend(
                self.lines[r.start.line + 1..r.end.line]
                    .iter()
                    .map(String::as_str),
            );
            parts.push(&self.lines[r.end.line][..r.end.col]);
            Ok(parts.join("\n"))
        }

        fn replace(&mut self, r: Range, text: &str) -> Result<Pos, EditError> {
            self.checked(r)?;
            let before = self.lines[r.start.line][..r.start.col].to_owned();
            let after = self.lines[r.end.line][r.end.col..].to_owned();
            let mut inserted: Vec<String> =
                split_lines(text).into_iter().map(str::to_owned).collect();
            let last = inserted.len() - 1;
            let end_col = if last == 0 { before.len() } else { 0 } + inserted[last].len();
            inserted[0].insert_str(0, &before);
            inserted[last].push_str(&after);
            self.lines.splice(r.start.line..=r.end.line, inserted);
            Ok(Pos::new(r.start.line + last, end_col))
        }

        fn text(&self) -> String {
            self.lines.join("\n")
        }

        fn offset_at(&self, p: Pos) -> Result<usize, EditError> {
            self.checked(Range::at(p))?;
            Ok(self.lines[..p.line]
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>()
                + p.col)
        }

        fn end(&self) -> Pos {
            Pos::new(self.lines.len() - 1, self.lines[self.lines.len() - 1].len())
        }

        fn clamp(&self, p: Pos) -> Pos {
            let line = p.line.min(self.lines.len() - 1);
            Pos::new(line, floor_char_boundary(&self.lines[line], p.col))
        }
    }

    impl LineStore {
        fn checked(&self, r: Range) -> Result<(), EditError> {
            if r.start > r.end {
                return Err(EditError::Reversed {
                    start: r.start,
                    end: r.end,
                });
            }
            for p in [r.start, r.end] {
                let line = self.line(p.line)?;
                if p.col > line.len() {
                    return Err(EditError::PastLineEnd {
                        pos: p,
                        line_len: line.len(),
                    });
                }
                if !line.is_char_boundary(p.col) {
                    return Err(EditError::NotACharBoundary { pos: p });
                }
            }
            Ok(())
        }
    }

    /// The editor's way of holding a rope: the current version, replaced by every edit.
    struct RopeStore(Rope);

    impl Store for RopeStore {
        fn line_count(&self) -> usize {
            self.0.line_count()
        }

        fn line(&self, i: usize) -> Result<String, EditError> {
            self.0.line(i).map(str::to_owned)
        }

        fn slice(&self, r: Range) -> Result<String, EditError> {
            self.0.slice(r)
        }

        fn replace(&mut self, r: Range, text: &str) -> Result<Pos, EditError> {
            let next = self.0.replace(r, text)?;
            self.0 = next.rope;
            Ok(next.end)
        }

        fn text(&self) -> String {
            self.0.text().to_owned()
        }

        fn offset_at(&self, p: Pos) -> Result<usize, EditError> {
            self.0.offset_at(p)
        }

        fn end(&self) -> Pos {
            self.0.end()
        }

        fn clamp(&self, p: Pos) -> Pos {
            self.0.clamp(p)
        }
    }

    fn p(line: usize, col: usize) -> Pos {
        Pos::new(line, col)
    }

    fn r(a: Pos, b: Pos) -> Range {
        Range::new(a, b)
    }

    fn stores(text: &str) -> [Box<dyn Store>; 2] {
        [
            Box::new(LineStore::new(text)),
            Box::new(RopeStore(Rope::new(text))),
        ]
    }

    fn numbered(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ok(rope: &Rope) {
        if let Err(broken) = rope.check_invariants() {
            panic!("rope invariant broken: {broken}");
        }
    }

    // ---- the suite both stores pass (buffer.test.ts) ----

    #[test]
    fn keeps_an_empty_document_as_one_empty_line() {
        for s in stores("") {
            assert_eq!(s.line_count(), 1);
            assert_eq!(s.line(0).unwrap(), "");
            assert_eq!(s.end(), p(0, 0));
        }
    }

    #[test]
    fn splits_on_lf_crlf_and_a_lone_cr() {
        for s in stores("a\r\nb") {
            assert_eq!(s.text(), "a\nb");
        }
    }

    #[test]
    fn slices_within_a_line_and_across_lines() {
        for s in stores("alpha\nbeta\ngamma") {
            assert_eq!(s.slice(r(p(0, 1), p(0, 3))).unwrap(), "lp");
            assert_eq!(s.slice(r(p(0, 3), p(2, 2))).unwrap(), "ha\nbeta\nga");
            assert_eq!(s.slice(r(p(1, 4), p(2, 0))).unwrap(), "\n");
        }
    }

    #[test]
    fn replaces_and_returns_the_position_after_the_inserted_text() {
        for mut s in stores("alpha\nbeta") {
            assert_eq!(s.replace(r(p(0, 2), p(0, 4)), "XY").unwrap(), p(0, 4));
            assert_eq!(s.text(), "alXYa\nbeta");
            assert_eq!(s.replace(r(p(0, 5), p(1, 0)), "").unwrap(), p(0, 5));
            assert_eq!(s.text(), "alXYabeta");
            assert_eq!(s.replace(r(p(0, 2), p(0, 2)), "1\n22\n3").unwrap(), p(2, 1));
            assert_eq!(s.text(), "al1\n22\n3XYabeta");
        }
    }

    #[test]
    fn refuses_a_reversed_range_or_a_position_outside_the_text() {
        for mut s in stores("ab\ncd") {
            assert_eq!(
                s.slice(r(p(0, 0), p(0, 3))),
                Err(EditError::PastLineEnd {
                    pos: p(0, 3),
                    line_len: 2
                })
            );
            let reversed = Range {
                start: p(1, 0),
                end: p(0, 0),
            };
            assert_eq!(
                s.replace(reversed, ""),
                Err(EditError::Reversed {
                    start: p(1, 0),
                    end: p(0, 0)
                })
            );
            assert_eq!(
                s.line(2),
                Err(EditError::NoSuchLine {
                    line: 2,
                    line_count: 2
                })
            );
            assert_eq!(s.text(), "ab\ncd", "a refused replace changes nothing");
        }
    }

    #[test]
    fn refuses_a_position_inside_a_character() {
        // "é" is two bytes, "🙂" four: a column between them names no character.
        for mut s in stores("é🙂x") {
            assert_eq!(
                s.slice(r(p(0, 1), p(0, 2))),
                Err(EditError::NotACharBoundary { pos: p(0, 1) })
            );
            assert_eq!(
                s.replace(r(p(0, 2), p(0, 4)), ""),
                Err(EditError::NotACharBoundary { pos: p(0, 4) })
            );
            assert_eq!(
                s.offset_at(p(0, 3)),
                Err(EditError::NotACharBoundary { pos: p(0, 3) })
            );
            assert_eq!(s.slice(r(p(0, 2), p(0, 6))).unwrap(), "🙂");
            assert_eq!(s.text(), "é🙂x");
        }
    }

    #[test]
    fn turns_a_position_into_an_offset_into_the_joined_text() {
        for s in stores("ab\ncdé\n") {
            assert_eq!(s.offset_at(p(0, 0)).unwrap(), 0);
            assert_eq!(s.offset_at(p(1, 2)).unwrap(), 5);
            // UTF-16: 7 — "é" is one unit there, two bytes here.
            assert_eq!(s.offset_at(p(2, 0)).unwrap(), 8);
            let text = s.text();
            let (from, to) = (s.offset_at(p(1, 0)).unwrap(), s.offset_at(p(1, 4)).unwrap());
            assert_eq!(&text[from..to], "cdé");
        }
    }

    #[test]
    fn clamps_a_position_into_the_text() {
        for s in stores("ab\ncdef") {
            // TS clamped a negative line too; a `usize` has none.
            assert_eq!(s.clamp(p(0, 5)), p(0, 2));
            assert_eq!(s.clamp(p(9, 9)), p(1, 4));
        }
        for s in stores("aé") {
            assert_eq!(
                s.clamp(p(0, 2)),
                p(0, 1),
                "inside a character, back to its start"
            );
        }
    }

    // ---- the rope's own cases (rope.test.ts) ----

    #[test]
    fn builds_balanced_trees_of_any_size() {
        for n in [1, 2, MAX_LEAF_LINES, MAX_LEAF_LINES + 1, 1000, 100_000] {
            let rope = Rope::new(&numbered(n));
            ok(&rope);
            assert_eq!(rope.line_count(), n);
            assert_eq!(rope.line(n - 1).unwrap(), format!("line {}", n - 1));
        }
        // 100 000 lines stay a shallow tree: half-full 16-way branches over 32-line leaves.
        assert!(Rope::new(&numbered(100_000)).depth() <= 5);
    }

    #[test]
    fn leaves_an_earlier_version_untouched_a_snapshot_is_a_fixed_text() {
        let before = Rope::new(&numbered(500));
        let after = before
            .replace(r(p(10, 0), p(400, 0)), "gone\n")
            .unwrap()
            .rope;
        assert_eq!(before.line_count(), 500);
        assert_eq!(before.line(200).unwrap(), "line 200");
        // Lines 0–9, "gone", then 400–499.
        assert_eq!(after.line_count(), 111);
        assert_eq!(after.line(10).unwrap(), "gone");
    }

    #[test]
    fn a_snapshot_can_be_read_on_another_thread() {
        let rope = Rope::new(&numbered(5000));
        let snapshot = rope.clone();
        let reader = std::thread::spawn(move || snapshot.text().len());
        let next = rope.replace(r(p(0, 0), p(4999, 0)), "").unwrap().rope;
        assert_eq!(reader.join().unwrap(), numbered(5000).len());
        assert_eq!(next.text(), "line 4999");
    }

    #[test]
    fn deletes_everything_down_to_one_empty_line() {
        let rope = Rope::new(&numbered(5000));
        let next = rope.replace(r(p(0, 0), rope.end()), "").unwrap().rope;
        ok(&next);
        assert_eq!(next.line_count(), 1);
        assert_eq!(next.text(), "");
        assert!(next.is_empty());
    }

    #[test]
    fn counts_its_length_with_each_line_break_as_one() {
        // UTF-16: 7.
        assert_eq!(Rope::new("ab\ncdé\n").len(), 8);
        assert_eq!(Rope::new("").len(), 0);
    }

    #[test]
    fn holds_a_single_very_long_line_without_splitting_it() {
        let long = "x".repeat(500_000);
        let rope = Rope::new(&long);
        ok(&rope);
        assert_eq!(rope.line_count(), 1);
        assert_eq!(rope.line(0).unwrap(), long);
        assert_eq!(
            rope.slice(r(p(0, 100_000), p(0, 100_010))).unwrap(),
            long[100_000..100_010]
        );
        assert_eq!(rope.offset_at(p(0, 250_000)).unwrap(), 250_000);
        let next = rope
            .replace(Range::at(p(0, 100_000)), "INSERTED")
            .unwrap()
            .rope;
        ok(&next);
        assert_eq!(
            next.line(0).unwrap(),
            [&long[..100_000], "INSERTED", &long[100_000..]].concat()
        );
    }

    #[test]
    fn appends_after_the_very_last_line_of_a_large_multi_level_document() {
        let n = 100_000;
        let text = numbered(n);
        let mut lines = LineStore::new(&text);
        let mut rope = RopeStore(Rope::new(&text));
        // The last leaf sits under a chain of branches that are each the last child at their level — exactly the
        // path an append walks.
        assert!(rope.0.depth() > 1);
        let end = lines.end();
        let expected = lines.replace(Range::at(end), "\nnew last line").unwrap();
        assert_eq!(
            rope.replace(Range::at(end), "\nnew last line").unwrap(),
            expected
        );
        ok(&rope.0);
        assert_eq!(rope.line_count(), n + 1);
        assert_eq!(rope.line(n).unwrap(), "new last line");
        assert_eq!(rope.text(), lines.text());
    }

    #[test]
    fn deletes_a_range_spanning_many_leaves_and_whole_subtrees() {
        let text = numbered(100_000);
        let mut lines = LineStore::new(&text);
        let mut rope = RopeStore(Rope::new(&text));
        let gone = r(p(1000, 2), p(90_000, 3));
        let expected = lines.replace(gone, "").unwrap();
        assert_eq!(rope.replace(gone, "").unwrap(), expected);
        ok(&rope.0);
        assert_eq!(rope.line_count(), lines.line_count());
        assert_eq!(rope.text(), lines.text());
        // The tree shrank along with the content, not just failed to grow.
        assert!(rope.0.depth() < 5);
    }

    #[test]
    fn lines_clamps_out_of_range_bounds_instead_of_refusing_unlike_line() {
        let rope = Rope::new("a\nb\nc");
        assert_eq!(rope.lines(0, 1000), ["a", "b", "c"]);
        assert_eq!(rope.lines(1, 1), ["b"]);
        assert_eq!(
            rope.lines(5, 2),
            Vec::<&str>::new(),
            "a reversed pair visits nothing"
        );
    }

    #[test]
    fn never_hands_out_a_stale_joined_text_after_a_later_replace() {
        let before = Rope::new("alpha\nbeta");
        assert_eq!(before.text(), "alpha\nbeta");
        let after = before.replace(r(p(0, 0), p(0, 5)), "ALPHA").unwrap().rope;
        assert_eq!(after.text(), "ALPHA\nbeta");
        // The earlier version keeps its own text; a clone shares it.
        assert_eq!(before.text(), "alpha\nbeta");
        assert_eq!(before.clone().text(), "alpha\nbeta");
    }

    #[test]
    fn pos_at_is_offset_ats_inverse_everywhere_across_leaves_and_line_breaks() {
        let rope = Rope::new(&numbered(3000).replace("line 7", "léne 7"));
        for line in 0..rope.line_count() {
            let text = rope.line(line).unwrap();
            for (col, _) in text.char_indices().chain([(text.len(), ' ')]) {
                assert_eq!(
                    rope.pos_at(rope.offset_at(p(line, col)).unwrap()),
                    p(line, col)
                );
            }
        }
        assert_eq!(rope.pos_at(0), p(0, 0));
        assert_eq!(rope.pos_at(rope.len()), rope.end());
        assert_eq!(rope.pos_at(rope.len() + 99), rope.end());
    }

    #[test]
    fn pos_at_moves_an_offset_inside_a_character_back_to_its_start() {
        let rope = Rope::new("a🙂\nb");
        assert_eq!(rope.pos_at(1), p(0, 1));
        assert_eq!(rope.pos_at(3), p(0, 1));
        assert_eq!(rope.pos_at(5), p(0, 5));
        assert_eq!(rope.pos_at(6), p(1, 0));
    }

    #[test]
    fn puts_the_offset_of_a_line_break_at_the_end_of_the_line_it_ends() {
        let rope = Rope::new("ab\ncd");
        assert_eq!(rope.pos_at(2), p(0, 2));
        assert_eq!(rope.pos_at(3), p(1, 0));
    }

    #[test]
    fn resolves_offsets_right_at_a_leaf_boundary_to_the_line_either_side_deterministically() {
        let rope = Rope::new(&numbered(MAX_LEAF_LINES * 3));
        ok(&rope);
        for boundary in [
            MAX_LEAF_LINES - 1,
            MAX_LEAF_LINES,
            2 * MAX_LEAF_LINES - 1,
            2 * MAX_LEAF_LINES,
        ] {
            let len = rope.line(boundary).unwrap().len();
            let end_of_line = rope.offset_at(p(boundary, len)).unwrap();
            assert_eq!(rope.pos_at(end_of_line), p(boundary, len));
            assert_eq!(rope.pos_at(end_of_line + 1), p(boundary + 1, 0));
            for col in 0..=len {
                assert_eq!(
                    rope.pos_at(rope.offset_at(p(boundary, col)).unwrap()),
                    p(boundary, col)
                );
            }
        }
    }

    // ---- random edits against the line store (rope.test.ts's seeded run, as a property) ----

    const PIECES: [&str; 7] = ["", "x", "é", "ab\ncd", "\n", "\n\n\n", "🙂"];

    /// One edit, as fractions so it lands inside whatever text the steps before it left.
    #[derive(Debug, Clone)]
    struct Step {
        a: (u16, u16),
        b: (u16, u16),
        cursor: bool,
        big: bool,
        piece: usize,
    }

    fn step() -> impl Strategy<Value = Step> {
        (
            any::<(u16, u16)>(),
            any::<(u16, u16)>(),
            prop::bool::weighted(0.3),
            prop::bool::weighted(0.05),
            0..9_usize,
        )
            .prop_map(|(a, b, cursor, big, piece)| Step {
                a,
                b,
                cursor,
                big,
                piece,
            })
    }

    fn place(store: &LineStore, (line, col): (u16, u16)) -> Pos {
        let line = usize::from(line) * store.lines.len() / (usize::from(u16::MAX) + 1);
        let text = &store.lines[line];
        let col = usize::from(col) * (text.len() + 1) / (usize::from(u16::MAX) + 1);
        Pos::new(line, floor_char_boundary(text, col))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn matches_the_line_store_through_random_edits_staying_balanced(steps in prop::collection::vec(step(), 1..400)) {
            let text = numbered(300);
            let mut lines = LineStore::new(&text);
            let mut rope = RopeStore(Rope::new(&text));
            let (forty, one_fifty) = (numbered(40), numbered(150));
            for (k, s) in steps.iter().enumerate() {
                let a = place(&lines, s.a);
                let b = if s.cursor { a } else { place(&lines, s.b) };
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                // Mostly small edits, now and then a big deletion or paste.
                let range = if s.big { r(p(a.line / 2, 0), b) } else { r(a, b) };
                let insert = match s.piece { 7 => forty.as_str(), 8 => one_fifty.as_str(), i => PIECES[i] };
                let expected = lines.replace(range, insert).unwrap();
                prop_assert_eq!(rope.replace(range, insert).unwrap(), expected, "step {}", k);
                prop_assert_eq!(rope.0.check_invariants(), Ok(()));
                prop_assert_eq!(rope.line_count(), lines.line_count());
                if k % 50 == 0 {
                    prop_assert_eq!(rope.text(), lines.text());
                    let at = place(&lines, s.b);
                    prop_assert_eq!(rope.offset_at(at), lines.offset_at(at));
                    prop_assert_eq!(rope.slice(r(a, b)), lines.slice(r(a, b)));
                }
            }
            prop_assert_eq!(rope.text(), lines.text());
        }
    }
}
