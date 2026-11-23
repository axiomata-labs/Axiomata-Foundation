//! What a session needs of the disk: reading and writing a file with its version, watching it, and the store of text
//! kept aside. The caller brings it — the Axiomata app over its guarded file service and its recovery store.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The fingerprint of a file's content as the file service hands it out; a write names the one it expects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(String);

impl Version {
    /// The textual form, as it travels over IPC.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Version {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for Version {
    fn from(text: &str) -> Self {
        Self(text.to_string())
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A text file as read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DiskText {
    /// The whole text.
    pub content: String,
    /// Its version, for the next write.
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub version: Version,
    /// Too large for the editor's full mode (no syntax colours, minimap or sticky scroll).
    pub large: bool,
}

/// Unsaved text kept aside from an earlier session of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// The version on disk the text was based on; `None` for a file that did not exist then.
    pub base: Option<Version>,
    /// The kept text.
    pub content: String,
    /// When it was kept, as the store wrote it (RFC 3339).
    pub saved_at: String,
}

/// The disk as a session sees it. `root` is a root id the caller understands, `rel` a path inside it.
///
/// Watching and the kept-aside store are best-effort: a session goes on without them (a conflict is still caught at
/// save time by the expected version), so their failures are the caller's to log and never reach the session.
pub trait Disk {
    /// A failed read or write, shown to the owner.
    type Error: fmt::Display;

    /// Whether `error` is a write refused because the file is no longer at the expected version.
    fn is_conflict(error: &Self::Error) -> bool;

    /// Reads a text file.
    ///
    /// # Errors
    ///
    /// Whatever the file service refuses with — gone, not text, outside its root.
    fn read(&self, root: &str, rel: &str) -> Result<DiskText, Self::Error>;

    /// Writes `content`, refusing when the file is not at `expected` (`None`: write whatever is there).
    ///
    /// # Errors
    ///
    /// A conflict ([`Disk::is_conflict`]) or whatever else the file service refuses with.
    fn write(
        &self,
        root: &str,
        rel: &str,
        content: &str,
        expected: Option<&Version>,
    ) -> Result<Version, Self::Error>;

    /// Starts reporting changes of the file from outside (counted: one [`Disk::unwatch`] per successful call). Whether
    /// it took — a session unwatches only what it watched, or it would take another session's subscription away.
    fn watch(&self, root: &str, rel: &str) -> bool;

    /// Stops one subscription taken with [`Disk::watch`].
    fn unwatch(&self, root: &str, rel: &str);

    /// Keeps `content` aside for the file, based on `base`.
    fn keep(&self, root: &str, rel: &str, base: Option<&Version>, content: &str);

    /// The text kept aside for the file, if any.
    fn kept(&self, root: &str, rel: &str) -> Option<Kept>;

    /// Forgets the text kept aside for the file.
    fn forget(&self, root: &str, rel: &str);
}
