//! Grants: files and folders the owner picked in the open dialog (E7 of
//! `docs/plans/editor.md` §ED0).
//!
//! Picking something outside every registered root is the only way a new
//! place on disk becomes reachable, and the pick happens in a native dialog
//! driven from Rust — a path the webview merely claims never gets here. A
//! grant is kept in a small JSON file so "recently opened" and the recovery
//! of unsaved work survive a restart, and it can be revoked again.
//!
//! A picked **file** gives out exactly that file ([`Root::single_file`]); a
//! picked **folder** gives out its contents under [`LinkPolicy::Contained`].
//! Picking the same path again returns the existing grant.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{FilesError, io, refused};
use crate::root::{LinkPolicy, Root};

/// Current layout of the grant file.
const GRANTS_FILE_VERSION: u32 = 1;
/// Upper bound for the grant file; a few hundred grants are a few KiB.
const MAX_GRANTS_FILE_BYTES: u64 = 1024 * 1024;

/// What a grant gives out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantKind {
    File,
    Folder,
}

/// One picked file or folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// Short numeric id, unique within the file; the root id is `grant:<id>`.
    pub id: String,
    /// Canonical path at the time of the pick.
    pub path: PathBuf,
    pub kind: GrantKind,
    pub granted_at: DateTime<Utc>,
}

impl Grant {
    /// The root this grant gives out.
    ///
    /// Errors:
    ///     [`FilesError::Io`] if the path no longer exists,
    ///     [`FilesError::Refused`] if it changed from file to folder or back.
    pub fn root(&self) -> Result<Root, FilesError> {
        match self.kind {
            GrantKind::Folder => Root::dir(&self.path, LinkPolicy::Contained),
            GrantKind::File => Root::single_file(&self.path),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantsFile {
    version: u32,
    /// The id the next grant gets. Kept on file rather than derived from
    /// the grants still listed: revoking the newest grant must not free its
    /// id, or a `grant:<id>` still held somewhere (an open tab, a recovery
    /// file) would silently name a different path afterwards.
    #[serde(default)]
    next_id: u64,
    grants: Vec<Grant>,
}

/// The grant file on disk. Every call reads it afresh, so a revoke from the
/// CLI takes effect in a running app on its next file access (E8).
#[derive(Debug, Clone)]
pub struct GrantStore {
    path: PathBuf,
}

impl GrantStore {
    /// A store backed by the JSON file at `path` (need not exist yet).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Every grant, oldest first. A missing file is an empty list.
    ///
    /// Errors:
    ///     [`FilesError::Refused`] if the file is a symlink, oversized or not
    ///     a grant file of a known version; [`FilesError::Io`] otherwise.
    pub fn list(&self) -> Result<Vec<Grant>, FilesError> {
        Ok(self.load()?.grants)
    }

    /// The grant with `id`, if there is one.
    ///
    /// Errors:
    ///     as [`Self::list`].
    pub fn get(&self, id: &str) -> Result<Option<Grant>, FilesError> {
        Ok(self.load()?.grants.into_iter().find(|g| g.id == id))
    }

    /// Grants `picked`, or returns the existing grant for the same canonical
    /// path. Only call this with a path that came out of the native dialog
    /// (or from the owner at the CLI) — it is what makes the path reachable.
    ///
    /// Errors:
    ///     [`FilesError::Io`] if `picked` cannot be canonicalised,
    ///     [`FilesError::Refused`] if it is neither a regular file nor a
    ///     directory, or is the file-system root; as [`Self::list`] for the
    ///     grant file itself.
    pub fn grant(&self, picked: &Path) -> Result<Grant, FilesError> {
        let path = picked.canonicalize().map_err(io(picked))?;
        let kind = if path.is_dir() {
            GrantKind::Folder
        } else if path.is_file() {
            GrantKind::File
        } else {
            return Err(refused(picked, "neither a file nor a folder"));
        };
        // Validates the pick the same way every later use will.
        match kind {
            GrantKind::Folder => Root::dir(&path, LinkPolicy::Contained)?,
            GrantKind::File => Root::single_file(&path)?,
        };

        let mut file = self.load()?;
        if let Some(existing) = file
            .grants
            .iter()
            .find(|g| g.path == path && g.kind == kind)
        {
            return Ok(existing.clone());
        }
        // `max` with the listed ids too, so a hand-edited file whose counter
        // lags behind still never hands out a live id.
        let next = file
            .grants
            .iter()
            .filter_map(|g| g.id.parse::<u64>().ok())
            .map(|id| id + 1)
            .chain([file.next_id, 1])
            .max()
            .unwrap_or(1);
        file.next_id = next + 1;
        let grant = Grant {
            id: next.to_string(),
            path,
            kind,
            granted_at: Utc::now(),
        };
        file.grants.push(grant.clone());
        self.save(&file)?;
        Ok(grant)
    }

    /// Revokes the grant with `id`. Returns whether there was one.
    ///
    /// Errors:
    ///     as [`Self::list`], plus [`FilesError::Io`] if saving fails.
    pub fn revoke(&self, id: &str) -> Result<bool, FilesError> {
        let mut file = self.load()?;
        let before = file.grants.len();
        file.grants.retain(|g| g.id != id);
        if file.grants.len() == before {
            return Ok(false);
        }
        self.save(&file)?;
        Ok(true)
    }

    fn load(&self) -> Result<GrantsFile, FilesError> {
        let meta = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GrantsFile {
                    version: GRANTS_FILE_VERSION,
                    next_id: 1,
                    grants: Vec::new(),
                });
            }
            Err(source) => {
                return Err(FilesError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if !meta.is_file() {
            return Err(refused(&self.path, "grant file is not a regular file"));
        }
        let mut raw = String::new();
        fs::File::open(&self.path)
            .and_then(|f| f.take(MAX_GRANTS_FILE_BYTES + 1).read_to_string(&mut raw))
            .map_err(io(&self.path))?;
        if raw.len() as u64 > MAX_GRANTS_FILE_BYTES {
            return Err(refused(&self.path, "grant file is too large"));
        }
        let file: GrantsFile = serde_json::from_str(&raw)
            .map_err(|err| refused(&self.path, format!("grant file is malformed: {err}")))?;
        if file.version != GRANTS_FILE_VERSION {
            return Err(refused(
                &self.path,
                format!(
                    "grant file version {} is not {GRANTS_FILE_VERSION}",
                    file.version
                ),
            ));
        }
        Ok(file)
    }

    /// Writes the file atomically and owner-only (`0600`): it is a list of
    /// places on this machine, nobody else's business.
    fn save(&self, file: &GrantsFile) -> Result<(), FilesError> {
        let json = serde_json::to_string_pretty(file).map_err(|err| FilesError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(err),
        })?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(io(dir))?;
        }
        let tmp = self.path.with_extension("json.axiomata-tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options
            .open(&tmp)
            .and_then(|mut f| f.write_all(json.as_bytes()));
        if let Err(source) = written {
            if source.kind() != std::io::ErrorKind::AlreadyExists {
                let _ = fs::remove_file(&tmp);
            }
            return Err(FilesError::Io { path: tmp, source });
        }
        fs::rename(&tmp, &self.path).map_err(|source| {
            let _ = fs::remove_file(&tmp);
            FilesError::Io {
                path: self.path.clone(),
                source,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "axiomata-files-grants-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn grants_a_file_and_a_folder_once_each_and_revokes_them() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("project")).unwrap();
        fs::write(dir.join("project/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("notes.txt"), "n").unwrap();
        let store = GrantStore::new(dir.join("state/file-grants.json"));
        assert!(store.list().unwrap().is_empty());

        let folder = store.grant(&dir.join("project")).unwrap();
        let file = store.grant(&dir.join("notes.txt")).unwrap();
        assert_eq!((folder.id.as_str(), folder.kind), ("1", GrantKind::Folder));
        assert_eq!((file.id.as_str(), file.kind), ("2", GrantKind::File));
        // The same path again (spelled differently) is the same grant.
        assert_eq!(store.grant(&dir.join("project/./")).unwrap(), folder);
        assert_eq!(store.list().unwrap().len(), 2);

        assert!(folder.root().unwrap().resolve("main.rs").is_ok());
        let file_root = file.root().unwrap();
        assert!(file_root.resolve("notes.txt").is_ok());
        assert!(file_root.resolve("project/main.rs").is_err());

        assert!(store.revoke("1").unwrap());
        assert!(!store.revoke("1").unwrap());
        assert_eq!(store.get("1").unwrap(), None);
        // A new grant never reuses a live id.
        assert_eq!(store.grant(&dir.join("project")).unwrap().id, "3");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("state/file-grants.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn revoking_the_newest_grant_never_frees_its_id() {
        // A `grant:1` kept by an open tab must not start naming another
        // path once grant 1 is revoked and something else is picked.
        let dir = temp_dir();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("b.txt"), "b").unwrap();
        let store = GrantStore::new(dir.join("file-grants.json"));

        let first = store.grant(&dir.join("a.txt")).unwrap();
        assert_eq!(first.id, "1");
        assert!(store.revoke(&first.id).unwrap());

        let second = store.grant(&dir.join("b.txt")).unwrap();
        assert_eq!(second.id, "2");
        assert_eq!(store.get("1").unwrap(), None);

        // An old file without the counter still never reuses a listed id.
        fs::write(
            dir.join("file-grants.json"),
            format!(
                r#"{{"version":1,"grants":[{{"id":"5","path":{:?},"kind":"file","granted_at":"2026-09-23T00:00:00Z"}}]}}"#,
                dir.join("a.txt").canonicalize().unwrap()
            ),
        )
        .unwrap();
        assert_eq!(store.grant(&dir.join("b.txt")).unwrap().id, "6");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refuses_a_symlinked_or_malformed_grant_file_and_the_filesystem_root() {
        let dir = temp_dir();
        let store = GrantStore::new(dir.join("file-grants.json"));
        assert_eq!(store.grant(Path::new("/")).unwrap_err().kind(), "Refused");

        fs::write(dir.join("file-grants.json"), "not json").unwrap();
        assert_eq!(store.list().unwrap_err().kind(), "Refused");
        fs::write(dir.join("file-grants.json"), r#"{"version":9,"grants":[]}"#).unwrap();
        assert_eq!(store.list().unwrap_err().kind(), "Refused");

        #[cfg(unix)]
        {
            fs::remove_file(dir.join("file-grants.json")).unwrap();
            fs::write(dir.join("elsewhere.json"), r#"{"version":1,"grants":[]}"#).unwrap();
            std::os::unix::fs::symlink(dir.join("elsewhere.json"), dir.join("file-grants.json"))
                .unwrap();
            assert_eq!(store.list().unwrap_err().kind(), "Refused");
        }
        let _ = fs::remove_dir_all(dir);
    }
}
