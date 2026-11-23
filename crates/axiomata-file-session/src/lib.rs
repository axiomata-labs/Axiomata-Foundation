//! One open file in an editor (Axiomata-OS `docs/plans/rust-core.md` §3d, F1–F10; the rules themselves come from
//! `docs/plans/editor.md` F8–F10, W4, W13): the version on disk the editor's text is based on, what a change from
//! outside means for it, the unsaved text kept aside — and around it the app-wide jump history, the recently opened
//! files and the folds kept per file.
//!
//! The editor keeps its document (text, undo) to itself; this crate holds everything *about* it. Four decisions carry
//! it:
//!
//! * **Every save states the version it expects.** If the file moved on, the write fails as a conflict and nothing is
//!   overwritten; the session raises a banner instead. "Keep mine" arms the next save, which then asks once.
//! * **A change from outside is judged by version, not by event.** One that carries the version the session's own
//!   save returned is its echo and is ignored. A clean document is reloaded — but the editor carries the reload out,
//!   and only while its document is still clean ([`FileSession::fetch_reload`], [`FileSession::reloaded`]): a first
//!   keystroke on its way here must never be reloaded over. Unsaved text gets the banner, never a merge.
//! * **The editor reports only the switch between saved and unsaved** ([`FileSession::set_dirty`]), never every
//!   keystroke; its text crosses over only to be saved or kept aside.
//! * **I/O is the caller's, through [`Disk`].** The crate has no file service, watcher or store of its own, so every
//!   flow is tested with a disk in memory, and the Axiomata app and Lohe bring their own.

#![forbid(unsafe_code)]

mod disk;
mod jumps;
mod memory;
mod paths;
mod session;
mod sessions;

pub use disk::{Disk, DiskText, Kept, Version};
pub use jumps::{JumpHistory, JumpPlace, MAX_JUMPS};
pub use memory::{FOLD_FILES_LIMIT, FileRef, Folds, MAX_FOLDS_PER_FILE, MAX_RECENT, with_recent};
pub use paths::{is_under, renamed_path};
pub use session::{
    Banner, ChangeKind, DRAFT_REL, DRAFT_ROOT, FOREIGN_ROOT, FailedAction, FileSession, SaveOutcome,
};
pub use sessions::{FileSessionId, FileSessionView, FileSessions};
