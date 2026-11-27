//! What a file tells about itself when it is opened (Axiomata-OS `editor/detect.ts`): its line ending, whether it
//! ends with a line break, and how it is indented. The editor keeps all three when saving, so opening and saving a
//! file never produces a diff by itself.

use std::collections::HashMap;

/// How many leading lines the indentation guess looks at.
const INDENT_SAMPLE_LINES: usize = 200;

/// The line ending a file is saved with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
}

impl LineEnding {
    /// The bytes of one line break.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// Spaces or tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndentKind {
    /// Indented with spaces.
    Spaces,
    /// Indented with tabs.
    Tabs,
}

/// How a file is indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Indent {
    /// Spaces or tabs.
    pub kind: IndentKind,
    /// Spaces per level; for tabs, the width a tab is shown with.
    pub size: usize,
}

/// A file's line ending and final line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileShape {
    /// The line ending it is saved with.
    pub eol: LineEnding,
    /// The file mixed LF and CRLF; it is saved as LF (and the status bar says so).
    pub mixed_eol: bool,
    /// The last line ends with a line break.
    pub final_newline: bool,
}

/// Splits text into lines on LF, CRLF or a lone CR. Always at least one line: an empty text is one empty line.
#[must_use]
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..i]);
                start = i + 1;
            }
            b'\r' => {
                lines.push(&text[start..i]);
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    lines.push(&text[start..]);
    lines
}

/// The line ending and final line break of `text`.
#[must_use]
pub fn detect_shape(text: &str) -> FileShape {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    let mixed = crlf > 0 && lf > 0;
    FileShape {
        eol: if crlf > 0 && !mixed {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        },
        mixed_eol: mixed,
        final_newline: text.ends_with('\n'),
    }
}

/// Guesses the indentation from the first lines: tabs if more lines start with a tab than with spaces; otherwise the
/// most common increase in leading spaces from one indented line to the next. `None` when there is too little to
/// tell — the caller then uses the owner's setting.
#[must_use]
pub fn detect_indent(text: &str) -> Option<Indent> {
    let mut tab_lines = 0_usize;
    let mut space_lines = 0_usize;
    let mut steps: HashMap<usize, usize> = HashMap::new();
    let mut previous = 0_usize;
    for line in split_lines(text).into_iter().take(INDENT_SAMPLE_LINES) {
        // A line of only whitespace says nothing — a byte order mark counts as whitespace, as JavaScript's `trim` has it.
        if line
            .trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            .is_empty()
        {
            continue;
        }
        if line.starts_with('\t') {
            tab_lines += 1;
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        if spaces > 0 {
            space_lines += 1;
        }
        let step = spaces.abs_diff(previous);
        if (2..=8).contains(&step) {
            *steps.entry(step).or_default() += 1;
        }
        previous = spaces;
    }
    if tab_lines > space_lines && tab_lines > 0 {
        return Some(Indent {
            kind: IndentKind::Tabs,
            size: 4,
        });
    }
    if space_lines == 0 {
        return None;
    }
    // The most common step; of two as common, the smaller.
    let (size, _) = steps
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;
    Some(Indent {
        kind: IndentKind::Spaces,
        size,
    })
}

/// Joins the stored text for saving, in the file's own shape. The store never holds the final line break
/// ([`body_for_store`]), so it is always added here when the file had one — also after an empty last line, which is
/// a line of its own.
#[must_use]
pub fn join_for_save(body: &str, shape: FileShape) -> String {
    let sep = shape.eol.as_str();
    let mut joined = split_lines(body).join(sep);
    if shape.final_newline {
        joined.push_str(sep);
    }
    joined
}

/// The text as the store holds it: a final line break is metadata ([`FileShape::final_newline`]), not an empty last
/// line, so moving to the end stops on the last real line and saving restores the break exactly once.
#[must_use]
pub fn body_for_store(text: &str, shape: FileShape) -> &str {
    if !shape.final_newline {
        return text;
    }
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(eol: LineEnding, mixed_eol: bool, final_newline: bool) -> FileShape {
        FileShape {
            eol,
            mixed_eol,
            final_newline,
        }
    }

    #[test]
    fn splits_on_lf_crlf_and_a_lone_cr() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
        assert_eq!(split_lines(""), [""]);
        assert_eq!(split_lines("a\n"), ["a", ""]);
        assert_eq!(split_lines("\r\r\n"), ["", "", ""]);
    }

    #[test]
    fn keeps_lf_or_crlf_and_notices_a_final_line_break() {
        assert_eq!(detect_shape("a\nb\n"), shape(LineEnding::Lf, false, true));
        assert_eq!(
            detect_shape("a\r\nb"),
            shape(LineEnding::Crlf, false, false)
        );
        assert_eq!(detect_shape(""), shape(LineEnding::Lf, false, false));
    }

    #[test]
    fn calls_a_mix_of_both_mixed_and_saves_it_as_lf() {
        assert_eq!(
            detect_shape("a\r\nb\nc"),
            shape(LineEnding::Lf, true, false)
        );
    }

    #[test]
    fn saves_exactly_what_was_opened() {
        for text in [
            "a\nb\n",
            "a\r\nb\r\n",
            "no newline",
            "",
            "\n",
            "a\n\n",
            "x\r\n\r\n",
        ] {
            let shape = detect_shape(text);
            assert_eq!(
                join_for_save(body_for_store(text, shape), shape),
                text,
                "{text:?}"
            );
        }
    }

    #[test]
    fn recognises_tabs() {
        assert_eq!(
            detect_indent("fn a() {\n\tb();\n\t\tc();\n}"),
            Some(Indent {
                kind: IndentKind::Tabs,
                size: 4
            })
        );
    }

    #[test]
    fn recognises_the_step_between_indented_lines() {
        let two = Some(Indent {
            kind: IndentKind::Spaces,
            size: 2,
        });
        let four = Some(Indent {
            kind: IndentKind::Spaces,
            size: 4,
        });
        assert_eq!(detect_indent("a:\n  b:\n    c: 1\n  d: 2"), two);
        assert_eq!(
            detect_indent("fn a() {\n    b();\n    if x {\n        c();\n    }\n}"),
            four
        );
    }

    #[test]
    fn takes_the_smaller_of_two_steps_seen_as_often() {
        // Steps 2 (0→2) and 4 (2→6) once each, then 6→2 is 4 again and 2→0 is 2 again: a tie.
        assert_eq!(
            detect_indent("a\n  b\n      c\n  d\ne"),
            Some(Indent {
                kind: IndentKind::Spaces,
                size: 2
            })
        );
    }

    #[test]
    fn gives_up_when_nothing_is_indented() {
        assert_eq!(detect_indent("# Title\n\nJust prose.\n"), None);
        assert_eq!(detect_indent(""), None);
    }

    #[test]
    fn looks_only_at_the_first_two_hundred_lines() {
        let prose = "word\n".repeat(INDENT_SAMPLE_LINES);
        assert_eq!(detect_indent(&format!("{prose}a\n  b\n    c")), None);
    }

    #[test]
    fn ignores_steps_of_one_or_more_than_eight_and_prefers_spaces_on_a_tie_with_tabs() {
        assert_eq!(detect_indent("a\n b\nc\n         d"), None);
        let spaces = Some(Indent {
            kind: IndentKind::Spaces,
            size: 2,
        });
        assert_eq!(detect_indent("a\n\tb\n  c"), spaces);
    }

    #[test]
    fn a_lone_cr_is_a_line_break_to_the_store_but_not_a_line_ending_of_the_file() {
        assert_eq!(detect_shape("a\rb"), shape(LineEnding::Lf, false, false));
        let lone = detect_shape("a\rb");
        assert_eq!(
            join_for_save(body_for_store("a\rb", lone), lone),
            "a\nb",
            "saving writes it as LF"
        );
        let mixed = detect_shape("a\nb\r\n");
        assert_eq!(mixed, shape(LineEnding::Lf, true, true));
        assert_eq!(body_for_store("a\nb\r\n", mixed), "a\nb");
    }
}
