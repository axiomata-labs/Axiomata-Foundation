//! Reading, writing and deleting files under a [`Root`] (E3/E4 of
//! `docs/plans/editor.md` §ED0).
//!
//! Writes are atomic: the content goes to a temp file next to the target,
//! opened with `O_EXCL` (a planted symlink at the temp path is never
//! followed), which is then renamed over the target. The rename keeps the
//! target's permission bits, so saving a script does not lose its `+x`.
//! Every open, rename and unlink acts relative to a pinned parent directory,
//! not by path name — see [`crate::pinned`] for why.

use std::fs;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{FilesError, io, refused};
use crate::pinned;
use crate::root::{Root, validate_components};

/// Largest file the editor reads at all.
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
/// Above this a read is flagged [`TextFile::large`]; the editor edits such a
/// file in its light mode, without syntax colours (`docs/plans/editor.md`, T2).
pub const LARGE_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Largest content the editor writes — everything it can read, it can edit (T2).
pub const MAX_WRITE_BYTES: u64 = MAX_READ_BYTES;
/// Largest image [`read_image`] inlines. Photos routinely exceed a MiB;
/// base64 inflates this by a third over IPC, still small for a local call.
pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// An opaque fingerprint of a file's content: its length and a 64-bit
/// FNV-1a hash, rendered as `<len>-<hash>`. Content-based on purpose — a
/// `touch` or an mtime with coarse resolution neither causes nor hides a
/// conflict. FNV-1a is not cryptographic; it only has to notice accidental
/// change, and it is stable across Rust versions, unlike `DefaultHasher`,
/// so a version kept in a recovery file still compares after an update.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(String);

impl Version {
    /// The version of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let hash = bytes.iter().fold(FNV_OFFSET, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME)
        });
        Self(format!("{}-{hash:016x}", bytes.len()))
    }

    /// The textual form, as it travels over IPC and the CLI.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Version {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A text file as read from a root.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextFile {
    /// The relative path as requested, with `/` separators.
    pub rel: String,
    pub content: String,
    /// Fingerprint of `content`, to hand back to [`write_text`].
    pub version: Version,
    /// Last modification time, if the file system reports one.
    pub modified: Option<DateTime<Utc>>,
    /// Over [`LARGE_FILE_BYTES`]: edited in the light mode.
    pub large: bool,
}

/// A raster image, base64-encoded, ready for a `data:` URI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Image {
    /// The relative path as requested, with `/` separators.
    pub rel: String,
    /// One of the MIME types [`image_mime`] maps an extension onto.
    pub mime: &'static str,
    pub base64: String,
}

/// Reads a UTF-8 text file of at most `max_bytes`.
///
/// Errors:
///     [`FilesError::NotFound`] if it does not exist,
///     [`FilesError::TooLarge`] over `max_bytes` (also if it grows past it
///     while being read), [`FilesError::NotUtf8`] for binary content (see
///     [`text_from_bytes`]), and every guard failure of [`Root::resolve`].
pub fn read_text(root: &Root, rel: &str, max_bytes: u64) -> Result<TextFile, FilesError> {
    let resolved = root.resolve_entry(rel)?;
    if !resolved.exists {
        return Err(FilesError::NotFound { path: rel.into() });
    }
    let (bytes, meta) = read_capped(root, rel, &resolved.target, max_bytes)?;
    let version = Version::of(&bytes);
    let large = bytes.len() as u64 > LARGE_FILE_BYTES;
    let content = text_from_bytes(bytes).ok_or_else(|| FilesError::NotUtf8 { path: rel.into() })?;
    Ok(TextFile {
        rel: normalise(rel),
        content,
        version,
        modified: meta.modified().ok().map(DateTime::<Utc>::from),
        large,
    })
}

/// `bytes` as text, or `None` for binary content: not UTF-8, or valid UTF-8
/// with a NUL byte in it (which no text file has, and the editor could not
/// show). The one rule for "is this text" — the editor's `read_text` and the
/// base side of a diff (M7.3 H2) both go through it, so the two sides of one
/// diff can never disagree about the same content.
pub fn text_from_bytes(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes)
        .ok()
        .filter(|text| !text.contains('\0'))
}

/// The current version of a file, or `None` if it does not exist — what a
/// watcher compares against to tell its own writes from someone else's.
///
/// Errors:
///     as [`read_text`], except that a missing file is `Ok(None)` and the
///     content need not be UTF-8.
pub fn current_version(root: &Root, rel: &str) -> Result<Option<Version>, FilesError> {
    let resolved = root.resolve_entry(rel)?;
    if !resolved.exists {
        return Ok(None);
    }
    let (bytes, _) = read_capped(root, rel, &resolved.target, MAX_READ_BYTES)?;
    Ok(Some(Version::of(&bytes)))
}

/// Atomically writes `content`, creating the file if it is missing, and
/// returns the new version.
///
/// With `expected`, the file on disk must still have that version — a
/// missing file counts as changed too. Without it the write is blind, as
/// the older workspace commands have always been. The check and the rename
/// are two steps, so a writer racing into that gap is not caught; the watcher
/// (E5) reports it right after.
///
/// Errors:
///     [`FilesError::TooLarge`] over `max_bytes`, [`FilesError::Conflict`]
///     for a stale `expected`, [`FilesError::Io`] if the parent directory is
///     missing, and every guard failure of [`Root::resolve`].
pub fn write_text(
    root: &Root,
    rel: &str,
    content: &str,
    expected: Option<&Version>,
    max_bytes: u64,
) -> Result<Version, FilesError> {
    if content.len() as u64 > max_bytes {
        return Err(FilesError::TooLarge {
            path: rel.into(),
            limit: max_bytes,
        });
    }
    let resolved = root.resolve_entry(rel)?;
    if let Some(expected) = expected {
        let on_disk = if resolved.exists {
            Some(Version::of(
                &read_capped(root, rel, &resolved.target, MAX_READ_BYTES)?.0,
            ))
        } else {
            None
        };
        if on_disk.as_ref() != Some(expected) {
            return Err(FilesError::Conflict { path: rel.into() });
        }
    }
    pinned::replace(root, rel, &resolved.target, content.as_bytes())?;
    Ok(Version::of(content.as_bytes()))
}

/// Makes `rel` a new, empty text file and returns its version; refused if
/// anything is already there — never an overwrite (the tree's "New file", W13).
///
/// Errors:
///     [`FilesError::Refused`] if something exists at `rel` or the guard
///     refuses it, [`FilesError::Io`] if the parent folder is missing.
pub fn create_text(root: &Root, rel: &str) -> Result<Version, FilesError> {
    let resolved = root.resolve_entry(rel)?;
    if resolved.exists {
        return Err(refused(rel, "something with this name is already there"));
    }
    pinned::create_new(root, rel, &resolved.entry)?;
    Ok(Version::of(b""))
}

/// Deletes a file. For an allowed symlink this removes the link, never the
/// file it points to.
///
/// Errors:
///     [`FilesError::NotFound`] if it does not exist, [`FilesError::Io`] if
///     the unlink fails, and every guard failure of [`Root::resolve`].
pub fn delete(root: &Root, rel: &str) -> Result<(), FilesError> {
    let resolved = root.resolve_entry(rel)?;
    if !resolved.exists {
        return Err(FilesError::NotFound { path: rel.into() });
    }
    pinned::unlink(root, rel, &resolved.entry)
}

/// Reads a raster image of at most `max_bytes`, base64-encoded.
///
/// Errors:
///     [`FilesError::Refused`] for an extension [`image_mime`] does not know
///     (checked before the file system is touched), [`FilesError::NotFound`],
///     [`FilesError::TooLarge`], and every guard failure of [`Root::resolve`].
pub fn read_image(root: &Root, rel: &str, max_bytes: u64) -> Result<Image, FilesError> {
    let mime = image_mime(rel).ok_or_else(|| {
        refused(
            rel,
            "not a supported image type (png/jpeg/gif/webp/bmp/tiff/heic/avif)",
        )
    })?;
    let resolved = root.resolve_entry(rel)?;
    if !resolved.exists {
        return Err(FilesError::NotFound { path: rel.into() });
    }
    let (bytes, _) = read_capped(root, rel, &resolved.target, max_bytes)?;
    Ok(encode_image(rel, mime, &bytes))
}

/// Bytes that came from somewhere other than a root — a git blob (M7.3 H8) —
/// as the same [`Image`] `read_image` returns, or `None` for an extension
/// [`image_mime`] does not know.
pub fn image_from_bytes(rel: &str, bytes: &[u8]) -> Option<Image> {
    image_mime(rel).map(|mime| encode_image(rel, mime, bytes))
}

fn encode_image(rel: &str, mime: &'static str, bytes: &[u8]) -> Image {
    use base64::Engine as _;
    Image {
        rel: normalise(rel),
        mime,
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

/// If `rel`'s parent is a single missing directory directly under the root,
/// creates exactly that one directory — so a write can open a new top-level
/// area (a connector module's own notes folder) without a separate step.
///
/// Deliberately narrow: the guard needs a directory to exist before it can
/// canonicalise and check it, so creating a whole chain would walk through
/// directories before any of them were proven safe. A single segment under
/// the already-canonical root has no unproven segment to walk through. A
/// deeper missing parent is left alone, and the write that follows reports
/// the real error. Anything already present under that name — a file, a
/// symlink — is left for [`Root::resolve`] to judge.
///
/// Errors:
///     [`FilesError::Refused`] for an absolute path or `..` (before anything
///     is touched), [`FilesError::Io`] if `create_dir` itself fails.
pub fn ensure_top_level_dir(root: &Root, rel: &str) -> Result<(), FilesError> {
    let rel_path = Path::new(rel);
    validate_components(rel_path)?;
    let Some(parent) = rel_path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    let is_single_segment = parent.parent().is_some_and(|gp| gp.as_os_str().is_empty());
    if !is_single_segment || root.only_file().is_some() {
        return Ok(());
    }
    let full = root.path().join(parent);
    if fs::symlink_metadata(&full).is_ok() {
        return Ok(());
    }
    fs::create_dir(&full).map_err(io(&full))
}

/// Maps an extension onto the raster MIME types the dashboard's Markdown
/// renderer allows inline (`core/markdown.ts`'s `DATA_IMAGE_RE`) and that
/// `md-file.svelte` treats as an image — keep the three lists in lockstep.
/// SVG is never included: it can carry `<script>`. Whether BMP/TIFF/HEIC/AVIF
/// actually render is up to WebKit's image stack, not this function.
pub fn image_mime(rel: &str) -> Option<&'static str> {
    let ext = Path::new(rel).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "avif" => "image/avif",
        _ => return None,
    })
}

/// Reads at most `max_bytes` from `target`; one byte more means too large,
/// which also catches a file that grew after its metadata was read.
fn read_capped(
    root: &Root,
    rel: &str,
    target: &Path,
    max_bytes: u64,
) -> Result<(Vec<u8>, fs::Metadata), FilesError> {
    let too_large = || FilesError::TooLarge {
        path: rel.into(),
        limit: max_bytes,
    };
    let file = pinned::open_read(root, rel, target)?;
    let meta = file.metadata().map_err(io(target))?;
    if meta.len() > max_bytes {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(io(target))?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large());
    }
    Ok((bytes, meta))
}

fn normalise(rel: &str) -> String {
    rel.replace('\\', "/")
}

#[cfg(test)]
mod tests;
