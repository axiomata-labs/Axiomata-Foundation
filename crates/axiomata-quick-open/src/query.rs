//! What was typed: the text to match and, after a last `:`, the line to jump to.

/// A query split into the text to match and the line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The text to match, trimmed.
    pub text: String,
    /// The line to jump to, zero-based (typed 1-based: `main.rs:12` is line 11; `:0` stays 0).
    pub line: Option<u32>,
}

/// Splits `input` into the text and a trailing `:line` — the last colon followed only by ASCII digits, after the
/// input is trimmed. A space between the colon and the number makes it no line (`main.rs: 12` is all text).
#[must_use]
pub fn parse_query(input: &str) -> Query {
    let trimmed = input.trim();
    if let Some(colon) = trimmed.rfind(':') {
        let digits = &trimmed[colon + 1..];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            // More digits than a line number can have still mean "the last line one can ask for".
            let typed = digits.parse::<u32>().unwrap_or(u32::MAX);
            return Query {
                text: trimmed[..colon].trim().to_owned(),
                line: Some(typed.saturating_sub(1)),
            };
        }
    }
    Query {
        text: trimmed.to_owned(),
        line: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str, line: Option<u32>) -> Query {
        Query {
            text: text.to_owned(),
            line,
        }
    }

    #[test]
    fn splits_off_a_line_to_jump_to() {
        assert_eq!(parse_query("main.rs:12"), q("main.rs", Some(11)));
        assert_eq!(parse_query(" notes "), q("notes", None));
        assert_eq!(parse_query("a:0"), q("a", Some(0)));
    }

    #[test]
    fn takes_the_last_colon_when_the_text_itself_has_one() {
        assert_eq!(parse_query("a:b:12"), q("a:b", Some(11)));
    }

    #[test]
    fn accepts_a_bare_line_with_an_empty_file_name() {
        assert_eq!(parse_query(":12"), q("", Some(11)));
    }

    #[test]
    fn still_finds_the_line_after_trailing_spaces() {
        assert_eq!(parse_query("main.rs:12   "), q("main.rs", Some(11)));
    }

    #[test]
    fn does_not_treat_a_space_before_the_line_number_as_one() {
        assert_eq!(parse_query("main.rs: 12"), q("main.rs: 12", None));
    }

    #[test]
    fn keeps_a_colon_without_digits_as_text() {
        assert_eq!(parse_query("main.rs:"), q("main.rs:", None));
        assert_eq!(parse_query("a:1x"), q("a:1x", None));
    }

    #[test]
    fn saturates_a_line_too_large_to_hold() {
        assert_eq!(parse_query("a:99999999999999"), q("a", Some(u32::MAX - 1)));
    }
}
