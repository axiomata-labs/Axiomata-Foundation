//! Checks the catalogs in `catalogs/` and compiles them into `$OUT_DIR/catalogs.rs` (see the crate's docs). A catalog
//! that breaks a rule fails the build, naming the area, the key and the rule.

#[path = "src/rules.rs"]
mod rules;

use std::collections::BTreeMap;
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
        if rules::reserved(&rules::module(&area)) {
            problems.push(format!(
                "{area}: the module `{}` is a name Rust keeps",
                rules::module(&area)
            ));
            continue;
        }
        let source =
            fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
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
                    let found = rules::check(&entry.en.forms(), &entry.de.forms());
                    problems.extend(found.into_iter().map(|p| format!("{area}.{key}: {p}")));
                    entries.push(entry);
                }
                Err(problem) => problems.push(format!("{area}.{key}: {problem}")),
            }
        }
        let mut idents = BTreeMap::new();
        for entry in &entries {
            let ident = rules::ident(&entry.key);
            if rules::reserved(&ident) {
                problems.push(format!(
                    "{area}.{}: the constant `{ident}` is a name Rust or the catalog keeps",
                    entry.key
                ));
            }
            if let Some(other) = idents.insert(ident, entry.key.clone()) {
                problems.push(format!(
                    "{area}: {} and {other} name the same constant",
                    entry.key
                ));
            }
        }
        areas.push((area, entries));
    }
    assert!(
        problems.is_empty(),
        "the text catalogs break a rule:\n  {}",
        problems.join("\n  ")
    );
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

fn generate(areas: &[(String, Vec<Entry>)]) -> String {
    let mut out = String::from(
        "// Generated by the build script from `catalogs/` — edit the catalogs, not this.\n\n",
    );
    for (area, entries) in areas {
        let _ = writeln!(
            out,
            "/// The texts of the area `{area}` (`catalogs/{area}.toml`)."
        );
        let _ = writeln!(out, "pub mod {} {{", rules::module(area));
        out.push_str("    use crate::{Entry, Text};\n\n");
        for entry in entries {
            let _ = writeln!(out, "    /// `{}`.", entry.key);
            let _ = writeln!(
                out,
                "    pub const {}: Entry = Entry {{ key: {:?}, en: {}, de: {} }};",
                rules::ident(&entry.key),
                entry.key,
                entry.en.rust(),
                entry.de.rust()
            );
        }
        let list: Vec<String> = entries.iter().map(|e| rules::ident(&e.key)).collect();
        let _ = writeln!(
            out,
            "\n    /// Every key of the area, in the catalog's order.\n    pub const ENTRIES: &[Entry] = &[{}];",
            list.join(", ")
        );
        out.push_str("}\n\n");
    }
    let list: Vec<String> = areas
        .iter()
        .map(|(area, _)| {
            format!(
                "Area {{ name: {area:?}, entries: {}::ENTRIES }}",
                rules::module(area)
            )
        })
        .collect();
    let _ = writeln!(
        out,
        "/// Every area, sorted by name.\n#[must_use]\npub fn areas() -> &'static [Area] {{\n    const AREAS: &[Area] = &[{}];\n    AREAS\n}}",
        list.join(", ")
    );
    out
}
