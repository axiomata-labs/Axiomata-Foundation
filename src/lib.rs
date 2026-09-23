//! The file service behind Axiomata-OS's file app and its own editor
//! (`docs/plans/editor.md`, milestone ED0).
//!
//! Everything a webview may touch on disk goes through a [`Root`]: a
//! canonical directory (or, for a single file picked in the open dialog, that
//! one file) plus a [`LinkPolicy`]. A caller names a file as *root + relative
//! path*, never as an absolute path, and [`Root::resolve`] is the one guard
//! that decides whether that path stays inside the root. Reads return a
//! [`Version`] of the content, and a write can demand that version still be
//! current, so an agent writing to the same file is noticed instead of
//! silently overwritten.
//!
//! Like `axiomata-terminal`, this crate has no dependency on `axiomata-core`
//! or Tauri: which roots exist (the Second-Brain workspace, IDE projects,
//! agent worktrees) is the embedder's business, answered through a
//! [`RootResolver`]. The one kind of root this crate owns itself is the
//! [`Grant`] — a file or folder the owner picked in the open dialog — since
//! a standalone editor needs those too.

pub mod error;
pub mod file;
pub mod grants;
mod pinned;
pub mod root;
pub mod watch;

pub use error::FilesError;
pub use file::{
    Image, LARGE_FILE_BYTES, MAX_IMAGE_BYTES, MAX_READ_BYTES, MAX_WRITE_BYTES, TextFile, Version,
    current_version, delete, ensure_top_level_dir, read_image, read_text, write_text,
};
pub use grants::{Grant, GrantKind, GrantStore};
pub use root::{LinkPolicy, Root, RootResolver};
pub use watch::{Change, ChangeKind, FileWatcher};
