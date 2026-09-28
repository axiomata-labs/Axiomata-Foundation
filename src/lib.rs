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

pub mod dir;
pub mod error;
pub mod file;
pub mod grants;
pub mod index;
pub mod lsp;
mod pinned;
pub mod root;
pub mod search;
pub mod watch;

pub use dir::{
    DirEntry, EntryKind, Listing, MAX_COUNT, MAX_LISTING, count_tree, delete_tree, list_dir,
    make_dir, rename_entry,
};
pub use error::FilesError;
pub use file::{
    Image, LARGE_FILE_BYTES, MAX_IMAGE_BYTES, MAX_READ_BYTES, MAX_WRITE_BYTES, TextFile, Version,
    create_text, current_version, delete, ensure_top_level_dir, image_from_bytes, image_mime,
    read_image, read_text, text_from_bytes, write_text,
};
pub use grants::{Grant, GrantKind, GrantStore};
pub use index::{FileIndex, MAX_INDEX, index_files};
pub use root::{LinkPolicy, Root, RootResolver};
pub use search::{FileMatches, LineMatch, MAX_MATCHES, SearchQuery, SearchSummary, search};
pub use watch::{Change, ChangeKind, FileWatcher};
