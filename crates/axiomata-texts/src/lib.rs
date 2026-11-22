//! The apps' texts in English and German (Axiomata-OS `docs/plans/rust-core.md` Q2, Q15): one catalog per area in
//! `catalogs/<area>.toml`, the one source of every text the owner reads. English is the source; German says "du" and
//! never genders.
//!
//! The build script checks every catalog and fails the build on any of: a text missing in one language, a text that
//! counts in one language and not the other, an empty text, the two languages naming different values (`{name}`), a
//! gendered German form. It compiles the catalogs into constants: [`areas`] lists them all, and each area is a module
//! with a constant per key (`errors::BOARD_PLAIN_FLOW_CARD` for `board.plainFlowCard`), so a key a caller names that
//! the catalog lacks does not compile.
//!
//! Rust logic hands out codes and values; the text is chosen where it is shown ([`Entry::get`], [`fill`]). The
//! Axiomata app writes its TypeScript catalogs from [`areas`] (`apps/axiomata/src-tauri/src/bindings.rs`).
//!
//! A catalog's file:
//!
//! ```toml
//! ["card.missing"]
//! en = "There is no card {id}."
//! de = "Es gibt keine Karte {id}."
//!
//! [cardsLeft]
//! en = { one = "One card is left.", other = "{n} cards are left." }
//! de = { one = "Eine Karte ist übrig.", other = "{n} Karten sind übrig." }
//! ```

#![forbid(unsafe_code)]

/// The languages of the catalogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// English, the source.
    En,
    /// German.
    De,
}

impl Lang {
    /// The language a tag names: German for `de` and its regions (`de-AT`, `de_CH`), English for any other.
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        let german = tag
            .get(..2)
            .is_some_and(|head| head.eq_ignore_ascii_case("de"))
            && matches!(tag.as_bytes().get(2), None | Some(b'-' | b'_'));
        if german { Self::De } else { Self::En }
    }
}

/// One text in one language: plain, or a form for one and one for every other number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    /// The same for every number.
    Plain(&'static str),
    /// For exactly one (`one`) and for every other number (`other`).
    Plural {
        /// For exactly 1.
        one: &'static str,
        /// For 0 and 2 and more.
        other: &'static str,
    },
}

impl Text {
    /// The form for the count `n`: German and English both use one form for exactly 1 and another for the rest.
    #[must_use]
    pub fn form(self, n: u64) -> &'static str {
        match self {
            Self::Plain(text) => text,
            Self::Plural { one, other } => {
                if n == 1 {
                    one
                } else {
                    other
                }
            }
        }
    }
}

/// One key of a catalog, in both languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// The key as the catalog writes it (`board.plainFlowCard`).
    pub key: &'static str,
    /// The English text.
    pub en: Text,
    /// The German text.
    pub de: Text,
}

impl Entry {
    /// The text in `lang`; a text that counts gives its form for many.
    #[must_use]
    pub fn get(&self, lang: Lang) -> &'static str {
        self.text(lang).form(u64::MAX)
    }

    /// The text in `lang` for the count `n`.
    #[must_use]
    pub fn count(&self, lang: Lang, n: u64) -> &'static str {
        self.text(lang).form(n)
    }

    /// The text in `lang`, both forms if it counts.
    #[must_use]
    pub fn text(&self, lang: Lang) -> Text {
        match lang {
            Lang::En => self.en,
            Lang::De => self.de,
        }
    }
}

/// One catalog: an area of an app and its keys, sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    /// The area's name, the catalog's file name (`errors`).
    pub name: &'static str,
    /// Its keys, in the catalog's order.
    pub entries: &'static [Entry],
}

/// `template` with each `{name}` filled from `values` — a name is a letter, then letters, digits and `_`, as the
/// catalogs' check reads it —; a name without a value stays as it is, so a missing value shows rather than vanishes.
#[must_use]
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = rules::placeholder(after).and_then(|name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (name, *value))
        });
        if let Some((name, value)) = value {
            out.push_str(value);
            rest = &after[name.len() + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

// The build script's checks: the library reads only `placeholder` from them and runs the rest in its tests.
#[allow(
    dead_code,
    reason = "the build script uses these rules; the library tests them"
)]
mod rules;

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_filled_by_name_and_a_missing_one_shows() {
        assert_eq!(
            fill("Karte {id} von {who}.", &[("id", "7"), ("who", "dir")]),
            "Karte 7 von dir."
        );
        assert_eq!(fill("Karte {id}.", &[]), "Karte {id}.");
        assert_eq!(fill("{ x } {} {a", &[("a", "1")]), "{ x } {} {a");
        // A name the catalogs' check would not read is no placeholder here either.
        assert_eq!(fill("{1x} {_a}", &[("1x", "!"), ("_a", "!")]), "{1x} {_a}");
        assert_eq!(fill("Größe {n}", &[("n", "3")]), "Größe 3");
    }

    #[test]
    fn a_count_picks_one_or_other() {
        let text = Text::Plural {
            one: "eine",
            other: "viele",
        };
        assert_eq!(text.form(1), "eine");
        assert_eq!(text.form(0), "viele");
        assert_eq!(Text::Plain("immer").form(1), "immer");
    }

    #[test]
    fn a_tag_names_its_language() {
        assert_eq!(Lang::from_tag("de-AT"), Lang::De);
        assert_eq!(Lang::from_tag("DE"), Lang::De);
        assert_eq!(Lang::from_tag("en"), Lang::En);
        assert_eq!(Lang::from_tag(""), Lang::En);
        assert_eq!(Lang::from_tag("de_CH"), Lang::De);
        for other in ["deu", "del", "d"] {
            assert_eq!(Lang::from_tag(other), Lang::En, "{other}");
        }
    }

    #[test]
    fn every_area_is_listed_by_name_with_keys_named_once() {
        let names: Vec<&str> = areas().iter().map(|area| area.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for area in areas() {
            let mut keys: Vec<&str> = area.entries.iter().map(|e| e.key).collect();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), area.entries.len(), "{}", area.name);
        }
    }
}
