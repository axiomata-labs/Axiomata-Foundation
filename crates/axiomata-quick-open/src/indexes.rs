//! The file lists quick open matches against, one per root (S1, S5, S6; W14 of the editor plan).
//!
//! An index is served at once and read again in the background once it is older than [`INDEX_FRESH`] — there is no
//! watcher over whole trees. What the app itself changes (a file created, renamed, deleted, a note filed) marks the
//! index of its root stale: the next lookup reads it again at once, and serves the old one until the new one is in. A
//! read that began before such a change brings an index that is still stale.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long an index is served as it is before it is read again.
pub const INDEX_FRESH: Duration = Duration::from_secs(30);

/// The most roots kept at once; beyond, the one used longest ago goes.
pub const MAX_INDEXES: usize = 8;

/// What a lookup over some roots has to work with.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The files of every root that has an index, by root id.
    pub files: Vec<(String, Arc<[String]>)>,
    /// The roots the caller must read now and hand in with [`Indexes::put`] (or [`Indexes::read_failed`]).
    pub to_read: Vec<String>,
    /// A root has no index yet: the list is not complete.
    pub loading: bool,
    /// An index stopped at its limit: some files are missing.
    pub truncated: bool,
}

/// The indexes of the roots quick open has looked at, while the app runs.
#[derive(Debug, Default)]
pub struct Indexes {
    entries: HashMap<String, Entry>,
    /// The roots being read, each with its generation when the read began.
    reading: HashMap<String, u64>,
    /// Counts, per root, the changes the app made to its files.
    generations: HashMap<String, u64>,
    /// Counts lookups and reads, to know which root was used longest ago.
    tick: u64,
}

#[derive(Debug)]
struct Entry {
    files: Arc<[String]>,
    truncated: bool,
    read_at: Instant,
    /// The app changed files under the root after this list was read.
    stale: bool,
    used: u64,
}

impl Indexes {
    /// No index yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What there is for `roots` now (each named once, however often it is listed), and which of them to read: one
    /// without an index, an old or a stale one — unless a read of it is already on its way.
    pub fn take(&mut self, roots: &[String], now: Instant) -> Snapshot {
        let mut snapshot = Snapshot::default();
        let mut seen = HashSet::new();
        for root in roots {
            if !seen.insert(root.as_str()) {
                continue;
            }
            self.tick += 1;
            let wants_read = if let Some(entry) = self.entries.get_mut(root) {
                entry.used = self.tick;
                snapshot
                    .files
                    .push((root.clone(), Arc::clone(&entry.files)));
                snapshot.truncated |= entry.truncated;
                entry.stale || now.saturating_duration_since(entry.read_at) >= INDEX_FRESH
            } else {
                snapshot.loading = true;
                true
            };
            if wants_read && !self.reading.contains_key(root) {
                self.reading.insert(root.clone(), self.generation(root));
                snapshot.to_read.push(root.clone());
            }
        }
        snapshot
    }

    /// The files of `root` as just read. Stale already when the app changed files there while it was read.
    pub fn put(&mut self, root: &str, files: Vec<String>, truncated: bool, now: Instant) {
        let began = self.reading.remove(root);
        let stale = began.is_some_and(|generation| generation != self.generation(root));
        self.tick += 1;
        self.entries.insert(
            root.to_owned(),
            Entry {
                files: files.into(),
                truncated,
                read_at: now,
                stale,
                used: self.tick,
            },
        );
        self.evict(root);
    }

    /// Reading `root` failed: an index it had stays and counts as read now; without one it gets an empty one. Either
    /// way it is tried again once that is old — not on every keystroke, not even for a stale one (a root that no longer
    /// resolves would be read again and again while quick open is open), and the list does not wait for it forever.
    pub fn read_failed(&mut self, root: &str, now: Instant) {
        self.reading.remove(root);
        if let Some(entry) = self.entries.get_mut(root) {
            entry.read_at = now;
            entry.stale = false;
        } else {
            self.put(root, Vec::new(), false, now);
        }
    }

    /// The app changed files under `root`: its index is read again at the next lookup. A root with neither an index nor
    /// a read on its way has nothing to mark — its first read sees the change anyway.
    pub fn mark_stale(&mut self, root: &str) {
        let known = self.entries.contains_key(root);
        if !known && !self.reading.contains_key(root) {
            return;
        }
        *self.generations.entry(root.to_owned()).or_default() += 1;
        if let Some(entry) = self.entries.get_mut(root) {
            entry.stale = true;
        }
    }

    /// How many roots have an index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no root has an index.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn generation(&self, root: &str) -> u64 {
        self.generations.get(root).copied().unwrap_or(0)
    }

    /// Drops the roots used longest ago beyond [`MAX_INDEXES`], never `keep`.
    fn evict(&mut self, keep: &str) {
        while self.entries.len() > MAX_INDEXES {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(root, _)| root.as_str() != keep)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(root, _)| root.clone())
            else {
                return;
            };
            self.entries.remove(&oldest);
            if !self.reading.contains_key(&oldest) {
                self.generations.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    fn files(names: &[&str]) -> Vec<String> {
        roots(names)
    }

    /// No root to read.
    const NONE: [String; 0] = [];

    fn served(snapshot: &Snapshot, root: &str) -> Option<Vec<String>> {
        snapshot
            .files
            .iter()
            .find(|(r, _)| r == root)
            .map(|(_, f)| f.to_vec())
    }

    #[test]
    fn a_root_without_an_index_is_read_once_and_loading_until_it_is_in() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        let first = indexes.take(&roots(&["p"]), t0);
        assert!(first.loading);
        assert_eq!(first.to_read, ["p"]);
        // The read is on its way: a second keystroke starts no second one.
        let second = indexes.take(&roots(&["p"]), t0);
        assert!(second.loading);
        assert_eq!(second.to_read, NONE);

        indexes.put("p", files(&["a.md"]), false, t0);
        let third = indexes.take(&roots(&["p"]), t0);
        assert!(!third.loading);
        assert_eq!(third.to_read, NONE);
        assert_eq!(served(&third, "p"), Some(files(&["a.md"])));
    }

    #[test]
    fn an_old_index_is_served_and_read_again() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p"]), t0);
        indexes.put("p", files(&["a.md"]), false, t0);
        let just_before = t0 + Duration::from_millis(29_999);
        assert_eq!(indexes.take(&roots(&["p"]), just_before).to_read, NONE);
        let later = indexes.take(&roots(&["p"]), t0 + INDEX_FRESH);
        assert_eq!(later.to_read, ["p"]);
        assert!(!later.loading);
        assert_eq!(served(&later, "p"), Some(files(&["a.md"])));
    }

    #[test]
    fn a_change_by_the_app_reads_the_index_again_at_once() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p"]), t0);
        indexes.put("p", files(&["a.md"]), false, t0);
        indexes.mark_stale("p");
        let next = indexes.take(&roots(&["p"]), t0);
        assert_eq!(next.to_read, ["p"]);
        assert_eq!(served(&next, "p"), Some(files(&["a.md"])));
        indexes.put("p", files(&["a.md", "b.md"]), false, t0);
        assert_eq!(indexes.take(&roots(&["p"]), t0).to_read, NONE);
    }

    #[test]
    fn a_read_that_began_before_a_change_brings_a_stale_index() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p"]), t0);
        indexes.mark_stale("p");
        indexes.put("p", files(&["a.md"]), false, t0);
        assert_eq!(indexes.take(&roots(&["p"]), t0).to_read, ["p"]);
    }

    #[test]
    fn a_change_to_another_root_leaves_this_one_alone() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p"]), t0);
        indexes.put("p", files(&["a.md"]), false, t0);
        indexes.mark_stale("q");
        assert_eq!(indexes.take(&roots(&["p"]), t0).to_read, NONE);
    }

    #[test]
    fn keeps_at_most_eight_roots_dropping_the_one_used_longest_ago() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        for i in 0..MAX_INDEXES {
            let root = format!("r{i}");
            indexes.take(std::slice::from_ref(&root), t0);
            indexes.put(&root, Vec::new(), false, t0);
        }
        // r0 is used again, so r1 is the one used longest ago.
        indexes.take(&roots(&["r0"]), t0);
        indexes.take(&roots(&["r8"]), t0);
        indexes.put("r8", Vec::new(), false, t0);
        assert_eq!(indexes.len(), MAX_INDEXES);
        assert!(indexes.take(&roots(&["r1"]), t0).loading);
        assert!(!indexes.take(&roots(&["r0"]), t0).loading);
    }

    #[test]
    fn a_failed_read_does_not_leave_the_list_waiting_and_is_tried_again_later() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["gone"]), t0);
        indexes.read_failed("gone", t0);
        let next = indexes.take(&roots(&["gone"]), t0);
        assert!(!next.loading);
        assert_eq!(next.to_read, NONE);
        assert_eq!(
            indexes.take(&roots(&["gone"]), t0 + INDEX_FRESH).to_read,
            ["gone"]
        );
    }

    #[test]
    fn a_failed_read_keeps_the_index_there_was() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p"]), t0);
        indexes.put("p", files(&["a.md"]), false, t0);
        let later = t0 + INDEX_FRESH;
        indexes.take(&roots(&["p"]), later);
        indexes.read_failed("p", later);
        let next = indexes.take(&roots(&["p"]), later);
        assert_eq!(served(&next, "p"), Some(files(&["a.md"])));
        assert_eq!(next.to_read, NONE);
    }

    #[test]
    fn a_stale_index_whose_read_fails_waits_for_the_fresh_window() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p"]), t0);
        indexes.put("p", files(&["a.md"]), false, t0);
        indexes.mark_stale("p");
        assert_eq!(indexes.take(&roots(&["p"]), t0).to_read, ["p"]);
        indexes.read_failed("p", t0);
        // Not read again on the next lookup: that would loop while quick open is open.
        assert_eq!(indexes.take(&roots(&["p"]), t0).to_read, NONE);
        assert_eq!(
            indexes.take(&roots(&["p"]), t0 + INDEX_FRESH).to_read,
            ["p"]
        );
    }

    #[test]
    fn marking_a_root_never_looked_at_keeps_nothing() {
        let mut indexes = Indexes::new();
        indexes.mark_stale("never");
        assert!(indexes.generations.is_empty());
    }

    #[test]
    fn says_when_an_index_stopped_at_its_limit() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        indexes.take(&roots(&["p", "q"]), t0);
        indexes.put("p", files(&["a.md"]), true, t0);
        indexes.put("q", files(&["b.md"]), false, t0);
        assert!(indexes.take(&roots(&["p", "q"]), t0).truncated);
        assert!(!indexes.take(&roots(&["q"]), t0).truncated);
    }

    #[test]
    fn a_root_listed_twice_counts_once() {
        let mut indexes = Indexes::new();
        let t0 = Instant::now();
        let first = indexes.take(&roots(&["p", "p"]), t0);
        assert_eq!(first.to_read, ["p"]);
        indexes.put("p", files(&["a.md"]), false, t0);
        assert_eq!(indexes.take(&roots(&["p", "p"]), t0).files.len(), 1);
    }
}
