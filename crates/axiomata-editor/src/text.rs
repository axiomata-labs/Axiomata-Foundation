//! Characters as a person sees them (Axiomata-OS `editor/text.ts`): grapheme and word boundaries within one line, and
//! display columns with tabs expanded. Every column here is a byte offset into the line, on a character boundary.
//!
//! The cursor must never stop between the bytes of one character, nor between a letter and its combining accent, nor
//! inside an emoji with its skin tone: graphemes come from Unicode's segmentation rules (`unicode-segmentation`).
//! Words use our own character classes (see [`is_word_char`] for why not Unicode's word rules).
//!
//! A column a caller passes inside a character counts as the start of that character; past the line's end, as its
//! end. Nothing here panics on a column.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::position::floor_char_boundary;

/// The grapheme boundary after `col`, or the line's length at its end.
#[must_use]
pub fn next_grapheme(line: &str, col: usize) -> usize {
    if col >= line.len() {
        return line.len();
    }
    line.grapheme_indices(true)
        .map(|(at, g)| at + g.len())
        .find(|&end| end > col)
        .unwrap_or(line.len())
}

/// The grapheme boundary before `col`, or 0 at the line's start.
#[must_use]
pub fn prev_grapheme(line: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    line.grapheme_indices(true)
        .map(|(at, _)| at)
        .take_while(|&at| at < col)
        .last()
        .unwrap_or(0)
}

/// How many graphemes `text` holds — what a person would call its length in characters.
#[must_use]
pub fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

/// The word characters of ⌥←/→, ⌥⌫ and a double-click.
static WORD_CHAR: LazyLock<Regex> = LazyLock::new(|| {
    // A fixed pattern, checked by the tests: it always compiles.
    Regex::new(r"^[\p{L}\p{N}\p{M}_]$")
        .unwrap_or_else(|e| unreachable!("the word pattern compiles: {e}"))
});

/// Whether `ch` is a word character: a letter, a digit, a combining mark or `_`. Deliberately not Unicode's word
/// boundaries (UAX #29), which follow prose rules and treat `bar.baz` or `3.14` as one word — wrong in code, where
/// ⌥→ should stop at the dot.
#[must_use]
pub fn is_word_char(ch: char) -> bool {
    if ch.is_ascii() {
        return ch.is_ascii_alphanumeric() || ch == '_';
    }
    let mut buf = [0; 4];
    WORD_CHAR.is_match(ch.encode_utf8(&mut buf))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

/// The class of the character starting at `col`; at the end of the line, `Other`.
fn class_at(line: &str, col: usize) -> CharClass {
    match line[col..].chars().next() {
        Some(ch) if is_word_char(ch) => CharClass::Word,
        Some(' ' | '\t') => CharClass::Space,
        _ => CharClass::Other,
    }
}

/// The start of the character before `col`.
fn prev_char(line: &str, col: usize) -> usize {
    line[..col]
        .char_indices()
        .next_back()
        .map_or(0, |(at, _)| at)
}

/// The end of the character starting at `col`.
fn next_char(line: &str, col: usize) -> usize {
    line[col..]
        .chars()
        .next()
        .map_or(line.len(), |ch| col + ch.len_utf8())
}

/// The end of the next word after `col` (⌥→): skips non-word characters, then the word.
#[must_use]
pub fn next_word_end(line: &str, col: usize) -> usize {
    let mut i = floor_char_boundary(line, col);
    while i < line.len() && class_at(line, i) != CharClass::Word {
        i = next_char(line, i);
    }
    while i < line.len() && class_at(line, i) == CharClass::Word {
        i = next_char(line, i);
    }
    i
}

/// The start of the word before `col` (⌥←), or 0 if there is none.
#[must_use]
pub fn prev_word_start(line: &str, col: usize) -> usize {
    let mut i = floor_char_boundary(line, col);
    while i > 0 && class_at(line, prev_char(line, i)) != CharClass::Word {
        i = prev_char(line, i);
    }
    while i > 0 && class_at(line, prev_char(line, i)) == CharClass::Word {
        i = prev_char(line, i);
    }
    i
}

/// What a double-click at `col` selects: the run of word characters, of whitespace, or of other characters it falls
/// in. At the very end of the line it takes the run before. The span is in bytes of the line (a [`std::ops::Range`],
/// not the crate's [`crate::Range`] of positions).
#[must_use]
pub fn word_at(line: &str, col: usize) -> Range<usize> {
    if line.is_empty() {
        return 0..0;
    }
    let at = if col >= line.len() {
        prev_char(line, line.len())
    } else {
        floor_char_boundary(line, col)
    };
    let kind = class_at(line, at);
    let mut start = at;
    while start > 0 && class_at(line, prev_char(line, start)) == kind {
        start = prev_char(line, start);
    }
    let mut end = at;
    while end < line.len() && class_at(line, end) == kind {
        end = next_char(line, end);
    }
    start..end
}

/// Cells one grapheme takes at display column `column`: a tab runs to the next stop, an East Asian wide character or
/// an emoji takes two, anything else one — also a grapheme Unicode gives no width, so the cursor still has a cell to
/// stand on.
///
/// The widths are Unicode's (`unicode-width`), not the coarse table of the TypeScript editor: a few symbols differ
/// there, such as "©", which that table counted as an emoji of two cells and Unicode gives one.
#[must_use]
pub fn cell_width(grapheme: &str, column: usize, tab_size: usize) -> usize {
    if grapheme == "\t" {
        let tab = tab_size.max(1);
        return tab - column % tab;
    }
    grapheme.width().clamp(1, 2)
}

/// The display column of `col`: cells before it, tabs expanded to `tab_size` stops.
#[must_use]
pub fn display_column(line: &str, col: usize, tab_size: usize) -> usize {
    line[..floor_char_boundary(line, col)]
        .graphemes(true)
        .fold(0, |column, g| column + cell_width(g, column, tab_size))
}

/// The column nearest to display column `target` — what ↑/↓ use to keep the cursor's visual column across lines of
/// different content.
#[must_use]
pub fn col_for_display_column(line: &str, target: usize, tab_size: usize) -> usize {
    let mut column = 0;
    for (at, g) in line.grapheme_indices(true) {
        let width = cell_width(g, column, tab_size);
        if column + width > target {
            return if target - column < column + width - target {
                at
            } else {
                at + g.len()
            };
        }
        column += width;
    }
    line.len()
}

/// The leading spaces and tabs of a line.
#[must_use]
pub fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_over_an_emoji_and_a_combining_accent_as_one_character() {
        // a, emoji (4 bytes; UTF-16: 2 units), e + combining acute (1 + 2 bytes; UTF-16: 2 units), b.
        let line = "a😀e\u{301}b";
        assert_eq!(next_grapheme(line, 0), 1);
        assert_eq!(next_grapheme(line, 1), 5); // UTF-16: 3
        assert_eq!(next_grapheme(line, 5), 8); // UTF-16: 5
        assert_eq!(prev_grapheme(line, 8), 5);
        assert_eq!(prev_grapheme(line, 5), 1);
        assert_eq!(grapheme_count(line), 4);
    }

    #[test]
    fn steps_over_an_emoji_with_its_skin_tone_as_one_character() {
        let line = "👍🏽x"; // thumbs up (4 bytes) + skin tone (4 bytes)
        assert_eq!(next_grapheme(line, 0), 8);
        assert_eq!(prev_grapheme(line, 8), 0);
        assert_eq!(display_column(line, 8, 4), 2);
    }

    #[test]
    fn stops_at_the_ends_of_the_line() {
        assert_eq!(next_grapheme("ab", 2), 2);
        assert_eq!(prev_grapheme("ab", 0), 0);
    }

    #[test]
    fn jumps_like_option_arrows_on_a_mac_over_punctuation_to_the_next_word() {
        let line = "let foo = bar.baz();";
        assert_eq!(next_word_end(line, 0), 3);
        assert_eq!(next_word_end(line, 3), 7);
        assert_eq!(next_word_end(line, 7), 13);
        assert_eq!(prev_word_start(line, 13), 10);
        assert_eq!(prev_word_start(line, 10), 4);
        assert_eq!(prev_word_start(line, 2), 0);
        assert_eq!(next_word_end("  ", 0), 2);
    }

    #[test]
    fn finds_the_word_or_the_run_of_punctuation_under_a_double_click() {
        let line = "foo(bar) baz";
        assert_eq!(word_at(line, 5), 4..7);
        assert_eq!(word_at(line, 4), 4..7);
        assert_eq!(word_at(line, 12), 9..12);
        // UTF-16: 9..14 — "ö" and "ß" are one unit there, two bytes here.
        assert_eq!(word_at("Umlaute: Größe", 11), 9..16);
        assert_eq!(word_at("", 0), 0..0);
    }

    #[test]
    fn counts_letters_digits_marks_and_the_underscore_as_word_characters() {
        for ch in ['a', 'Ö', 'ß', '7', '٣', '\u{301}', '_', '日'] {
            assert!(is_word_char(ch), "{ch:?}");
        }
        for ch in ['.', ' ', '-', '😀', '('] {
            assert!(!is_word_char(ch), "{ch:?}");
        }
    }

    #[test]
    fn expands_tabs_to_the_next_stop() {
        assert_eq!(display_column("\tx", 1, 4), 4);
        assert_eq!(display_column("ab\tx", 3, 4), 4);
        // UTF-16 column 3; here the emoji's four bytes end at 5.
        assert_eq!(display_column("a😀b", 5, 4), 3);
        // UTF-16 column 2; each of the two characters is three bytes.
        assert_eq!(display_column("日本x", 6, 4), 4);
    }

    #[test]
    fn maps_a_display_column_back_to_the_nearest_real_column() {
        assert_eq!(col_for_display_column("\tabc", 5, 4), 2);
        assert_eq!(col_for_display_column("\tabc", 1, 4), 0);
        assert_eq!(col_for_display_column("\tabc", 3, 4), 1);
        assert_eq!(col_for_display_column("ab", 9, 4), 2);
        assert_eq!(col_for_display_column("a😀b", 3, 4), 5); // UTF-16: 3
        assert_eq!(col_for_display_column("a😀b", 1, 4), 1);
    }

    #[test]
    fn gives_a_grapheme_without_width_a_cell_all_the_same() {
        assert_eq!(cell_width("\u{200d}", 0, 4), 1);
        assert_eq!(
            cell_width("\t", 3, 0),
            1,
            "a tab size of 0 counts as 1, never divides by zero"
        );
    }

    #[test]
    fn takes_a_column_inside_a_character_as_its_start_and_never_panics() {
        // "é" is bytes 0..2; column 1 lies inside it.
        assert_eq!(display_column("éa", 1, 4), 0);
        assert_eq!(next_word_end("é a", 1), 2);
        assert_eq!(prev_word_start("aé", 2), 0);
        assert_eq!(word_at("éa", 1), 0..3);
        assert_eq!(next_grapheme("éa", 1), 2);
        assert_eq!(prev_grapheme("éa", 1), 0);
        assert_eq!(
            display_column("ab", 99, 4),
            2,
            "past the end counts as the end"
        );
        assert_eq!(next_word_end("ab", 99), 2);
    }

    #[test]
    fn rounds_a_display_column_inside_a_wide_character_to_its_nearer_edge() {
        // "日" takes cells 1–2 after "a"; cell 2 is its middle, which rounds forward like the TS did.
        assert_eq!(col_for_display_column("a日b", 2, 4), 4);
        assert_eq!(col_for_display_column("a日b", 3, 4), 4);
        let family = "👨\u{200d}👩\u{200d}👧"; // one grapheme of 18 bytes, two cells
        assert_eq!(col_for_display_column(&format!("{family}x"), 1, 4), 18);
        assert_eq!(display_column(&format!("{family}x"), 18, 4), 2);
    }

    #[test]
    fn reads_leading_whitespace() {
        assert_eq!(leading_whitespace("  \tx "), "  \t");
        assert_eq!(leading_whitespace("x"), "");
    }
}
