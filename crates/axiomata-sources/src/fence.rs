//! The fence around what may be read (C22, §8j Q13): a fixed baseline — the places
//! where keys, passwords and other programs' private data live — and the owner's own folders on top. A path is judged
//! as written and as it really is (every link resolved), so a link in an open folder never leads into a closed one.
//! What is closed is not seen at all: a caller drops it before anything else looks at it.

use std::path::{Component, Path, PathBuf};

/// One rule of the baseline, for an app to show the owner what is always closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// Anything outside the home folder: the system, other users, mounted volumes.
    OutsideHome,
    /// `~/Library`: the Keychain, browser profiles, Messages' `chat.db`, Mail's own store, every app's private data.
    /// What lives there is read through the system's own interfaces (EventKit, Mail), never as files.
    Library,
    /// Hidden files and folders (`~/.ssh`, `~/.aws`, `~/.config`, `.env`, the apps' own `~/.lirvel`, `~/.axiomata`,
    /// `~/.glut`, `~/.hauch`): where programs keep their keys and settings.
    Hidden,
    /// The library of Photos (`*.photoslibrary`).
    PhotoLibraries,
    /// Key and certificate files, keychains and password stores (`*.pem`, `*.key`, `*.p12`, `id_rsa`, `*.kdbx`, …).
    KeyFiles,
    /// Files whose name says they hold passwords (`passwords.csv`, `Passwörter.txt`): an export of a password manager.
    PasswordExports,
}

/// Every rule of the baseline, in the order an app shows them.
pub fn baseline() -> [Baseline; 6] {
    [
        Baseline::OutsideHome,
        Baseline::Library,
        Baseline::Hidden,
        Baseline::PhotoLibraries,
        Baseline::KeyFiles,
        Baseline::PasswordExports,
    ]
}

/// Endings of key, certificate, keychain and password-store files.
const KEY_ENDINGS: [&str; 16] = [
    "pem",
    "key",
    "p12",
    "pfx",
    "cer",
    "crt",
    "der",
    "keychain",
    "keychain-db",
    "kdbx",
    "kdb",
    "1pux",
    "1pif",
    "opvault",
    "agilekeychain",
    "gpg",
];

/// Names of private keys without an ending (`id_rsa`, `id_ed25519`, …).
const KEY_NAMES: [&str; 4] = ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];

/// Words in a file's name that mark a password export, lower-case.
const PASSWORD_WORDS: [&str; 5] = ["password", "passwort", "passwörter", "kennwort", "passcode"];

/// What may be read: the baseline, and the owner's own closed folders.
#[derive(Clone, Debug)]
pub struct Fence {
    home: PathBuf,
    /// The owner's closed folders, each as written and as it really is.
    closed: Vec<PathBuf>,
}

impl Fence {
    /// A fence for the home folder `home`, with the owner's closed `folders` on top of the baseline.
    pub fn new(home: &Path, folders: impl IntoIterator<Item = PathBuf>) -> Self {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        let mut closed = Vec::new();
        for folder in folders {
            if let Ok(real) = std::fs::canonicalize(&folder)
                && real != folder
            {
                closed.push(real);
            }
            closed.push(folder);
        }
        Self { home, closed }
    }

    /// A fence for the current user's home folder; `None` when it cannot be told.
    pub fn for_home(folders: impl IntoIterator<Item = PathBuf>) -> Option<Self> {
        Some(Self::new(&std::env::home_dir()?, folders))
    }

    /// Whether `path` may be read. A path that does not exist, or whose real place cannot be told, may not: what
    /// cannot be judged stays closed.
    pub fn allows(&self, path: &Path) -> bool {
        if !path.is_absolute() {
            return false;
        }
        let Ok(real) = std::fs::canonicalize(path) else {
            return false;
        };
        self.why_closed(path).is_none() && self.why_closed(&real).is_none()
    }

    /// Whether `path`, as written and without looking at the disk, lies in the owner's closed folders or under the
    /// baseline. For a path that is not there yet (a folder the owner names); [`Fence::allows`] for everything read.
    pub fn closes(&self, path: &Path) -> bool {
        self.why_closed(path).is_some()
    }

    /// The baseline's rule `path` falls under, or `None` when it is the owner's folders that close it — and `None`
    /// too when it is open; [`Fence::closes`] tells the two apart.
    pub fn rule_for(&self, path: &Path) -> Option<Baseline> {
        self.why_closed(path).flatten()
    }

    /// `Some(Some(rule))` closed by the baseline, `Some(None)` closed by the owner, `None` open.
    fn why_closed(&self, path: &Path) -> Option<Option<Baseline>> {
        let Ok(inside) = path.strip_prefix(&self.home) else {
            return Some(Some(Baseline::OutsideHome));
        };
        // `..` could climb out after the prefix matched: a path that says it is never judged as open.
        if inside
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Some(Some(Baseline::OutsideHome));
        }
        // The Mac's file system ignores case: "library" is the Library.
        let names: Vec<String> = inside
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        if names.first().is_some_and(|first| first == "library") {
            return Some(Some(Baseline::Library));
        }
        if names.iter().any(|name| name.starts_with('.')) {
            return Some(Some(Baseline::Hidden));
        }
        if names.iter().any(|name| name.ends_with(".photoslibrary")) {
            return Some(Some(Baseline::PhotoLibraries));
        }
        if let Some(name) = names.last() {
            let ending = name.rsplit_once('.').map(|(_, ending)| ending);
            if ending.is_some_and(|ending| KEY_ENDINGS.contains(&ending))
                || KEY_NAMES
                    .iter()
                    .any(|key| name == key || name.starts_with(&format!("{key}.")))
            {
                return Some(Some(Baseline::KeyFiles));
            }
            if PASSWORD_WORDS.iter().any(|word| name.contains(word)) {
                return Some(Some(Baseline::PasswordExports));
            }
        }
        if self.closed.iter().any(|folder| path.starts_with(folder)) {
            return Some(None);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home folder with a few files, cleaned up before each use.
    fn home(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("axiomata-sources-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for folder in [
            "Documents/vault",
            "Documents/Privat",
            "Library/Messages",
            ".ssh",
        ] {
            std::fs::create_dir_all(home.join(folder)).unwrap();
        }
        for file in [
            "Documents/vault/note.md",
            "Documents/Privat/diary.md",
            "Library/Messages/chat.db",
            ".ssh/id_ed25519",
        ] {
            std::fs::write(home.join(file), "x").unwrap();
        }
        std::fs::canonicalize(home).unwrap()
    }

    #[test]
    fn the_baseline_closes_keys_private_data_and_everything_outside_home() {
        let home = home("baseline");
        let fence = Fence::new(&home, []);
        assert!(fence.allows(&home.join("Documents/vault/note.md")));
        assert!(!fence.allows(&home.join("Library/Messages/chat.db")));
        assert!(!fence.allows(&home.join(".ssh/id_ed25519")));
        assert!(!fence.allows(Path::new("/etc/hosts")));
        assert!(!fence.allows(Path::new("relative/note.md")));
        // Not there: cannot be judged, stays closed.
        assert!(!fence.allows(&home.join("Documents/vault/gone.md")));

        let rule = |rel: &str| fence.rule_for(&home.join(rel));
        assert_eq!(rule(".lirvel/lirvel.db"), Some(Baseline::Hidden));
        assert_eq!(rule("Projekt/.env"), Some(Baseline::Hidden));
        assert_eq!(rule("Library/Mail/V10"), Some(Baseline::Library));
        assert_eq!(rule("library/Messages/chat.db"), Some(Baseline::Library));
        assert!(!fence.allows(&home.join("LIBRARY/Messages/chat.db")));
        assert_eq!(
            rule("Pictures/Photos Library.photoslibrary/database"),
            Some(Baseline::PhotoLibraries)
        );
        assert_eq!(rule("Downloads/server.PEM"), Some(Baseline::KeyFiles));
        assert_eq!(rule("Backup/id_rsa.pub"), Some(Baseline::KeyFiles));
        assert_eq!(rule("Backup/Tresor.kdbx"), Some(Baseline::KeyFiles));
        assert_eq!(
            rule("Downloads/Passwörter Export.csv"),
            Some(Baseline::PasswordExports)
        );
        assert_eq!(
            rule("Downloads/chrome-passwords.csv"),
            Some(Baseline::PasswordExports)
        );
        assert_eq!(rule("Documents/../Library/x"), Some(Baseline::OutsideHome));
        assert_eq!(rule("Documents/Keynote Vortrag.md"), None);
        assert!(!fence.closes(&home.join("Documents/key notes.md")));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_owner_s_folders_close_too_and_a_link_does_not_lead_around_them() {
        let home = home("owner");
        let fence = Fence::new(&home, [home.join("Documents/Privat")]);
        assert!(!fence.allows(&home.join("Documents/Privat/diary.md")));
        assert!(fence.closes(&home.join("Documents/Privat/later.md")));
        assert_eq!(
            fence.rule_for(&home.join("Documents/Privat/diary.md")),
            None
        );
        assert!(fence.allows(&home.join("Documents/vault/note.md")));
        // The file system ignores case, and the real place is judged in its real spelling.
        assert!(!fence.allows(&home.join("documents/privat/diary.md")));
        // "Privat-Archiv" is not inside "Privat".
        assert!(!fence.closes(&home.join("Documents/Privat-Archiv/x.md")));

        std::os::unix::fs::symlink(
            home.join("Documents/Privat"),
            home.join("Documents/vault/hidden-way"),
        )
        .unwrap();
        assert!(!fence.allows(&home.join("Documents/vault/hidden-way/diary.md")));
        std::os::unix::fs::symlink(home.join(".ssh"), home.join("Documents/vault/keys")).unwrap();
        assert!(!fence.allows(&home.join("Documents/vault/keys/id_ed25519")));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_closed_folder_named_through_a_link_closes_its_real_place() {
        let home = home("linked");
        std::os::unix::fs::symlink(
            home.join("Documents/Privat"),
            home.join("Documents/Tagebuch"),
        )
        .unwrap();
        let fence = Fence::new(&home, [home.join("Documents/Tagebuch")]);
        assert!(!fence.allows(&home.join("Documents/Privat/diary.md")));
        let _ = std::fs::remove_dir_all(home);
    }
}
