//! The rules a catalog's texts and names keep — read by the build script (`#[path]`) and tested here, so a rule that
//! lets something slip is caught by `cargo test`, not by a catalog that happens to break it.

/// The values a text names (`{name}`, a letter first, then letters, digits and `_`), sorted, each as often as it is
/// named — the reading of the app's TypeScript catalog check.
pub(crate) fn names(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        if let Some(name) = placeholder(after) {
            out.push(name);
        }
        rest = after;
    }
    out.sort_unstable();
    out
}

/// The name of the placeholder `after` starts with (the text after a `{`), if it is one: a letter, then letters,
/// digits and `_`, then `}`.
pub(crate) fn placeholder(after: &str) -> Option<&str> {
    let len = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after.len());
    let name = &after[..len];
    (name.starts_with(|c: char| c.is_ascii_alphabetic()) && after[len..].starts_with('}'))
        .then_some(name)
}

/// Whether `text` holds a `{` or `}` that belongs to no placeholder: a typo (`{name`, `{ name}`) the app would show
/// as it is. The catalogs need no literal brace.
pub(crate) fn stray_brace(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find(['{', '}']) {
        if rest[at..].starts_with('}') {
            return true;
        }
        let after = &rest[at + 1..];
        match placeholder(after) {
            Some(name) => rest = &after[name.len() + 1..],
            None => return true,
        }
    }
    false
}

/// The participles that stand in for a plain noun ("Studierende" for "Studenten"), without their ending: each counts
/// with `-e` and `-en`.
const PARTICIPLES: [&str; 10] = [
    "Studierend",
    "Lehrend",
    "Lesend",
    "Nutzend",
    "Benutzend",
    "Teilnehmend",
    "Mitarbeitend",
    "Forschend",
    "Entwickelnd",
    "Mitwirkend",
];

/// The gendered form in `text`, if any (Axiomata-OS `docs/plans/i18n.md` Q23; the owner never wants to read one):
/// `*innen`, `:innen`, `_innen`, `/innen`, `(innen)` — also inside a compound —, the singular `*in`, `:in`, `_in`,
/// `/in` after a word, a capital `Innen` inside a word, and the participles that stand in for the plain word.
pub(crate) fn gendered(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    for i in 0..chars.len() {
        let mark = chars[i];
        if matches!(mark, '*' | ':' | '_' | '/' | '(') {
            let word: String = chars[i + 1..].iter().take(5).collect();
            if word == "innen" {
                return Some(format!("{mark}innen"));
            }
            let singular = word.starts_with("in") && at(i + 3).is_none_or(|c| !c.is_alphanumeric());
            let after_word = i
                .checked_sub(1)
                .and_then(at)
                .is_some_and(char::is_alphabetic);
            if singular && after_word {
                return Some(format!("{mark}in"));
            }
        }
        if chars[i..].starts_with(&['I', 'n', 'n', 'e', 'n'])
            && i.checked_sub(1)
                .and_then(at)
                .is_some_and(char::is_lowercase)
        {
            return Some("…Innen".to_owned());
        }
    }
    text.split(|c: char| !c.is_alphanumeric()).find_map(|word| {
        PARTICIPLES
            .iter()
            .any(|base| word == format!("{base}e") || word == format!("{base}en"))
            .then(|| word.to_owned())
    })
}

/// Names a generated module or constant must not take: the list of an area's keys, and Rust's own words.
const RESERVED: [&str; 41] = [
    "ENTRIES", "tests", "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
    "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
    "trait", "true", "type", "unsafe", "use", "where", "while", "_",
];

/// Whether `name` (a generated module or constant) is taken by the generated code or by Rust.
pub(crate) fn reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// The constant a key becomes: `board.plainFlowCard` → `BOARD_PLAIN_FLOW_CARD`; a leading digit gets a `_`.
pub(crate) fn ident(key: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '.' || c == '_' {
            out.push('_');
            prev_lower = false;
        } else if c.is_ascii_uppercase() && prev_lower {
            out.push('_');
            out.push(c);
            prev_lower = false;
        } else {
            out.push(c.to_ascii_uppercase());
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// The module an area becomes: `editorEngine` → `editor_engine`.
pub(crate) fn module(area: &str) -> String {
    let mut out = String::new();
    for c in area.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// What is wrong with a key's texts, given its English and German forms (one form, or `one` and `other`): another
/// kind in each language, an empty text, a stray brace, other values named, a gendered German form.
pub(crate) fn check(en: &[&str], de: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    if en.len() != de.len() {
        problems.push("counts in one language and not in the other".to_owned());
        return problems;
    }
    for (e, d) in en.iter().zip(de) {
        if e.trim().is_empty() || d.trim().is_empty() {
            problems.push("a text is empty".to_owned());
        }
        if stray_brace(e) || stray_brace(d) {
            problems.push("a `{` or `}` belongs to no `{name}`".to_owned());
        }
        if names(e) != names(d) {
            problems.push(format!(
                "English names {:?}, German {:?}",
                names(e),
                names(d)
            ));
        }
        if let Some(form) = gendered(d) {
            problems.push(format!("German genders (“{form}”); write the plain word"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_as_the_apps_check_reads_them() {
        assert_eq!(names("{b} and {a} and {a}"), ["a", "a", "b"]);
        assert_eq!(names("{1x} {_a} { a} {}"), Vec::<&str>::new());
        assert_eq!(names("{n_2}"), ["n_2"]);
    }

    #[test]
    fn a_brace_outside_a_placeholder_is_a_typo() {
        assert!(!stray_brace("Karte {id} von {who}."));
        assert!(stray_brace("Karte {id von"));
        assert!(stray_brace("Karte { id}"));
        assert!(stray_brace("Karte id}"));
        assert!(stray_brace("{}"));
    }

    #[test]
    fn every_gendered_form_is_found() {
        for text in [
            "die Nutzer*innen",
            "die Nutzer:innen",
            "die Nutzer_innen",
            "die Nutzer/innen",
            "die Nutzer(innen)",
            "die Nutzer*innenliste",
            "die NutzerInnen",
            "die BürgerInnenmeinung",
            "ein Nutzer*in",
            "eine Nutzer:in.",
            "Nutzer/in",
            "die Studierenden",
            "alle Studierende",
            "für Lehrende",
            "den Teilnehmenden",
        ] {
            assert!(gendered(text).is_some(), "{text}");
        }
    }

    #[test]
    fn plain_german_is_left_alone() {
        for text in [
            "Die Nutzer und Nutzerinnen",
            "Innen und außen",
            "Hinweis: in der Liste",
            "Ordner/Datei",
            "lesende Zugriffe",
            "die Studenten, Lehrer und Forscher",
            "{n} Einträge (in Arbeit)",
        ] {
            assert_eq!(gendered(text), None, "{text}");
        }
    }

    #[test]
    fn keys_and_areas_become_names_rust_accepts() {
        assert_eq!(ident("board.plainFlowCard"), "BOARD_PLAIN_FLOW_CARD");
        assert_eq!(ident("x2Fast"), "X2_FAST");
        assert_eq!(ident("1st"), "_1ST");
        assert_eq!(module("editorEngine"), "editor_engine");
        assert!(reserved(&ident("entries")) && reserved("tests") && reserved("type"));
        assert!(!reserved("errors"));
    }

    #[test]
    fn a_keys_texts_are_checked_together() {
        assert_eq!(check(&["Hi {a}"], &["Hallo {a}"]), Vec::<String>::new());
        assert_eq!(check(&["one", "many"], &["viele"]).len(), 1);
        let problems = check(&["Hi {a} {a}"], &["Hallo {a} für Nutzer*innen {"]);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(check(&[" "], &["x"]), ["a text is empty"]);
    }
}
