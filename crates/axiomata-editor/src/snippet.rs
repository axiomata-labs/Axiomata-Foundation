//! Snippets (Axiomata-OS `editor/lsp/completion.ts` `expandSnippet`/`completionEdit`, `editor/snippet.ts`, L9 and
//! board card #36): a snippet's text with its tab stops, what taking a completion does to the line, and jumping
//! through the stops afterwards — ⇥ selects the next placeholder and ⇧⇥ the one before, so typing replaces it, as
//! `LuaSnip` / `blink.cmp` in the owner's Neovim.
//!
//! * **The stops follow the text**: every change moves them; the stop being edited grows with what is typed at its
//!   end, so a replaced placeholder stays one stop however long the new text gets.
//! * **The session lives beside the document** (`docs/plans/rust-core.md` S7) with a bookmark of its own: every
//!   question to it first takes in the changes since, so it can never fall behind.
//! * **It ends** at the last stop (`$0`, or the snippet's end), when the cursor leaves every stop, or when the caller
//!   ends it (Esc, another completion). A repeated `$1` is not mirrored.

use std::collections::BTreeMap;

use crate::document::{Change, Document, Follower, TextChange};
use crate::multicursor::map_pos;
use crate::position::{Pos, Range, Selection};

/// A tab stop in an expanded snippet: its placeholder text as byte offsets into the text (equal for an empty stop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SnippetStop {
    /// Where the placeholder starts.
    pub start: usize,
    /// Where it ends.
    pub end: usize,
}

/// A snippet resolved into plain text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Expanded {
    /// The text with every placeholder as its default.
    pub text: String,
    /// Where the cursor goes: the first stop, or the end.
    pub cursor: usize,
    /// The stops in jump order — `$1`, `$2`, … then `$0`, or the end when there is no `$0`.
    pub stops: Vec<SnippetStop>,
}

/// The characters a backslash escapes in a snippet.
const ESCAPED: &str = "$}\\,|";

struct Parser<'a> {
    snippet: &'a str,
    at: usize,
    /// Each stop's number and place, at its first occurrence.
    stops: BTreeMap<u64, SnippetStop>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.snippet[self.at..].chars().next()
    }

    fn peek_byte(&self, ahead: usize) -> Option<u8> {
        self.snippet.as_bytes().get(self.at + ahead).copied()
    }

    /// A tab stop's number or a variable's name right at the parser, consumed: digits, or a letter or `_` and then
    /// letters, digits and `_`.
    fn name(&mut self) -> Option<Name> {
        let rest = &self.snippet.as_bytes()[self.at..];
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 {
            let number = self.snippet[self.at..self.at + digits]
                .parse()
                .unwrap_or(u64::MAX);
            self.at += digits;
            return Some(Name::Stop(number));
        }
        let first = rest
            .first()
            .filter(|b| b.is_ascii_alphabetic() || **b == b'_')?;
        let _ = first;
        let len = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count();
        self.at += len;
        Some(Name::Variable)
    }

    /// The stop numbered `n` at `start..end`, if `n` has none yet (only the first occurrence counts).
    fn note(&mut self, n: u64, start: usize, end: usize) -> Option<u64> {
        if self.stops.contains_key(&n) {
            return None;
        }
        self.stops.insert(n, SnippetStop { start, end });
        Some(n)
    }

    /// The text up to `stop_at` (not consumed) or the end, its first byte at `base` in the whole output.
    fn parse(&mut self, stop_at: Option<char>, base: usize) -> String {
        let mut out = String::new();
        while let Some(ch) = self.peek() {
            if Some(ch) == stop_at {
                return out;
            }
            if ch == '\\'
                && let Some(next) = self.snippet[self.at + 1..].chars().next()
                && ESCAPED.contains(next)
            {
                out.push(next);
                self.at += 1 + next.len_utf8();
                continue;
            }
            if ch != '$' {
                out.push(ch);
                self.at += ch.len_utf8();
                continue;
            }
            let dollar = self.at;
            self.at += 1;
            if self.peek_byte(0) != Some(b'{') {
                match self.name() {
                    Some(Name::Stop(n)) => {
                        let here = base + out.len();
                        self.note(n, here, here);
                    }
                    Some(Name::Variable) => {}
                    None => out.push('$'),
                }
                continue;
            }
            self.at += 1;
            let Some(name) = self.name() else {
                // `${` with no name: the `$` is text, the `{` comes next.
                self.at = dollar + 1;
                out.push('$');
                continue;
            };
            let here = base + out.len();
            // Noted before the placeholder's own text, so a stop nested in it (`${1:a ${2:b}}`) comes after it.
            let stop = match name {
                Name::Stop(n) => self.note(n, here, here),
                Name::Variable => None,
            };
            match self.peek_byte(0) {
                Some(b'|') => {
                    // A choice: the first option.
                    let options_start = self.at + 1;
                    let end = self.snippet[options_start..]
                        .find("|}")
                        .map(|at| options_start + at);
                    let options = &self.snippet[options_start..end.unwrap_or(self.snippet.len())];
                    out.push_str(options.split(',').next().unwrap_or(""));
                    self.at = end.map_or(self.snippet.len(), |end| end + 2);
                    self.end_stop(stop, base + out.len());
                    continue;
                }
                Some(b':') => {
                    self.at += 1;
                    let inner = self.parse(Some('}'), here);
                    out.push_str(&inner);
                    self.end_stop(stop, base + out.len());
                }
                Some(b'/') => self.skip_transform(),
                _ => {}
            }
            if self.peek_byte(0) == Some(b'}') {
                self.at += 1;
            }
        }
        out
    }

    fn end_stop(&mut self, stop: Option<u64>, end: usize) {
        if let Some(n) = stop
            && let Some(s) = self.stops.get_mut(&n)
        {
            s.end = end;
        }
    }

    /// A transform (`${1/re/format/}`): the variable's value would be rewritten; without values there is nothing.
    fn skip_transform(&mut self) {
        let bytes = self.snippet.as_bytes();
        let mut depth = 0usize;
        while let Some(&b) = bytes.get(self.at) {
            if b == b'}' && depth == 0 {
                break;
            }
            match b {
                b'\\' => self.at += 1,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            self.at += 1;
        }
        self.at = self.at.min(bytes.len());
        // Land on a character boundary after an escape before a multi-byte character.
        while !self.snippet.is_char_boundary(self.at) {
            self.at += 1;
        }
    }
}

enum Name {
    Stop(u64),
    Variable,
}

/// A snippet's text (L9): tab stops and placeholders become their default text (`${1:name}` → `name`, `$1` →
/// nothing, `${1|a,b|}` → `a`), variables their default or nothing, `\` escapes are resolved. The stops are in jump
/// order — `$1`, `$2`, … then `$0`, or the end when there is no `$0` — each at its first occurrence.
#[must_use]
pub fn expand_snippet(snippet: &str) -> Expanded {
    let mut parser = Parser {
        snippet,
        at: 0,
        stops: BTreeMap::new(),
    };
    let text = parser.parse(None, 0);
    let end = SnippetStop {
        start: text.len(),
        end: text.len(),
    };
    let last = parser.stops.get(&0).copied();
    let mut stops: Vec<SnippetStop> = parser
        .stops
        .iter()
        .filter(|(n, _)| **n > 0)
        .map(|(_, s)| *s)
        .collect();
    if !stops.is_empty() {
        stops.push(last.unwrap_or(end));
    } else if let Some(last) = last {
        stops.push(last);
    }
    Expanded {
        cursor: stops.first().map_or(text.len(), |s| s.start),
        text,
        stops,
    }
}

/// What a completion item inserts, as the language server gave it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Insertion {
    /// The text, or the snippet.
    pub text: String,
    /// Whether `text` is a snippet with tab stops.
    pub snippet: bool,
    /// The server's range to replace, if it gave one.
    pub range: Option<Range>,
    /// Edits elsewhere (an auto-import), in the document's coordinates before this edit.
    pub additional_edits: Vec<Change>,
}

/// What taking a completion does, as [`crate::Command::Complete`] carries it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionEdit {
    /// Bytes before the cursor on its line that are replaced (the typed word).
    pub before: usize,
    /// Bytes after the cursor on its line that are replaced.
    pub after: usize,
    /// What goes there.
    pub text: String,
    /// Where the cursor goes, as a byte offset into `text`.
    pub cursor: usize,
    /// A snippet's tab stops as byte offsets into `text`, in jump order; empty for plain text.
    pub stops: Vec<SnippetStop>,
    /// Edits elsewhere (an auto-import), in the document's coordinates before this edit.
    pub extra: Vec<Change>,
}

/// What taking `item` at `at` does. `word_start` is where the typed word begins on the cursor's line; a server range
/// on that line containing the cursor wins over it. `indent` is the line's own indentation, given to every further
/// line of a multi-line insert.
#[must_use]
pub fn completion_edit(
    item: &Insertion,
    at: Pos,
    word_start: usize,
    indent: &str,
) -> CompletionEdit {
    let usable = item
        .range
        .filter(|r| r.start.line == at.line && r.end.line == at.line && r.start.col <= at.col);
    let start = usable.map_or(word_start, |r| r.start.col);
    let end = usable.map_or(at.col, |r| r.end.col.max(at.col));
    let expanded = if item.snippet {
        expand_snippet(&item.text)
    } else {
        Expanded {
            text: item.text.clone(),
            cursor: item.text.len(),
            stops: Vec::new(),
        }
    };
    let indented = |s: &str| {
        s.split('\n')
            .collect::<Vec<_>>()
            .join(&format!("\n{indent}"))
    };
    // An offset into the expanded text, moved by the indentation added before it.
    let offset = |o: usize| indented(&expanded.text[..o.min(expanded.text.len())]).len();
    let replaced = Range {
        start: Pos::new(at.line, start),
        end: Pos::new(at.line, end),
    };
    // Additional edits must not touch the replaced range; ones that do are dropped.
    let extra = item
        .additional_edits
        .iter()
        .filter(|e| e.range.end <= replaced.start || e.range.start >= replaced.end)
        .cloned()
        .collect();
    CompletionEdit {
        before: at.col.saturating_sub(start),
        after: end - at.col,
        text: indented(&expanded.text),
        cursor: offset(expanded.cursor),
        stops: expanded
            .stops
            .iter()
            .map(|s| SnippetStop {
                start: offset(s.start),
                end: offset(s.end),
            })
            .collect(),
        extra,
    }
}

/// `p` after `change`, counting an insertion right at `p` as before it — the end of the stop being typed into.
fn map_end_inclusive(p: Pos, change: &TextChange) -> Pos {
    if p < change.start {
        p
    } else if p <= change.old_end {
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

/// Jumping through a taken snippet's stops (see the module header). End it with [`SnippetSession::end`]: until then
/// the document keeps every change for it.
#[derive(Debug)]
pub struct SnippetSession {
    stops: Vec<Range>,
    index: usize,
    follower: Follower,
}

impl SnippetSession {
    /// `stops` in jump order, where they are in `doc` right now; the first is the current one. Made right *after* the
    /// completion was taken: [`crate::completion_targets`] gives the stops as they will be then, and a session made
    /// before would move them once more by the completion's own change.
    #[must_use = "a session keeps every change until it is ended with `SnippetSession::end`"]
    pub fn new(doc: &mut Document, stops: Vec<Range>) -> Self {
        Self {
            stops,
            index: 0,
            follower: doc.follow(),
        }
    }

    /// The stop being edited.
    pub fn current(&mut self, doc: &mut Document) -> Option<Range> {
        self.catch_up(doc);
        self.stops.get(self.index).copied()
    }

    /// Whether `selection` is still inside one of the stops (their edges included).
    pub fn holds(&mut self, doc: &mut Document, selection: Selection) -> bool {
        self.catch_up(doc);
        let r = selection.range();
        self.stops
            .iter()
            .any(|s| s.start <= r.start && r.end <= s.end)
    }

    /// The next stop to select, or `None` at the end; at the last stop the caller ends the session.
    pub fn next(&mut self, doc: &mut Document) -> Option<Range> {
        self.catch_up(doc);
        if self.index + 1 >= self.stops.len() {
            return None;
        }
        self.index += 1;
        self.stops.get(self.index).copied()
    }

    /// The stop before, or `None` at the first.
    pub fn previous(&mut self, doc: &mut Document) -> Option<Range> {
        self.catch_up(doc);
        self.index = self.index.checked_sub(1)?;
        self.stops.get(self.index).copied()
    }

    /// Whether the current stop is the last one.
    #[must_use]
    pub fn at_last(&self) -> bool {
        self.index + 1 >= self.stops.len()
    }

    /// Ends the session and gives its bookmark back.
    pub fn end(self, doc: &mut Document) {
        doc.unfollow(self.follower);
    }

    fn catch_up(&mut self, doc: &mut Document) {
        for change in doc.changes_since(&self.follower) {
            self.follow(&change);
        }
    }

    fn follow(&mut self, change: &TextChange) {
        let active_end = self.stops.get(self.index).map(|s| s.end);
        let index = self.index;
        for (i, s) in self.stops.iter_mut().enumerate() {
            *s = if i == index {
                Range {
                    start: map_pos(s.start, change),
                    end: map_end_inclusive(s.end, change),
                }
            } else {
                // A stop right at the active one's end (`${1:x}$0`, the implicit end after the last placeholder)
                // stays after what is typed there, not before it.
                let touches = active_end == Some(s.start);
                let map = if touches { map_end_inclusive } else { map_pos };
                Range {
                    start: map(s.start, change),
                    end: map(s.end, change),
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, completion_targets, run};
    use crate::testing::{FOUR, ctx, cursor, p};

    fn s(start: usize, end: usize) -> SnippetStop {
        SnippetStop { start, end }
    }

    fn range(a: Pos, b: Pos) -> Range {
        Range { start: a, end: b }
    }

    #[test]
    fn inserts_placeholders_as_their_text_and_puts_the_cursor_on_the_first_stop() {
        assert_eq!(
            expand_snippet("push(${1:value})$0"),
            Expanded {
                text: "push(value)".into(),
                cursor: 5,
                stops: vec![s(5, 10), s(11, 11)]
            }
        );
        assert_eq!(
            expand_snippet("fn ${1:name}(${2}) {\n\t$0\n}"),
            Expanded {
                text: "fn name() {\n\t\n}".into(),
                cursor: 3,
                stops: vec![s(3, 7), s(8, 8), s(13, 13)]
            }
        );
        assert_eq!(expand_snippet("if $0 {}").stops, [s(3, 3)]);
        assert_eq!(expand_snippet("if $0 {}").text, "if  {}");
        // Without `$0` the end of the snippet is the last stop.
        let choice = expand_snippet("${1|a,b|} x");
        assert_eq!(
            (choice.text.as_str(), choice.stops),
            ("a x", vec![s(0, 1), s(3, 3)])
        );
        let nested = expand_snippet("${1:outer ${2:inner}}");
        assert_eq!(
            (nested.text.as_str(), nested.stops),
            ("outer inner", vec![s(0, 11), s(6, 11), s(11, 11)])
        );
        // A repeated stop counts where it first appears.
        assert_eq!(expand_snippet("${1:a} = $1;").stops, [s(0, 1), s(5, 5)]);
        assert_eq!(
            expand_snippet("cost \\$5 ${TM_FILENAME:file}"),
            Expanded {
                text: "cost $5 file".into(),
                cursor: 12,
                stops: vec![]
            }
        );
        assert_eq!(expand_snippet("plain").cursor, 5);
    }

    #[test]
    fn counts_stops_in_bytes_and_keeps_odd_input_as_text() {
        // "ä" is two bytes: the stop after it starts at 2 (1 in UTF-16).
        assert_eq!(expand_snippet("ä${1:ö}").stops, [s(2, 4), s(4, 4)]);
        assert_eq!(expand_snippet("a $ b ${ c").text, "a $ b ${ c");
        assert_eq!(expand_snippet("${1/(.)/x/}z").text, "z");
        assert_eq!(
            expand_snippet("${1|a,b").text,
            "a",
            "an unclosed choice: its first option"
        );
        assert_eq!(expand_snippet("${99999999999999999999:x}").text, "x");
    }

    fn snippet_item(text: &str) -> Insertion {
        Insertion {
            text: text.into(),
            snippet: true,
            ..Insertion::default()
        }
    }

    #[test]
    fn replaces_the_typed_word_adds_an_import_above_and_lands_where_the_snippet_says() {
        let mut d = Document::new("fn main() {\n    let m = Has\n}", FOUR);
        d.set_selection(cursor(1, 15), false);
        let item = Insertion {
            additional_edits: vec![Change {
                range: Range::at(p(0, 0)),
                text: "use std::collections::HashMap;\n".into(),
            }],
            ..snippet_item("HashMap::new($1)")
        };
        let edit = completion_edit(&item, p(1, 15), 12, "    ");
        assert_eq!(
            (edit.before, edit.after, edit.text.as_str(), edit.cursor),
            (3, 0, "HashMap::new()", 13)
        );
        run(&mut d, &Command::Complete(edit), &ctx()).unwrap();
        assert_eq!(
            d.rope().text(),
            "use std::collections::HashMap;\nfn main() {\n    let m = HashMap::new()\n}"
        );
        assert_eq!(d.selection().head, p(2, 25));
        d.undo().unwrap();
        assert_eq!(d.rope().text(), "fn main() {\n    let m = Has\n}");
    }

    #[test]
    fn puts_the_cursor_right_after_several_extra_edits_one_on_its_own_line() {
        let mut d = Document::new("a\nb c", FOUR);
        d.set_selection(cursor(1, 3), false);
        let edit = CompletionEdit {
            before: 1,
            after: 0,
            text: "cat".into(),
            cursor: 3,
            stops: vec![],
            extra: vec![
                Change {
                    range: range(p(1, 0), p(1, 1)),
                    text: "x\ny".into(),
                },
                Change {
                    range: Range::at(p(0, 0)),
                    text: "use z;\n".into(),
                },
            ],
        };
        run(&mut d, &Command::Complete(edit), &ctx()).unwrap();
        assert_eq!(d.rope().text(), "use z;\na\nx\ny cat");
        assert_eq!(d.selection().head, p(3, 5));
    }

    #[test]
    fn uses_the_servers_range_indents_further_lines_and_drops_overlapping_extras() {
        let item = Insertion {
            range: Some(range(p(2, 4), p(2, 8))),
            additional_edits: vec![Change {
                range: range(p(2, 5), p(2, 6)),
                text: "!".into(),
            }],
            ..snippet_item("match ${1:x} {\n\t$0\n}")
        };
        assert_eq!(
            completion_edit(&item, p(2, 7), 7, "    "),
            CompletionEdit {
                before: 3,
                after: 1,
                text: "match x {\n    \t\n    }".into(),
                cursor: 6,
                // The `$0` on the indented line moves with the indentation added before it.
                stops: vec![s(6, 7), s(15, 15)],
                extra: vec![],
            }
        );
    }

    /// A document with `fo` typed, the snippet taken there, and a session over its stops.
    fn taken(snippet: &str) -> (Document, SnippetSession) {
        let mut d = Document::new("x fo", FOUR);
        d.set_selection(cursor(0, 4), false);
        let edit = completion_edit(&snippet_item(snippet), p(0, 4), 2, "");
        let stops = completion_targets(&d, &edit).unwrap().stops;
        run(&mut d, &Command::Complete(edit), &ctx()).unwrap();
        let session = SnippetSession::new(&mut d, stops);
        (d, session)
    }

    fn typed(d: &mut Document, text: &str) {
        run(d, &Command::insert(text), &ctx()).unwrap();
    }

    #[test]
    fn selects_the_first_placeholder_so_typing_replaces_it() {
        let (mut d, session) = taken("for ${1:item} in ${2:list} {$0}");
        assert_eq!(d.rope().text(), "x for item in list {}");
        assert_eq!(
            d.selection(),
            Selection {
                anchor: p(0, 6),
                head: p(0, 10)
            }
        );
        typed(&mut d, "row");
        assert_eq!(d.rope().text(), "x for row in list {}");
        session.end(&mut d);
    }

    #[test]
    fn follows_the_text_a_grown_placeholder_stays_one_stop_the_next_one_moves_along() {
        let (mut d, mut session) = taken("for ${1:item} in ${2:list} {$0}");
        typed(&mut d, "r");
        typed(&mut d, "ows");
        assert_eq!(d.rope().text(), "x for rows in list {}");
        let sel = d.selection();
        assert!(session.holds(&mut d, sel));
        assert_eq!(session.current(&mut d), Some(range(p(0, 6), p(0, 10))));
        assert_eq!(session.next(&mut d), Some(range(p(0, 14), p(0, 18))));
        assert!(!session.at_last());
        assert_eq!(session.next(&mut d), Some(range(p(0, 20), p(0, 20))));
        assert!(session.at_last());
        assert_eq!(session.next(&mut d), None);
        assert_eq!(session.previous(&mut d), Some(range(p(0, 14), p(0, 18))));
        session.end(&mut d);
        typed(&mut d, "x");
        let follower = d.follow();
        assert_eq!(
            d.changes_since(&follower),
            [],
            "the ended session left nothing behind"
        );
    }

    #[test]
    fn grows_an_empty_stop_with_what_is_typed_into_it() {
        let (mut d, mut session) = taken("f(${1})$0");
        assert_eq!(d.selection(), cursor(0, 4));
        typed(&mut d, "ab");
        assert_eq!(session.current(&mut d), Some(range(p(0, 4), p(0, 6))));
        assert_eq!(session.next(&mut d), Some(range(p(0, 7), p(0, 7))));
    }

    #[test]
    fn keeps_the_end_stop_after_what_is_typed_into_the_last_placeholder() {
        let (mut d, mut session) = taken("for ${1:a} in ${2:b}");
        assert_eq!(session.next(&mut d), Some(range(p(0, 11), p(0, 12))));
        d.set_selection(
            Selection {
                anchor: p(0, 11),
                head: p(0, 12),
            },
            false,
        );
        typed(&mut d, "x");
        typed(&mut d, "y");
        assert_eq!(d.rope().text(), "x for a in xy");
        assert_eq!(session.next(&mut d), Some(range(p(0, 13), p(0, 13))));
        assert_eq!(session.previous(&mut d), Some(range(p(0, 11), p(0, 13))));
        assert_eq!(session.previous(&mut d), Some(range(p(0, 6), p(0, 7))));
        assert_eq!(session.previous(&mut d), None);
    }

    #[test]
    fn no_longer_holds_a_cursor_outside_every_stop() {
        let (mut d, mut session) = taken("for ${1:item} in ${2:list} {$0}");
        d.set_selection(cursor(0, 0), false);
        let sel = d.selection();
        assert!(!session.holds(&mut d, sel));
    }

    #[test]
    fn a_plain_completion_has_no_stops_and_lands_at_its_end() {
        let mut d = Document::new("fo", FOUR);
        d.set_selection(cursor(0, 2), false);
        let item = Insertion {
            snippet: false,
            ..snippet_item("foobar")
        };
        let edit = completion_edit(&item, p(0, 2), 0, "");
        let targets = completion_targets(&d, &edit).unwrap();
        assert_eq!(targets.stops, []);
        assert_eq!(targets.selection, cursor(0, 6));
    }
}
