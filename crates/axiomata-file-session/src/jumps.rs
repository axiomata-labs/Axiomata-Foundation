//! The app-wide jump history across files (Axiomata-OS `docs/plans/tags-links.md`, Q2): every jump into another file —
//! a followed wikilink, a definition elsewhere — leaves the place it started from here, and "back" walks along it,
//! "forward" again. One history for every editor of the app, as in a browser.
//!
//! It walks like Vi's jump list: going back from the newest end first remembers where the cursor is, so forward
//! returns there; a new jump drops what was ahead.

use serde::{Deserialize, Serialize};

/// Most places kept; the oldest go first.
pub const MAX_JUMPS: usize = 100;

/// A place in a file: its root, its path, and the cursor's line and column (zero-based, as the editor counts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JumpPlace {
    /// The root id.
    pub root: String,
    /// The path inside the root.
    pub rel: String,
    /// The cursor's line.
    pub line: u32,
    /// The cursor's column.
    pub col: u32,
}

/// The history itself.
#[derive(Debug, Default)]
pub struct JumpHistory {
    places: Vec<JumpPlace>,
    /// Where back/forward are in `places`; `places.len()` means "at the newest end".
    index: usize,
}

impl JumpHistory {
    /// An empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Before a jump into another file: remember where it started.
    pub fn record(&mut self, from: JumpPlace) {
        self.places.truncate(self.index);
        if self.places.last() != Some(&from) {
            self.places.push(from);
        }
        self.keep_newest();
        self.index = self.places.len();
    }

    /// One place back from `here` (where the cursor is now, if in a file); `None` when there is none.
    pub fn back(&mut self, here: Option<JumpPlace>) -> Option<JumpPlace> {
        if self.index == self.places.len()
            && let Some(here) = here
        {
            // Not trimmed here: the place the walk started from stays reachable by "forward" even with a full history.
            if self.places.last() != Some(&here) {
                self.places.push(here);
            }
            self.index = self.places.len() - 1;
        }
        self.index = self.index.checked_sub(1)?;
        self.places.get(self.index).cloned()
    }

    /// One place forward again; `None` past the newest.
    pub fn forward(&mut self) -> Option<JumpPlace> {
        if self.index + 1 >= self.places.len() {
            return None;
        }
        self.index += 1;
        self.places.get(self.index).cloned()
    }

    fn keep_newest(&mut self) {
        let over = self.places.len().saturating_sub(MAX_JUMPS);
        self.places.drain(..over);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rel: &str, line: u32) -> JumpPlace {
        JumpPlace {
            root: "workspace".into(),
            rel: rel.into(),
            line,
            col: 0,
        }
    }

    #[test]
    fn goes_back_to_where_each_jump_started_and_forward_again_to_where_it_was() {
        let mut h = JumpHistory::new();
        h.record(at("a.md", 3)); // a → b
        h.record(at("b.md", 7)); // b → c
        assert_eq!(h.back(Some(at("c.md", 1))), Some(at("b.md", 7)));
        assert_eq!(h.back(Some(at("b.md", 7))), Some(at("a.md", 3)));
        assert_eq!(h.back(Some(at("a.md", 3))), None);
        assert_eq!(h.forward(), Some(at("b.md", 7)));
        assert_eq!(h.forward(), Some(at("c.md", 1)));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn drops_what_was_ahead_on_a_new_jump() {
        let mut h = JumpHistory::new();
        h.record(at("a.md", 0));
        h.record(at("b.md", 0));
        h.back(Some(at("c.md", 0)));
        h.record(at("b.md", 4)); // from b somewhere new
        assert_eq!(h.forward(), None);
        assert_eq!(h.back(Some(at("d.md", 0))), Some(at("b.md", 4)));
        assert_eq!(h.back(None), Some(at("a.md", 0)));
    }

    #[test]
    fn has_nothing_to_go_back_to_before_the_first_jump() {
        assert_eq!(JumpHistory::new().back(Some(at("a.md", 0))), None);
        assert_eq!(JumpHistory::new().back(None), None);
    }

    #[test]
    fn keeps_the_newest_places_only() {
        let mut h = JumpHistory::new();
        for i in 0..MAX_JUMPS + 10 {
            h.record(at(&format!("n{i}.md"), 0));
        }
        let mut count = 0;
        while h
            .back(if count == 0 {
                Some(at("here.md", 0))
            } else {
                None
            })
            .is_some()
        {
            count += 1;
        }
        assert_eq!(count, MAX_JUMPS);
    }
}
