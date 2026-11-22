//! Checks the catalogs in `catalogs/` and compiles them into `$OUT_DIR/catalogs.rs` (see the crate's docs). A catalog
//! that breaks a rule fails the build, naming the area, the key and the rule.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

/// One text of one language as the catalog writes it.
#[derive(Clone)]
enum Text {
    Plain(String),
    Plural { one: String, other: String },
}

impl Text {
    fn forms(&self) -> Vec<&str> {
        match self {
            Self::Plain(text) => vec![text],
            Self::Plural { one, other } => vec![one, other],
        }
    }

    fn rust(&self) -> String {
        match self {
            Self::Plain(text) => format!("Text::Plain({text:?})"),
            Self::Plural { one, other } => {
                format!("Text::Plural {{ one: {one:?}, other: {other:?} }}")
            }
        }
    }
}

struct Entry {
    key: String,
    en: Text,
    de: Text,
}

fn main() {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("cargo sets it")).join("catalogs");
    println!("cargo::rerun-if-changed=catalogs");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("the catalogs folder")
        .map(|entry| entry.expect("a catalog").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    let mut areas: Vec<(String, Vec<Entry>)> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for path in files {
        println!("cargo::rerun-if-changed={}", path.display());
        let area = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a UTF-8 file name")
            .to_owned();
        if !area.starts_with(|c: char| c.is_ascii_lowercase())
            || !area.chars().all(|c| c.is_ascii_alphanumeric())
        {
            problems.push(format!(
                "{area}: an area is named in camelCase letters and digits"
            ));
            continue;
        }
        let source = fs::read_to_string(&path).expect("a readable catalog");
        let table: toml::Table = match source.parse() {
            Ok(table) => table,
            Err(err) => {
                problems.push(format!("{area}: {err}"));
                continue;
            }
        };
        let mut entries = Vec::new();
        for (key, value) in table {
            match entry(&key, &value) {
                Ok(entry) => {
                    problems.extend(
                        check(&entry)
                            .into_iter()
                            .map(|p| format!("{area}.{key}: {p}")),
                    );
                    entries.push(entry);
                }
                Err(problem) => problems.push(format!("{area}.{key}: {problem}")),
            }
        }
        let mut idents = BTreeMap::new();
        for entry in &entries {
            if let Some(other) = idents.insert(ident(&entry.key), entry.key.clone()) {
                problems.push(format!(
                    "{area}: {} and {other} name the same constant",
                    entry.key
                ));
            }
        }
        areas.push((area, entries));
    }
    if !problems.is_empty() {
        panic!(
            "the text catalogs break a rule:\n  {}",
            problems.join("\n  ")
        );
    }
    let out = Path::new(&env::var("OUT_DIR").expect("cargo sets it")).join("catalogs.rs");
    fs::write(out, generate(&areas)).expect("the generated catalogs");
}

/// The entry `key` = `value` of a catalog.
fn entry(key: &str, value: &toml::Value) -> Result<Entry, String> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    {
        return Err("a key holds letters, digits, `.` and `_` only".to_owned());
    }
    let toml::Value::Table(langs) = value else {
        return Err("a key is a table of `en` and `de`".to_owned());
    };
    if let Some(extra) = langs.keys().find(|k| *k != "en" && *k != "de") {
        return Err(format!("`{extra}` is no language of the catalogs (en, de)"));
    }
    let text = |lang: &str| -> Result<Text, String> {
        match langs.get(lang) {
            None => Err(format!("no `{lang}` text")),
            Some(toml::Value::String(text)) => Ok(Text::Plain(text.clone())),
            Some(toml::Value::Table(forms)) => {
                let form = |name: &str| match forms.get(name) {
                    Some(toml::Value::String(text)) => Ok(text.clone()),
                    _ => Err(format!(
                        "`{lang}` counts, so it needs `one` and `other` texts"
                    )),
                };
                if forms.len() != 2 {
                    return Err(format!("`{lang}` counts with exactly `one` and `other`"));
                }
                Ok(Text::Plural {
                    one: form("one")?,
                    other: form("other")?,
                })
            }
            Some(_) => Err(format!("`{lang}` is a text, or `one` and `other` texts")),
        }
    };
    Ok(Entry {
        key: key.to_owned(),
        en: text("en")?,
        de: text("de")?,
    })
}

/// What is wrong with `entry`: the two languages of another kind, an empty text, other values named, a gendered form.
fn check(entry: &Entry) -> Vec<String> {
    let mut problems = Vec::new();
    let (en, de) = (entry.en.forms(), entry.de.forms());
    if en.len() != de.len() {
        problems.push("counts in one language and not in the other".to_owned());
        return problems;
    }
    for (e, d) in en.iter().zip(&de) {
        if e.trim().is_empty() || d.trim().is_empty() {
            problems.push("a text is empty".to_owned());
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

/// The values a text names (`{name}`).
fn names(text: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if len > 0
            && after[len..].starts_with('}')
            && after.starts_with(|c: char| c.is_ascii_alphabetic())
        {
            out.insert(&after[..len]);
        }
        rest = after;
    }
    out
}

/// The gendered form in `text`, if any (Axiomata-OS `docs/plans/i18n.md` Q23): `*innen`, `:innen`, `_innen`, a capital
/// `Innen` inside a word, and the participles that stand in for the plain word.
fn gendered(text: &str) -> Option<String> {
    const PARTICIPLES: [&str; 4] = ["Benutzende", "Nutzende", "Teilnehmende", "Mitarbeitende"];
    let chars: Vec<char> = text.chars().collect();
    for (i, window) in chars.windows(5).enumerate() {
        let word: String = window.iter().collect();
        let before = i.checked_sub(1).map(|b| chars[b]);
        let after_ok = chars.get(i + 5).is_none_or(|c| !c.is_alphanumeric());
        if word == "innen" && after_ok && matches!(before, Some('*' | ':' | '_')) {
            return Some(format!("{}innen", before.unwrap_or_default()));
        }
        if word == "Innen" && after_ok && before.is_some_and(char::is_lowercase) {
            return Some("…Innen".to_owned());
        }
    }
    text.split(|c: char| !c.is_alphanumeric())
        .find(|word| PARTICIPLES.contains(word))
        .map(str::to_owned)
}

/// The constant a key becomes: `board.plainFlowCard` → `BOARD_PLAIN_FLOW_CARD`.
fn ident(key: &str) -> String {
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
fn module(area: &str) -> String {
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

fn generate(areas: &[(String, Vec<Entry>)]) -> String {
    let mut out = String::from(
        "// Generated by the build script from `catalogs/` — edit the catalogs, not this.\n\n",
    );
    for (area, entries) in areas {
        let _ = writeln!(
            out,
            "/// The texts of the area `{area}` (`catalogs/{area}.toml`)."
        );
        let _ = writeln!(out, "pub mod {} {{", module(area));
        out.push_str("    use crate::{Entry, Text};\n\n");
        for entry in entries {
            let _ = writeln!(out, "    /// `{}`.", entry.key);
            let _ = writeln!(
                out,
                "    pub const {}: Entry = Entry {{ key: {:?}, en: {}, de: {} }};",
                ident(&entry.key),
                entry.key,
                entry.en.rust(),
                entry.de.rust()
            );
        }
        let list: Vec<String> = entries.iter().map(|e| ident(&e.key)).collect();
        let _ = writeln!(
            out,
            "\n    /// Every key of the area, sorted.\n    pub const ENTRIES: &[Entry] = &[{}];",
            list.join(", ")
        );
        out.push_str("}\n\n");
    }
    let list: Vec<String> = areas
        .iter()
        .map(|(area, _)| {
            format!(
                "Area {{ name: {area:?}, entries: {}::ENTRIES }}",
                module(area)
            )
        })
        .collect();
    let _ = writeln!(
        out,
        "/// Every area, sorted by name.\npub fn areas() -> &'static [Area] {{\n    const AREAS: &[Area] = &[{}];\n    AREAS\n}}",
        list.join(", ")
    );
    out
}
