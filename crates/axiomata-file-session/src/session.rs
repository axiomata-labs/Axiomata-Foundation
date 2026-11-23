//! One open file: the rules around the editor's text (Axiomata-OS `docs/plans/editor.md` F8–F10, W4, W13, L11).

use serde::Serialize;

use crate::disk::{Disk, DiskText, Kept, Version};

/// The root id prefix of a language server's files outside every root (L11): shown, never written or watched.
pub const FOREIGN_ROOT: &str = "lsp:";

/// The key a new note's draft is kept aside under — a key, not a place on disk.
pub const DRAFT_ROOT: &str = "new-note";
/// `.md`, so the draft gets Markdown's colours and preview like the note it becomes.
pub const DRAFT_REL: &str = "Untitled.md";

/// What the editor shows above the text, if anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "FileBanner"))]
pub enum Banner {
    /// Unsaved text from an earlier session was found.
    #[serde(rename_all = "camelCase")]
    Recovery {
        /// When it was kept (RFC 3339).
        saved_at: String,
        /// The file changed since the text was kept: restoring it arms the next save.
        changed_since: bool,
    },
    /// The file changed on disk while there are unsaved edits (F10).
    #[serde(rename_all = "camelCase")]
    External {
        /// "Reload" was clicked once over unsaved edits; the next click reloads.
        confirm_reload: bool,
    },
    /// "Keep mine" was chosen; the next save asks before overwriting.
    ConfirmOverwrite,
    /// The file was deleted on disk.
    Deleted,
    /// The clean document was reloaded — a hint, not a question.
    Reloaded,
    /// Saving or filing failed.
    Failed {
        /// What was being done.
        action: FailedAction,
        /// The file service's or the store's own words.
        error: String,
    },
}

/// What a [`Banner::Failed`] was doing, for the sentence around its error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum FailedAction {
    /// Writing the file.
    Save,
    /// Filing a new note.
    File,
}

/// How a save went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "SaveResult"))]
pub enum SaveOutcome {
    /// Written; the editor marks its document saved.
    Saved,
    /// Nothing to write (clean, a new note, a read-only file).
    Unchanged,
    /// The file moved on since it was read; the banner says so.
    Conflict,
    /// "Keep mine" is armed: the owner confirms, then the save overwrites.
    NeedsConfirm,
    /// The write failed; the banner says why.
    Error,
}

/// A change from outside, as the watcher reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The file's content changed.
    Modified,
    /// The file appeared.
    Created,
    /// The file is gone.
    Deleted,
}

/// What a session edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A file of a root; `light` over the file service's large-file mark.
    File { light: bool },
    /// A new note, not filed yet (W4).
    NewNote,
}

/// A silent reload the session asked the editor for.
#[derive(Debug, Default, Clone)]
struct Reload {
    /// Asked and not yet carried out or turned into a banner.
    due: bool,
    /// Counts every ask, so the editor acts once on each.
    seq: u64,
    /// The version the editor's fetch read; taken over when the editor says it reloaded.
    fetched: Option<Version>,
}

/// One open file.
#[derive(Debug)]
pub struct FileSession {
    root: String,
    rel: String,
    /// The version on disk the editor's text is based on; `None` once the file is gone (or for a new note).
    version: Option<Version>,
    /// A file of a root (and how large), or a new note with no file behind it.
    origin: Origin,
    banner: Option<Banner>,
    /// The kept-aside text the recovery banner offers.
    offered: Option<Kept>,
    /// Set by "keep mine": the next save must be confirmed, then overwrites.
    overwrite_armed: bool,
    /// Whether the editor's document has unsaved changes, as it last reported.
    dirty: bool,
    /// Whether the watcher took this session's subscription (to be given back once, never twice).
    watching: bool,
    reload: Reload,
}

impl FileSession {
    /// Opens `rel` under `root`: reads it, starts watching it, and looks for unsaved text kept from before. Returns the
    /// session and the text for the editor.
    ///
    /// # Errors
    ///
    /// Whatever reading refuses with; nothing is watched then.
    pub fn open<D: Disk>(disk: &D, root: &str, rel: &str) -> Result<(Self, DiskText), D::Error> {
        let file = disk.read(root, rel)?;
        let mut session = Self {
            root: root.to_string(),
            rel: rel.to_string(),
            version: Some(file.version.clone()),
            origin: Origin::File { light: file.large },
            banner: None,
            offered: None,
            overwrite_armed: false,
            dirty: false,
            watching: false,
            reload: Reload::default(),
        };
        if session.read_only() {
            return Ok((session, file));
        }
        session.watching = disk.watch(root, rel);
        match disk.kept(root, rel) {
            Some(kept) if kept.content != file.content => {
                session.banner = Some(Banner::Recovery {
                    saved_at: kept.saved_at.clone(),
                    changed_since: kept.base.as_ref() != Some(&file.version),
                });
                session.offered = Some(kept);
            }
            // The same text as the file: nothing to offer.
            Some(_) => disk.forget(root, rel),
            None => {}
        }
        Ok((session, file))
    }

    /// A new note: an empty document with no file behind it — offered last time's draft if one was kept. Nothing is
    /// read or watched.
    pub fn new_note<D: Disk>(disk: &D) -> Self {
        let mut session = Self {
            root: DRAFT_ROOT.to_string(),
            rel: DRAFT_REL.to_string(),
            version: None,
            origin: Origin::NewNote,
            banner: None,
            offered: None,
            overwrite_armed: false,
            dirty: false,
            watching: false,
            reload: Reload::default(),
        };
        if let Some(kept) = disk
            .kept(DRAFT_ROOT, DRAFT_REL)
            .filter(|k| !k.content.is_empty())
        {
            session.banner = Some(Banner::Recovery {
                saved_at: kept.saved_at.clone(),
                changed_since: false,
            });
            session.offered = Some(kept);
        }
        session
    }

    /// The root id.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The path inside the root; a rename in the file tree moves it ([`FileSession::moved`]).
    #[must_use]
    pub fn rel(&self) -> &str {
        &self.rel
    }

    /// The version the editor's text is based on.
    #[must_use]
    pub fn version(&self) -> Option<&Version> {
        self.version.as_ref()
    }

    /// Over the file service's large-file mark: edited in the light mode.
    #[must_use]
    pub fn light(&self) -> bool {
        matches!(self.origin, Origin::File { light: true })
    }

    /// A new note not filed yet (W4).
    #[must_use]
    pub fn untitled(&self) -> bool {
        self.origin == Origin::NewNote
    }

    /// A language server's file outside every root (L11): shown, never written.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.root.starts_with(FOREIGN_ROOT)
    }

    /// What the editor shows above the text.
    #[must_use]
    pub fn banner(&self) -> Option<&Banner> {
        self.banner.as_ref()
    }

    /// Whether the editor's document has unsaved changes, as it last reported.
    #[must_use]
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The silent reload the editor is to carry out, numbered so it acts once on each; `None` when none is due.
    #[must_use]
    pub fn reload_due(&self) -> Option<u64> {
        self.reload.due.then_some(self.reload.seq)
    }

    /// The editor's document switched between saved and unsaved. A reload still due becomes the banner: the editor
    /// has text of its own now. Returns whether the view changed (the flag itself is no part of it: the editor knows).
    pub fn set_dirty(&mut self, dirty: bool) -> bool {
        if self.dirty == dirty {
            return false;
        }
        self.dirty = dirty;
        if dirty && self.reload.due {
            self.cancel_due_reload();
            self.banner = Some(Banner::External {
                confirm_reload: false,
            });
            return true;
        }
        false
    }

    /// Saves `text` with the expected version. A conflict raises the external-change banner; after "keep mine" the
    /// first save only asks ([`SaveOutcome::NeedsConfirm`]), and one with `confirmed` then overwrites.
    pub fn save<D: Disk>(&mut self, disk: &D, text: &str, confirmed: bool) -> SaveOutcome {
        // A new note is filed, never written here — not even by autosave.
        if self.untitled() || self.read_only() {
            return SaveOutcome::Unchanged;
        }
        if !self.dirty && self.version.is_some() && !self.overwrite_armed {
            return SaveOutcome::Unchanged;
        }
        if self.overwrite_armed && !confirmed {
            self.banner = Some(Banner::ConfirmOverwrite);
            return SaveOutcome::NeedsConfirm;
        }
        let expected = if self.overwrite_armed {
            None
        } else {
            self.version.as_ref()
        };
        match disk.write(&self.root, &self.rel, text, expected) {
            Ok(version) => self.version = Some(version),
            Err(err) if D::is_conflict(&err) => {
                self.banner = Some(Banner::External {
                    confirm_reload: false,
                });
                return SaveOutcome::Conflict;
            }
            Err(err) => {
                self.banner = Some(Banner::Failed {
                    action: FailedAction::Save,
                    error: err.to_string(),
                });
                return SaveOutcome::Error;
            }
        }
        self.dirty = false;
        self.overwrite_armed = false;
        self.cancel_due_reload();
        if !matches!(self.banner, Some(Banner::Recovery { .. })) {
            self.banner = None;
        }
        disk.forget(&self.root, &self.rel);
        SaveOutcome::Saved
    }

    /// A change of this file from outside (the caller matched root and path). Returns whether anything visible
    /// changed: the echo of the session's own save (same version) is ignored, a deletion raises its banner, a clean
    /// document gets a silent reload due ([`FileSession::reload_due`]), unsaved edits the banner.
    pub fn on_change(&mut self, kind: ChangeKind, version: Option<&Version>) -> bool {
        if self.untitled() {
            return false;
        }
        if version.is_some() && version == self.version.as_ref() {
            return false;
        }
        if kind == ChangeKind::Deleted {
            self.version = None;
            self.cancel_due_reload();
            self.banner = Some(Banner::Deleted);
            return true;
        }
        if !self.dirty && !self.overwrite_armed {
            self.reload.due = true;
            self.reload.seq += 1;
            self.reload.fetched = None;
            return true;
        }
        self.banner = Some(Banner::External {
            confirm_reload: false,
        });
        true
    }

    /// Reads the file for the reload that is due, without taking it over yet: the editor puts the text in only while
    /// its document is still clean, then says so ([`FileSession::reloaded`]). `None` when no reload is due any more.
    ///
    /// # Errors
    ///
    /// Whatever reading refuses with.
    pub fn fetch_reload<D: Disk>(&mut self, disk: &D) -> Result<Option<DiskText>, D::Error> {
        if !self.reload.due {
            return Ok(None);
        }
        match disk.read(&self.root, &self.rel) {
            Ok(file) => {
                self.reload.fetched = Some(file.version.clone());
                Ok(Some(file))
            }
            Err(err) => {
                // The file changed into something the editor cannot take (too large, no text, unreadable): the owner
                // decides, as with unsaved edits; the next save is a conflict, never a silent overwrite.
                self.cancel_due_reload();
                self.banner = Some(Banner::External {
                    confirm_reload: false,
                });
                Err(err)
            }
        }
    }

    /// The editor put the text of reload `seq` in: the session is based on it now, with the "reloaded" hint. Returns
    /// whether it took (a newer change or a keystroke reported meanwhile leaves it out).
    pub fn reloaded(&mut self, seq: u64) -> bool {
        if !self.reload.due || self.reload.seq != seq {
            return false;
        }
        let Some(version) = self.reload.fetched.take() else {
            return false;
        };
        self.version = Some(version);
        self.reload.due = false;
        self.overwrite_armed = false;
        self.dirty = false;
        self.banner = Some(Banner::Reloaded);
        true
    }

    /// "Reload" over unsaved edits: the first call only asks; the second, or one on a clean document, reads the file
    /// and bases the session on it. `Some` is the text the editor puts in.
    ///
    /// # Errors
    ///
    /// Whatever reading refuses with.
    pub fn request_reload<D: Disk>(&mut self, disk: &D) -> Result<Option<DiskText>, D::Error> {
        if self.untitled() {
            return Ok(None);
        }
        let confirmed = matches!(
            self.banner,
            Some(Banner::External {
                confirm_reload: true
            })
        );
        if self.dirty && !confirmed {
            self.banner = Some(Banner::External {
                confirm_reload: true,
            });
            return Ok(None);
        }
        let file = self.take_disk(disk)?;
        self.banner = None;
        Ok(Some(file))
    }

    /// Backs out of the reload confirmation to the plain external-change banner.
    pub fn cancel_reload(&mut self) {
        if matches!(self.banner, Some(Banner::External { .. })) {
            self.banner = Some(Banner::External {
                confirm_reload: false,
            });
        }
    }

    /// "Keep mine": the banner goes; the next save asks before overwriting.
    pub fn keep_mine(&mut self) {
        self.overwrite_armed = true;
        self.banner = None;
    }

    /// What is on disk now, for the comparison beside the editor's text; empty for a new note.
    ///
    /// # Errors
    ///
    /// Whatever reading refuses with.
    pub fn disk_text<D: Disk>(&self, disk: &D) -> Result<String, D::Error> {
        if self.untitled() {
            return Ok(String::new());
        }
        Ok(disk.read(&self.root, &self.rel)?.content)
    }

    /// The kept text the recovery banner offers, for the editor to put back (as one undoable step). Based on an older
    /// version, the next save must not silently win over the file: it is armed.
    pub fn restore(&mut self) -> Option<String> {
        let Some(Banner::Recovery { changed_since, .. }) = self.banner else {
            return None;
        };
        let kept = self.offered.take()?;
        if changed_since {
            self.overwrite_armed = true;
        }
        self.banner = None;
        Some(kept.content)
    }

    /// Forgets the kept text the recovery banner offers.
    pub fn discard_kept<D: Disk>(&mut self, disk: &D) {
        disk.forget(&self.root, &self.rel);
        self.offered = None;
        if matches!(self.banner, Some(Banner::Recovery { .. })) {
            self.banner = None;
        }
    }

    /// Keeps the editor's unsaved `text` aside (F8) — or forgets what was kept, once the document is clean.
    pub fn keep_aside<D: Disk>(&self, disk: &D, text: &str) {
        if self.read_only() {
            return;
        }
        if self.dirty {
            disk.keep(&self.root, &self.rel, self.version.as_ref(), text);
        } else {
            disk.forget(&self.root, &self.rel);
        }
    }

    /// Throws the unsaved edits away for good (the editor asked first): a new note empties, a file is read again.
    /// `Some` is the text the editor puts in; `None` for a new note, which the editor empties.
    ///
    /// # Errors
    ///
    /// Whatever reading refuses with; nothing is forgotten then.
    pub fn discard_changes<D: Disk>(&mut self, disk: &D) -> Result<Option<DiskText>, D::Error> {
        let file = if self.untitled() {
            self.dirty = false;
            None
        } else {
            Some(self.take_disk(disk)?)
        };
        disk.forget(&self.root, &self.rel);
        self.offered = None;
        self.banner = None;
        Ok(file)
    }

    /// Drops the banner — only the "reloaded" hint when `only_hint` (its timer ran out, but a question may have
    /// replaced it meanwhile).
    pub fn dismiss(&mut self, only_hint: bool) {
        if !only_hint || self.banner == Some(Banner::Reloaded) {
            self.banner = None;
        }
    }

    /// The file was renamed or moved inside its root by the file tree (W13): the session follows it, watching the new
    /// path and keeping its version, instead of taking the old path's disappearance for a deletion. The kept-aside
    /// text moves along. Returns whether it moved.
    pub fn moved<D: Disk>(&mut self, disk: &D, rel: &str) -> bool {
        if self.untitled() || self.read_only() || rel == self.rel {
            return false;
        }
        let old = std::mem::replace(&mut self.rel, rel.to_string());
        // The watcher may already have taken the old path's disappearance for a deletion.
        if self.banner == Some(Banner::Deleted) {
            self.banner = None;
        }
        if self.watching {
            disk.unwatch(&self.root, &old);
        }
        let kept = disk.kept(&self.root, &old);
        disk.forget(&self.root, &old);
        if let Some(kept) = kept.filter(|_| self.dirty) {
            disk.keep(&self.root, rel, kept.base.as_ref(), &kept.content);
        }
        self.watching = disk.watch(&self.root, rel);
        // A deletion the watcher reported first took the version: without it the next save would write with no
        // expected version and win over any later change. The moved file is the same file: its version now is the one
        // the text is based on (a write squeezed in between the rename and this is the one change taken unseen).
        if self.version.is_none()
            && let Ok(file) = disk.read(&self.root, rel)
        {
            self.version = Some(file.version);
        }
        true
    }

    /// A new note became a file: the draft is forgotten.
    pub fn filed<D: Disk>(&mut self, disk: &D) {
        if !self.untitled() {
            return;
        }
        self.dirty = false;
        self.offered = None;
        self.banner = None;
        disk.forget(DRAFT_ROOT, DRAFT_REL);
    }

    /// Filing a new note failed: the banner says why.
    pub fn filing_failed(&mut self, error: String) {
        self.banner = Some(Banner::Failed {
            action: FailedAction::File,
            error,
        });
    }

    /// Stops watching; unsaved text stays kept aside.
    pub fn close<D: Disk>(self, disk: &D) {
        if self.watching {
            disk.unwatch(&self.root, &self.rel);
        }
    }

    /// Reads the file and bases the session on it, clean.
    fn take_disk<D: Disk>(&mut self, disk: &D) -> Result<DiskText, D::Error> {
        let file = disk.read(&self.root, &self.rel)?;
        self.version = Some(file.version.clone());
        self.overwrite_armed = false;
        self.dirty = false;
        self.cancel_due_reload();
        Ok(file)
    }

    fn cancel_due_reload(&mut self) {
        self.reload.due = false;
        self.reload.fetched = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt;

    use super::*;

    /// A failed fake read or write.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum FakeError {
        NotFound,
        Conflict,
        Broken,
    }

    impl fmt::Display for FakeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{self:?}")
        }
    }

    /// An in-memory disk with the file service's version and conflict rules.
    #[derive(Default)]
    pub(crate) struct FakeDisk {
        pub(crate) files: RefCell<BTreeMap<String, String>>,
        pub(crate) kept: RefCell<BTreeMap<String, Kept>>,
        pub(crate) watched: RefCell<BTreeSet<String>>,
        pub(crate) writes: Cell<usize>,
        /// Every `unwatch`, even one of nothing watched.
        pub(crate) unwatch_calls: Cell<usize>,
        pub(crate) broken: Cell<bool>,
        /// The watcher refuses every file.
        pub(crate) unwatchable: Cell<bool>,
    }

    pub(crate) fn key(root: &str, rel: &str) -> String {
        format!("{root}:{rel}")
    }

    pub(crate) fn version_of(content: &str) -> Version {
        Version::from(format!("{}-{content}", content.len()))
    }

    impl FakeDisk {
        pub(crate) fn with(rel: &str, content: &str) -> Self {
            let disk = Self::default();
            disk.put(rel, content);
            disk
        }

        pub(crate) fn put(&self, rel: &str, content: &str) {
            self.files
                .borrow_mut()
                .insert(key("workspace", rel), content.to_string());
        }

        pub(crate) fn file(&self, rel: &str) -> Option<String> {
            self.files.borrow().get(&key("workspace", rel)).cloned()
        }

        /// Someone else writes (or, with `None`, deletes) the file; what the watcher would report.
        pub(crate) fn external(
            &self,
            rel: &str,
            content: Option<&str>,
        ) -> (ChangeKind, Option<Version>) {
            let k = key("workspace", rel);
            let existed = self.files.borrow().contains_key(&k);
            match content {
                None => {
                    self.files.borrow_mut().remove(&k);
                    (ChangeKind::Deleted, None)
                }
                Some(text) => {
                    self.files.borrow_mut().insert(k, text.to_string());
                    let kind = if existed {
                        ChangeKind::Modified
                    } else {
                        ChangeKind::Created
                    };
                    (kind, Some(version_of(text)))
                }
            }
        }

        pub(crate) fn kept_text(&self, root: &str, rel: &str) -> Option<String> {
            self.kept
                .borrow()
                .get(&key(root, rel))
                .map(|k| k.content.clone())
        }
    }

    impl Disk for FakeDisk {
        type Error = FakeError;

        fn is_conflict(error: &FakeError) -> bool {
            *error == FakeError::Conflict
        }

        fn read(&self, root: &str, rel: &str) -> Result<DiskText, FakeError> {
            let content = self
                .files
                .borrow()
                .get(&key(root, rel))
                .cloned()
                .ok_or(FakeError::NotFound)?;
            Ok(DiskText {
                version: version_of(&content),
                large: content.len() > 1000,
                content,
            })
        }

        fn write(
            &self,
            root: &str,
            rel: &str,
            content: &str,
            expected: Option<&Version>,
        ) -> Result<Version, FakeError> {
            if self.broken.get() {
                return Err(FakeError::Broken);
            }
            let k = key(root, rel);
            let current = self.files.borrow().get(&k).cloned();
            if let Some(expected) = expected
                && current.as_deref().map(version_of).as_ref() != Some(expected)
            {
                return Err(FakeError::Conflict);
            }
            self.writes.set(self.writes.get() + 1);
            self.files.borrow_mut().insert(k, content.to_string());
            Ok(version_of(content))
        }

        fn watch(&self, root: &str, rel: &str) -> bool {
            if self.unwatchable.get() {
                return false;
            }
            self.watched.borrow_mut().insert(key(root, rel));
            true
        }

        fn unwatch(&self, root: &str, rel: &str) {
            self.unwatch_calls.set(self.unwatch_calls.get() + 1);
            self.watched.borrow_mut().remove(&key(root, rel));
        }

        fn keep(&self, root: &str, rel: &str, base: Option<&Version>, content: &str) {
            self.kept.borrow_mut().insert(
                key(root, rel),
                Kept {
                    base: base.cloned(),
                    content: content.to_string(),
                    saved_at: "2026-11-23T10:00:00Z".into(),
                },
            );
        }

        fn kept(&self, root: &str, rel: &str) -> Option<Kept> {
            self.kept.borrow().get(&key(root, rel)).cloned()
        }

        fn forget(&self, root: &str, rel: &str) {
            self.kept.borrow_mut().remove(&key(root, rel));
        }
    }

    fn opened(content: &str) -> (FakeDisk, FileSession) {
        let disk = FakeDisk::with("a.md", content);
        let (session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
        (disk, session)
    }

    /// The editor typed: its document is unsaved now and holds `text`.
    fn typed(session: &mut FileSession) {
        session.set_dirty(true);
    }

    /// The editor carries out a due silent reload, its document still clean; the text it put in.
    fn carry_out_reload(disk: &FakeDisk, session: &mut FileSession) -> Option<String> {
        let seq = session.reload_due()?;
        let file = session.fetch_reload(disk).unwrap()?;
        assert!(session.reloaded(seq));
        Some(file.content)
    }

    mod opening_and_saving {
        use super::*;

        #[test]
        fn opens_watches_and_saves_with_the_expected_version() {
            let (disk, mut session) = opened("hello\n");
            assert!(disk.watched.borrow().contains("workspace:a.md"));
            assert_eq!(
                session.save(&disk, "hello\n", false),
                SaveOutcome::Unchanged
            );
            typed(&mut session);
            assert_eq!(session.save(&disk, "hello!\n", false), SaveOutcome::Saved);
            assert_eq!(disk.file("a.md").as_deref(), Some("hello!\n"));
            assert!(!session.dirty());
        }

        #[test]
        fn refuses_to_overwrite_an_external_change_and_raises_the_banner() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            disk.external("a.md", Some("agent wrote this\n"));
            assert_eq!(
                session.save(&disk, "hello mine\n", false),
                SaveOutcome::Conflict
            );
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: false
                })
            );
            assert_eq!(disk.file("a.md").as_deref(), Some("agent wrote this\n"));
        }

        #[test]
        fn says_why_a_write_failed() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            disk.broken.set(true);
            assert_eq!(session.save(&disk, "x", false), SaveOutcome::Error);
            assert_eq!(
                session.banner(),
                Some(&Banner::Failed {
                    action: FailedAction::Save,
                    error: "Broken".into()
                })
            );
            assert!(session.dirty());
        }

        #[test]
        fn saves_a_large_file_like_any_other_in_the_light_mode() {
            let (disk, mut session) = opened(&"x".repeat(2000));
            assert!(session.light());
            typed(&mut session);
            assert_eq!(session.save(&disk, "y", false), SaveOutcome::Saved);
        }

        #[test]
        fn never_writes_watches_or_keeps_aside_a_language_servers_file_outside_every_root() {
            let disk = FakeDisk::default();
            disk.files.borrow_mut().insert(
                key("lsp:3", "/Users/me/.rustup/lib.rs"),
                "pub fn std() {}\n".into(),
            );
            let (mut session, _) =
                FileSession::open(&disk, "lsp:3", "/Users/me/.rustup/lib.rs").unwrap();
            assert!(session.read_only());
            typed(&mut session);
            assert_eq!(session.save(&disk, "x", false), SaveOutcome::Unchanged);
            session.keep_aside(&disk, "x");
            assert_eq!(disk.writes.get(), 0);
            assert!(disk.kept.borrow().is_empty());
            assert!(disk.watched.borrow().is_empty());
            session.close(&disk);
            assert_eq!(disk.unwatch_calls.get(), 0);
            assert!(!opened("a").1.read_only());
        }

        #[test]
        fn a_failed_open_watches_nothing() {
            let disk = FakeDisk::default();
            assert_eq!(
                FileSession::open(&disk, "workspace", "gone.md").unwrap_err(),
                FakeError::NotFound
            );
            assert!(disk.watched.borrow().is_empty());
        }

        #[test]
        fn stops_watching_when_closed() {
            let (disk, session) = opened("hello\n");
            session.close(&disk);
            assert!(disk.watched.borrow().is_empty());
            assert_eq!(disk.unwatch_calls.get(), 1);
        }

        #[test]
        fn gives_back_only_a_watch_that_took() {
            let disk = FakeDisk::with("a.md", "hello\n");
            disk.unwatchable.set(true);
            let (mut session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            disk.unwatchable.set(false);
            // The new path's watch takes; the old one, never taken, is not given back.
            disk.put("b.md", "hello\n");
            session.moved(&disk, "b.md");
            assert_eq!(disk.unwatch_calls.get(), 0);
            session.close(&disk);
            assert_eq!(disk.unwatch_calls.get(), 1);
        }
    }

    mod external_changes {
        use super::*;

        #[test]
        fn ignores_the_echo_of_its_own_save() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            session.save(&disk, "hello!\n", false);
            assert!(!session.on_change(ChangeKind::Modified, Some(&version_of("hello!\n"))));
            assert_eq!(session.banner(), None);
            assert_eq!(session.reload_due(), None);
            assert_eq!(disk.writes.get(), 1);
        }

        #[test]
        fn reloads_a_clean_document_with_a_hint_through_the_editor() {
            let (disk, mut session) = opened("hello\n");
            let (kind, version) = disk.external("a.md", Some("new text\n"));
            assert!(session.on_change(kind, version.as_ref()));
            assert_eq!(
                carry_out_reload(&disk, &mut session).as_deref(),
                Some("new text\n")
            );
            assert_eq!(session.banner(), Some(&Banner::Reloaded));
            assert_eq!(session.version(), Some(&version_of("new text\n")));
            assert_eq!(session.reload_due(), None);
        }

        #[test]
        fn a_file_that_can_no_longer_be_read_raises_the_banner_instead_of_a_reload() {
            let (disk, mut session) = opened("hello\n");
            let (kind, version) = disk.external("a.md", Some("theirs\n"));
            session.on_change(kind, version.as_ref());
            disk.external("a.md", None);
            assert_eq!(
                session.fetch_reload(&disk).unwrap_err(),
                FakeError::NotFound
            );
            assert_eq!(session.reload_due(), None);
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: false
                })
            );
        }

        #[test]
        fn switching_dirty_changes_the_view_only_when_a_due_reload_becomes_the_banner() {
            let (disk, mut session) = opened("hello\n");
            assert!(!session.set_dirty(true));
            assert!(!session.set_dirty(false));
            let (kind, version) = disk.external("a.md", Some("theirs\n"));
            session.on_change(kind, version.as_ref());
            assert!(session.set_dirty(true));
        }

        #[test]
        fn a_keystroke_reported_before_the_reload_turns_it_into_the_banner() {
            let (disk, mut session) = opened("hello\n");
            let (kind, version) = disk.external("a.md", Some("theirs\n"));
            session.on_change(kind, version.as_ref());
            let seq = session.reload_due().unwrap();
            // The editor fetched, but found its document typed into meanwhile.
            session.fetch_reload(&disk).unwrap();
            typed(&mut session);
            assert!(!session.reloaded(seq));
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: false
                })
            );
            assert_eq!(session.version(), Some(&version_of("hello\n")));
            // Saving now is a conflict, not a silent overwrite.
            assert_eq!(
                session.save(&disk, "hello mine\n", false),
                SaveOutcome::Conflict
            );
        }

        #[test]
        fn an_older_reload_is_not_taken_once_a_newer_change_came_in() {
            let (disk, mut session) = opened("hello\n");
            let (kind, version) = disk.external("a.md", Some("one\n"));
            session.on_change(kind, version.as_ref());
            let first = session.reload_due().unwrap();
            session.fetch_reload(&disk).unwrap();
            let (kind, version) = disk.external("a.md", Some("two\n"));
            session.on_change(kind, version.as_ref());
            assert!(!session.reloaded(first));
            assert_eq!(
                carry_out_reload(&disk, &mut session).as_deref(),
                Some("two\n")
            );
        }

        #[test]
        fn never_touches_unsaved_edits_and_reload_takes_a_second_click() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            let (kind, version) = disk.external("a.md", Some("theirs\n"));
            session.on_change(kind, version.as_ref());
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: false
                })
            );
            assert_eq!(session.reload_due(), None);

            assert_eq!(session.request_reload(&disk).unwrap(), None);
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: true
                })
            );
            session.cancel_reload();
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: false
                })
            );
            session.request_reload(&disk).unwrap();
            let file = session.request_reload(&disk).unwrap().unwrap();
            assert_eq!(file.content, "theirs\n");
            assert_eq!(session.banner(), None);
            assert!(!session.dirty());
        }

        #[test]
        fn keep_mine_makes_the_next_save_ask_then_overwrite() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            let (kind, version) = disk.external("a.md", Some("theirs\n"));
            session.on_change(kind, version.as_ref());
            session.keep_mine();
            assert_eq!(
                session.save(&disk, "hello mine\n", false),
                SaveOutcome::NeedsConfirm
            );
            assert_eq!(session.banner(), Some(&Banner::ConfirmOverwrite));
            assert_eq!(
                session.save(&disk, "hello mine\n", true),
                SaveOutcome::Saved
            );
            assert_eq!(disk.file("a.md").as_deref(), Some("hello mine\n"));
        }

        #[test]
        fn reports_a_deletion_and_recreates_the_file_on_save() {
            let (disk, mut session) = opened("hello\n");
            let (kind, version) = disk.external("a.md", None);
            assert!(session.on_change(kind, version.as_ref()));
            assert_eq!(session.banner(), Some(&Banner::Deleted));
            assert_eq!(session.save(&disk, "hello\n", false), SaveOutcome::Saved);
            assert_eq!(disk.file("a.md").as_deref(), Some("hello\n"));
        }

        #[test]
        fn the_reloaded_hint_goes_by_its_timer_but_a_question_stays() {
            let (disk, mut session) = opened("hello\n");
            let (kind, version) = disk.external("a.md", Some("new\n"));
            session.on_change(kind, version.as_ref());
            carry_out_reload(&disk, &mut session);
            session.dismiss(true);
            assert_eq!(session.banner(), None);
            typed(&mut session);
            let (kind, version) = disk.external("a.md", Some("newer\n"));
            session.on_change(kind, version.as_ref());
            session.dismiss(true);
            assert_eq!(
                session.banner(),
                Some(&Banner::External {
                    confirm_reload: false
                })
            );
            session.dismiss(false);
            assert_eq!(session.banner(), None);
        }
    }

    mod recovery {
        use super::*;

        #[test]
        fn keeps_unsaved_text_aside_and_drops_it_on_save() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            session.keep_aside(&disk, "hello draft\n");
            assert_eq!(
                disk.kept_text("workspace", "a.md").as_deref(),
                Some("hello draft\n")
            );
            session.save(&disk, "hello draft\n", false);
            assert!(disk.kept.borrow().is_empty());
        }

        #[test]
        fn keeps_a_draft_kept_aside_when_the_session_is_then_closed() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            session.keep_aside(&disk, "hello draft\n");
            session.close(&disk);
            assert_eq!(
                disk.kept_text("workspace", "a.md").as_deref(),
                Some("hello draft\n")
            );
            assert_eq!(disk.unwatch_calls.get(), 1);
        }

        #[test]
        fn keeping_aside_a_clean_document_forgets_what_was_kept() {
            let (disk, session) = opened("hello\n");
            disk.keep("workspace", "a.md", None, "stale");
            session.keep_aside(&disk, "hello\n");
            assert!(disk.kept.borrow().is_empty());
        }

        #[test]
        fn offers_kept_text_back_on_the_next_open() {
            let disk = FakeDisk::with("a.md", "hello\n");
            disk.keep(
                "workspace",
                "a.md",
                Some(&version_of("hello\n")),
                "hello draft\n",
            );
            let (mut session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            assert_eq!(
                session.banner(),
                Some(&Banner::Recovery {
                    saved_at: "2026-11-23T10:00:00Z".into(),
                    changed_since: false
                })
            );
            assert_eq!(session.restore().as_deref(), Some("hello draft\n"));
            assert_eq!(session.banner(), None);
            assert_eq!(session.restore(), None);
        }

        #[test]
        fn says_when_the_file_changed_since_and_asks_before_the_next_save() {
            let disk = FakeDisk::with("a.md", "changed meanwhile\n");
            disk.keep(
                "workspace",
                "a.md",
                Some(&version_of("hello\n")),
                "hello draft\n",
            );
            let (mut session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            assert!(matches!(
                session.banner(),
                Some(Banner::Recovery {
                    changed_since: true,
                    ..
                })
            ));
            session.restore();
            typed(&mut session);
            assert_eq!(
                session.save(&disk, "hello draft\n", false),
                SaveOutcome::NeedsConfirm
            );
        }

        #[test]
        fn forgets_a_kept_copy_that_equals_the_file_and_discards_on_request() {
            let disk = FakeDisk::with("a.md", "same\n");
            disk.keep("workspace", "a.md", None, "same\n");
            let (session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            assert_eq!(session.banner(), None);
            assert!(disk.kept.borrow().is_empty());

            disk.keep("workspace", "a.md", None, "other\n");
            let (mut again, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            again.discard_kept(&disk);
            assert!(disk.kept.borrow().is_empty());
            assert_eq!(again.banner(), None);
        }

        #[test]
        fn discarding_changes_reads_the_file_again_and_forgets_the_kept_copy() {
            let (disk, mut session) = opened("hello\n");
            typed(&mut session);
            session.keep_aside(&disk, "mine\n");
            let file = session.discard_changes(&disk).unwrap().unwrap();
            assert_eq!(file.content, "hello\n");
            assert!(!session.dirty());
            assert!(disk.kept.borrow().is_empty());
        }
    }

    mod a_new_note {
        use super::*;

        #[test]
        fn starts_empty_is_never_written_or_watched_and_keeps_its_draft_under_its_own_key() {
            let disk = FakeDisk::default();
            let mut session = FileSession::new_note(&disk);
            assert!(session.untitled());
            assert!(disk.watched.borrow().is_empty());
            typed(&mut session);
            assert_eq!(session.save(&disk, "# Idea", false), SaveOutcome::Unchanged);
            assert_eq!(disk.writes.get(), 0);
            session.keep_aside(&disk, "# Idea");
            assert_eq!(
                disk.kept_text(DRAFT_ROOT, DRAFT_REL).as_deref(),
                Some("# Idea")
            );
            session.close(&disk);
            assert_eq!(disk.unwatch_calls.get(), 0);
        }

        #[test]
        fn offers_last_times_draft_back() {
            let disk = FakeDisk::default();
            disk.keep(DRAFT_ROOT, DRAFT_REL, None, "draft text");
            let mut session = FileSession::new_note(&disk);
            assert!(matches!(
                session.banner(),
                Some(Banner::Recovery {
                    changed_since: false,
                    ..
                })
            ));
            assert_eq!(session.restore().as_deref(), Some("draft text"));
        }

        #[test]
        fn empties_the_draft_on_discard_instead_of_reading_a_file_that_is_not_there() {
            let disk = FakeDisk::default();
            let mut session = FileSession::new_note(&disk);
            typed(&mut session);
            session.keep_aside(&disk, "gone");
            assert_eq!(session.discard_changes(&disk).unwrap(), None);
            assert!(!session.dirty());
            assert!(disk.kept.borrow().is_empty());
            assert_eq!(session.disk_text(&disk).unwrap(), "");
        }

        #[test]
        fn keeping_aside_a_clean_draft_deletes_one_offered_but_never_restored() {
            let disk = FakeDisk::default();
            disk.keep(DRAFT_ROOT, DRAFT_REL, None, "leftover draft");
            let session = FileSession::new_note(&disk);
            session.keep_aside(&disk, "");
            assert!(disk.kept.borrow().is_empty());
        }

        #[test]
        fn never_reacts_to_a_change_of_anything() {
            let disk = FakeDisk::default();
            let mut session = FileSession::new_note(&disk);
            assert!(!session.on_change(ChangeKind::Modified, Some(&Version::from("v1"))));
            assert_eq!(session.banner(), None);
        }

        #[test]
        fn filing_forgets_the_draft_and_a_failure_says_why() {
            let disk = FakeDisk::default();
            let mut session = FileSession::new_note(&disk);
            typed(&mut session);
            session.keep_aside(&disk, "# Idea");
            session.filing_failed("no area".into());
            assert_eq!(
                session.banner(),
                Some(&Banner::Failed {
                    action: FailedAction::File,
                    error: "no area".into()
                })
            );
            session.filed(&disk);
            assert!(!session.dirty());
            assert!(disk.kept.borrow().is_empty());
            assert_eq!(session.banner(), None);
        }
    }

    mod following_a_rename {
        use super::*;

        #[test]
        fn watches_the_new_path_keeps_the_version_and_moves_the_kept_copy() {
            let disk = FakeDisk::with("old.md", "hello");
            let (mut session, _) = FileSession::open(&disk, "workspace", "old.md").unwrap();
            typed(&mut session);
            session.keep_aside(&disk, "xhello");
            let version = session.version().cloned();
            assert!(session.moved(&disk, "new/name.md"));
            assert_eq!(session.rel(), "new/name.md");
            assert_eq!(session.version().cloned(), version);
            assert!(!disk.watched.borrow().contains("workspace:old.md"));
            assert!(disk.watched.borrow().contains("workspace:new/name.md"));
            assert_eq!(disk.kept_text("workspace", "old.md"), None);
            assert_eq!(
                disk.kept_text("workspace", "new/name.md").as_deref(),
                Some("xhello")
            );
        }

        #[test]
        fn leaves_no_stale_kept_copy_behind_when_a_clean_document_is_moved() {
            let disk = FakeDisk::with("clean.md", "hello");
            disk.keep("workspace", "clean.md", None, "offered, never restored");
            let (mut session, _) = FileSession::open(&disk, "workspace", "clean.md").unwrap();
            assert!(session.moved(&disk, "moved.md"));
            assert!(disk.watched.borrow().contains("workspace:moved.md"));
            assert!(disk.kept.borrow().is_empty());
        }

        #[test]
        fn takes_back_a_deletion_the_watcher_reported_first() {
            let disk = FakeDisk::with("a.md", "hello");
            let (mut session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            // The rename on disk, then the watcher's report of the old path first.
            disk.external("a.md", None);
            disk.put("b.md", "hello");
            session.on_change(ChangeKind::Deleted, None);
            session.moved(&disk, "b.md");
            assert_eq!(session.banner(), None);
            assert_eq!(session.version(), Some(&version_of("hello")));
            // Clean, so nothing is written; and a change made later is a conflict, never overwritten.
            assert_eq!(session.save(&disk, "hello", false), SaveOutcome::Unchanged);
            disk.put("b.md", "agent");
            typed(&mut session);
            assert_eq!(session.save(&disk, "mine", false), SaveOutcome::Conflict);
        }

        #[test]
        fn does_nothing_for_a_new_note_or_the_same_path() {
            let disk = FakeDisk::with("a.md", "a");
            let mut draft = FileSession::new_note(&disk);
            assert!(!draft.moved(&disk, "x.md"));
            assert_eq!(draft.rel(), DRAFT_REL);
            let (mut session, _) = FileSession::open(&disk, "workspace", "a.md").unwrap();
            assert!(!session.moved(&disk, "a.md"));
        }
    }
}
