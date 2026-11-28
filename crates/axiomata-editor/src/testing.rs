//! Test helpers (Axiomata-OS `editor/testing.ts`): a document written as text with markers. `|` is a cursor (the
//! selection's head), `^` the anchor when there is a selection: `"let ^x|= 1"` is `x` selected left to right. `show`
//! prints a document back in the same notation, so a test reads as before → after.

use crate::commands::{CommandContext, UNWRAPPED};
use crate::detect::{Indent, IndentKind};
use crate::document::{Document, Millis};
use crate::multicursor::all_selections;
use crate::position::{Pos, Selection};

/// Four-space indentation, the fallback of every test document.
pub(crate) const FOUR: Indent = Indent {
    kind: IndentKind::Spaces,
    size: 4,
};

pub(crate) fn p(line: usize, col: usize) -> Pos {
    Pos::new(line, col)
}

pub(crate) fn cursor(line: usize, col: usize) -> Selection {
    Selection::cursor(p(line, col))
}

/// Where `marker` sits in `text`, and the text without it.
fn find(text: &str, marker: char) -> (Option<Pos>, String) {
    let Some(index) = text.find(marker) else {
        return (None, text.to_owned());
    };
    let before = &text[..index];
    let line = before.matches('\n').count();
    let col = before.len() - before.rfind('\n').map_or(0, |at| at + 1);
    (
        Some(p(line, col)),
        [before, &text[index + marker.len_utf8()..]].concat(),
    )
}

/// A document from text with one cursor marked: `|`, and `^` for a selection's anchor.
pub(crate) fn doc_from(marked: &str) -> Document {
    // Remove the earlier marker first; the later one is then found in the text it will really sit in.
    let caret_first = marked
        .find('^')
        .is_none_or(|anchor| marked.find('|').is_some_and(|head| head < anchor));
    let (head, anchor, text) = if caret_first {
        let (head, text) = find(marked, '|');
        let (anchor, text) = find(&text, '^');
        (head, anchor, text)
    } else {
        let (anchor, text) = find(marked, '^');
        let (head, text) = find(&text, '|');
        (head, anchor, text)
    };
    let mut doc = Document::new(&text, FOUR);
    let head = head.unwrap_or_default();
    doc.set_selection(
        Selection {
            anchor: anchor.unwrap_or(head),
            head,
        },
        false,
    );
    doc
}

/// The document's text with its main cursor's `|` and `^` put back in.
pub(crate) fn show(doc: &Document) -> String {
    marked(doc, &[doc.selection()])
}

/// A document with several cursors (`multicursor.test.ts`): every `|` is a cursor's head, a `^` before it its
/// anchor. The last cursor in the text is the main one unless `main` says which (in text order).
pub(crate) fn doc_with(marked_text: &str, main: Option<usize>) -> Document {
    let mut sels = Vec::new();
    let mut anchor = None;
    let (mut line, mut col) = (0, 0);
    let mut text = String::new();
    for ch in marked_text.chars() {
        match ch {
            '^' => anchor = Some(p(line, col)),
            '|' => {
                sels.push(Selection {
                    anchor: anchor.take().unwrap_or(p(line, col)),
                    head: p(line, col),
                });
            }
            _ => {
                text.push(ch);
                if ch == '\n' {
                    line += 1;
                    col = 0;
                } else {
                    col += ch.len_utf8();
                }
            }
        }
    }
    let mut doc = Document::new(&text, FOUR);
    let main = main.unwrap_or(sels.len() - 1);
    let others: Vec<Selection> = sels
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != main)
        .map(|(_, s)| *s)
        .collect();
    doc.set_selections(sels[main], &others, false);
    doc
}

/// Every cursor put back into the text, `|` heads and `^` anchors.
pub(crate) fn show_all(doc: &Document) -> String {
    marked(doc, &all_selections(doc))
}

fn marked(doc: &Document, sels: &[Selection]) -> String {
    let mut lines: Vec<String> = doc.rope().text().split('\n').map(str::to_owned).collect();
    let mut marks = Vec::new();
    for s in sels {
        marks.push((s.head, '|'));
        if s.anchor != s.head {
            marks.push((s.anchor, '^'));
        }
    }
    // Right to left so earlier columns stay valid; at one place the head's `|` goes in first, ending up after `^`.
    marks.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| (a.1 != '|').cmp(&(b.1 != '|'))));
    for (at, mark) in marks {
        lines[at.line].insert(at.col, mark);
    }
    lines.join("\n")
}

/// The context the command tests run in: tabs of 4, no wrapping, pages of 3 rows, `//` comments, time 0.
pub(crate) fn ctx() -> CommandContext<'static> {
    CommandContext {
        tab_size: 4,
        layout: &UNWRAPPED,
        page_rows: 3,
        comment_prefix: Some("//"),
        now: Millis(0),
    }
}
