//! The one error type of the file service.

use std::path::PathBuf;

/// Why a file operation failed. [`FilesError::kind`] is the stable word the
/// dashboard branches on (a conflict opens the reload bar, a binary file is
/// refused politely, …); the `Display` text is for people.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    /// The root id names nothing the embedder knows (an agent's worktree that
    /// has been discarded, a revoked grant).
    #[error("unknown file root `{0}`")]
    UnknownRoot(String),

    /// The path breaks a guard: it climbs out, is absolute, is a directory, a
    /// link the root's policy does not allow, or not a regular file.
    #[error("refused {path}: {reason}")]
    Refused { path: PathBuf, reason: String },

    /// The file does not exist.
    #[error("{path} does not exist")]
    NotFound { path: PathBuf },

    /// The file or the content to write exceeds the caller's limit.
    #[error("{path} is larger than the {limit}-byte limit")]
    TooLarge { path: PathBuf, limit: u64 },

    /// The file is not UTF-8 text — a binary file, as far as the editor cares.
    #[error("{path} is not valid UTF-8")]
    NotUtf8 { path: PathBuf },

    /// A write expected a version of the file that is no longer on disk.
    #[error("{path} changed since it was read")]
    Conflict { path: PathBuf },

    /// Any other file-system failure, including a missing parent directory.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl FilesError {
    /// The stable machine-readable name of the variant, e.g. `"Conflict"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnknownRoot(_) => "UnknownRoot",
            Self::Refused { .. } => "Refused",
            Self::NotFound { .. } => "NotFound",
            Self::TooLarge { .. } => "TooLarge",
            Self::NotUtf8 { .. } => "NotUtf8",
            Self::Conflict { .. } => "Conflict",
            Self::Io { .. } => "Io",
        }
    }
}

/// Shorthand for a [`FilesError::Refused`].
pub(crate) fn refused(path: impl Into<PathBuf>, reason: impl Into<String>) -> FilesError {
    FilesError::Refused {
        path: path.into(),
        reason: reason.into(),
    }
}

/// Maps an `io::Error` onto [`FilesError::Io`] for `path`.
pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> FilesError {
    let path = path.into();
    move |source| FilesError::Io { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_every_variant_and_is_stable_for_callers_to_match_on() {
        let path = PathBuf::from("notes/inbox.md");
        let cases: [(FilesError, &str); 7] = [
            (FilesError::UnknownRoot("grant:9".into()), "UnknownRoot"),
            (refused(&path, "is a directory"), "Refused"),
            (FilesError::NotFound { path: path.clone() }, "NotFound"),
            (
                FilesError::TooLarge {
                    path: path.clone(),
                    limit: 1024,
                },
                "TooLarge",
            ),
            (FilesError::NotUtf8 { path: path.clone() }, "NotUtf8"),
            (FilesError::Conflict { path: path.clone() }, "Conflict"),
            (
                FilesError::Io {
                    path: path.clone(),
                    source: std::io::ErrorKind::NotFound.into(),
                },
                "Io",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.kind(), expected);
        }
    }

    #[test]
    fn display_text_names_the_path_for_a_person_to_read() {
        let path = PathBuf::from("notes/inbox.md");
        assert_eq!(
            FilesError::NotFound { path: path.clone() }.to_string(),
            "notes/inbox.md does not exist"
        );
        assert_eq!(
            refused(&path, "is a directory").to_string(),
            "refused notes/inbox.md: is a directory"
        );
        assert_eq!(
            FilesError::UnknownRoot("grant:9".into()).to_string(),
            "unknown file root `grant:9`"
        );
    }
}
