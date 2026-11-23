//! Every open file of an app, by id: one session per editor (two editors on the same file each have their own, as
//! each has its own text and undo), the changes the watcher and the file tree report sent to the sessions they
//! concern, and the files with unsaved changes somewhere.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::disk::{Disk, DiskText, Version};
use crate::memory::FileRef;
use crate::paths::{is_under, renamed_path};
use crate::session::{Banner, ChangeKind, FileSession};

/// A session's id, handed to the editor at open; a hand-over from one editor to another passes it along.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize,
)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileSessionId(pub u64);

/// What the editor shows of its session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileSessionView {
    /// The session.
    pub id: FileSessionId,
    /// Grows with every view of any session: the editor keeps the newest it got.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    /// The root id.
    pub root: String,
    /// The path inside the root — a rename in the file tree changes it.
    pub rel: String,
    /// Edited in the light mode.
    pub light: bool,
    /// A new note, not filed yet.
    pub untitled: bool,
    /// A language server's file outside every root.
    pub read_only: bool,
    /// What shows above the text.
    pub banner: Option<Banner>,
    /// A silent reload to carry out while the document is still clean, numbered so the editor acts once on each.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub reload: Option<u64>,
}

/// The open sessions.
#[derive(Debug, Default)]
pub struct FileSessions {
    sessions: BTreeMap<FileSessionId, FileSession>,
    next: u64,
    revision: u64,
}

impl FileSessions {
    /// No session open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many sessions are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no session is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Forgets every session without closing it (the editors that held them, and their watches, are gone). Ids and
    /// revisions keep counting: a late call of a gone editor must never reach a session opened afterwards.
    pub fn clear(&mut self) {
        self.sessions.clear();
    }

    /// Opens `rel` under `root` as a new session ([`FileSession::open`]).
    ///
    /// # Errors
    ///
    /// Whatever reading refuses with; no session is made.
    pub fn open<D: Disk>(
        &mut self,
        disk: &D,
        root: &str,
        rel: &str,
    ) -> Result<(FileSessionId, DiskText), D::Error> {
        let (session, file) = FileSession::open(disk, root, rel)?;
        Ok((self.add(session), file))
    }

    /// A new note as a new session ([`FileSession::new_note`]).
    pub fn new_note<D: Disk>(&mut self, disk: &D) -> FileSessionId {
        let session = FileSession::new_note(disk);
        self.add(session)
    }

    /// The session `id`, if open.
    pub fn get_mut(&mut self, id: FileSessionId) -> Option<&mut FileSession> {
        self.sessions.get_mut(&id)
    }

    /// The session `id` as its editor shows it, with a revision newer than every view before.
    pub fn view(&mut self, id: FileSessionId) -> Option<FileSessionView> {
        let session = self.sessions.get(&id)?;
        self.revision += 1;
        Some(FileSessionView {
            id,
            revision: self.revision,
            root: session.root().to_string(),
            rel: session.rel().to_string(),
            light: session.light(),
            untitled: session.untitled(),
            read_only: session.read_only(),
            banner: session.banner().cloned(),
            reload: session.reload_due(),
        })
    }

    /// Closes the session `id` ([`FileSession::close`]); whether it was open.
    pub fn close<D: Disk>(&mut self, disk: &D, id: FileSessionId) -> bool {
        self.sessions
            .remove(&id)
            .map(|session| session.close(disk))
            .is_some()
    }

    /// The watcher reported a change of `rel` under `root`: every session of that file judges it. The ids of those
    /// whose view changed.
    pub fn changed(
        &mut self,
        root: &str,
        rel: &str,
        kind: ChangeKind,
        version: Option<&Version>,
    ) -> Vec<FileSessionId> {
        self.touch(
            |s| s.root() == root && s.rel() == rel,
            |s| s.on_change(kind, version),
        )
    }

    /// The file tree renamed `from` to `to` under `root`: the sessions of that file, or of files under that folder,
    /// follow ([`FileSession::moved`]).
    pub fn renamed<D: Disk>(
        &mut self,
        disk: &D,
        root: &str,
        from: &str,
        to: &str,
    ) -> Vec<FileSessionId> {
        let mut moved = Vec::new();
        for (id, session) in &mut self.sessions {
            if session.root() != root {
                continue;
            }
            if let Some(rel) = renamed_path(session.rel(), from, to)
                && session.moved(disk, &rel)
            {
                moved.push(*id);
            }
        }
        moved
    }

    /// The file tree deleted `rel` under `root` (a file or a folder): every session at or under it learns at once,
    /// without waiting for the watcher.
    pub fn removed(&mut self, root: &str, rel: &str) -> Vec<FileSessionId> {
        self.touch(
            |s| s.root() == root && is_under(s.rel(), rel),
            |s| s.on_change(ChangeKind::Deleted, None),
        )
    }

    /// Every file with unsaved changes in some session (a new note is no file), each once, in order.
    #[must_use]
    pub fn unsaved(&self) -> Vec<FileRef> {
        let mut files: Vec<FileRef> = self
            .sessions
            .values()
            .filter(|s| s.dirty() && !s.untitled())
            .map(|s| FileRef {
                root: s.root().to_string(),
                rel: s.rel().to_string(),
            })
            .collect();
        files.sort_by(|a, b| (&a.root, &a.rel).cmp(&(&b.root, &b.rel)));
        files.dedup();
        files
    }

    fn add(&mut self, session: FileSession) -> FileSessionId {
        self.next += 1;
        let id = FileSessionId(self.next);
        self.sessions.insert(id, session);
        id
    }

    fn touch(
        &mut self,
        concerns: impl Fn(&FileSession) -> bool,
        mut change: impl FnMut(&mut FileSession) -> bool,
    ) -> Vec<FileSessionId> {
        self.sessions
            .iter_mut()
            .filter(|(_, s)| concerns(s))
            .filter_map(|(id, s)| change(s).then_some(*id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{FakeDisk, version_of};
    use crate::{DRAFT_REL, Disk};

    #[test]
    fn hands_out_a_new_id_per_open_even_for_the_same_file() {
        let disk = FakeDisk::with("a.md", "hello");
        let mut sessions = FileSessions::new();
        let (a, file) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let (b, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        assert_ne!(a, b);
        assert_eq!(file.content, "hello");
        assert!(sessions.open(&disk, "workspace", "gone.md").is_err());
    }

    #[test]
    fn every_view_is_newer_than_the_one_before() {
        let disk = FakeDisk::with("a.md", "hello");
        let mut sessions = FileSessions::new();
        let (id, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let first = sessions.view(id).unwrap();
        let second = sessions.view(id).unwrap();
        assert!(second.revision > first.revision);
        assert_eq!(first.rel, "a.md");
        assert!(!first.read_only);
    }

    #[test]
    fn a_change_reaches_every_session_of_its_file_and_no_other() {
        let disk = FakeDisk::with("a.md", "hello");
        disk.put("b.md", "b");
        let mut sessions = FileSessions::new();
        let (a1, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let (a2, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let (_b, _) = sessions.open(&disk, "workspace", "b.md").unwrap();
        let (kind, version) = disk.external("a.md", Some("new"));
        assert_eq!(
            sessions.changed("workspace", "a.md", kind, version.as_ref()),
            [a1, a2]
        );
        assert_eq!(sessions.view(a1).unwrap().reload, Some(1));
    }

    #[test]
    fn a_rename_moves_the_sessions_of_the_file_and_of_files_in_the_folder() {
        let disk = FakeDisk::with("docs/a.md", "a");
        disk.put("docs/b.md", "b");
        disk.put("other.md", "o");
        let mut sessions = FileSessions::new();
        let (a, _) = sessions.open(&disk, "workspace", "docs/a.md").unwrap();
        let (b, _) = sessions.open(&disk, "workspace", "docs/b.md").unwrap();
        let (_o, _) = sessions.open(&disk, "workspace", "other.md").unwrap();
        assert_eq!(
            sessions.renamed(&disk, "workspace", "docs", "notes"),
            [a, b]
        );
        assert_eq!(sessions.view(b).unwrap().rel, "notes/b.md");
        assert_eq!(sessions.renamed(&disk, "project:1", "other.md", "x.md"), []);
    }

    #[test]
    fn a_deleted_folder_reaches_the_sessions_under_it() {
        let disk = FakeDisk::with("docs/a.md", "a");
        disk.put("docsx.md", "x");
        let mut sessions = FileSessions::new();
        let (a, _) = sessions.open(&disk, "workspace", "docs/a.md").unwrap();
        let (_x, _) = sessions.open(&disk, "workspace", "docsx.md").unwrap();
        assert_eq!(sessions.removed("workspace", "docs"), [a]);
        assert_eq!(sessions.view(a).unwrap().banner, Some(Banner::Deleted));
    }

    #[test]
    fn lists_a_file_unsaved_while_any_session_of_it_is_and_never_a_new_note() {
        let disk = FakeDisk::with("a.md", "a");
        let mut sessions = FileSessions::new();
        let (tab, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let (pane, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let draft = sessions.new_note(&disk);
        sessions.get_mut(draft).unwrap().set_dirty(true);
        sessions.get_mut(tab).unwrap().set_dirty(true);
        sessions.get_mut(pane).unwrap().set_dirty(true);
        let a = FileRef {
            root: "workspace".into(),
            rel: "a.md".into(),
        };
        assert_eq!(sessions.unsaved(), std::slice::from_ref(&a));
        sessions.get_mut(tab).unwrap().set_dirty(false);
        assert_eq!(sessions.unsaved(), [a]);
        assert!(sessions.close(&disk, pane));
        assert_eq!(sessions.unsaved(), []);
        assert_eq!(sessions.view(draft).unwrap().rel, DRAFT_REL);
    }

    #[test]
    fn clearing_keeps_counting_ids_and_revisions() {
        let disk = FakeDisk::with("a.md", "a");
        let mut sessions = FileSessions::new();
        let (old, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        let before = sessions.view(old).unwrap().revision;
        sessions.clear();
        assert!(sessions.is_empty());
        let (new, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        assert_ne!(new, old);
        assert!(sessions.view(new).unwrap().revision > before);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn a_closed_session_is_gone_and_stops_watching() {
        let disk = FakeDisk::with("a.md", "a");
        let mut sessions = FileSessions::new();
        let (id, _) = sessions.open(&disk, "workspace", "a.md").unwrap();
        assert!(sessions.close(&disk, id));
        assert!(!sessions.close(&disk, id));
        assert!(sessions.view(id).is_none());
        assert!(disk.watched.borrow().is_empty());
        assert_eq!(sessions.renamed(&disk, "workspace", "a.md", "b.md"), []);
        let _ = disk
            .read("workspace", "a.md")
            .map(|f| assert_eq!(f.version, version_of("a")));
    }
}
