//! A plugin's manifest, `plugin.json`, beside its `plugin.wasm` in every release: who it is, what it was built
//! against, what it asks for, and the checksum of its code.

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{App, Error, Result};

/// The manifest's file name in a release and on disk.
pub const MANIFEST: &str = "plugin.json";
/// The component's file name in a release and on disk.
pub const WASM: &str = "plugin.wasm";

/// The memory a plugin gets unless its manifest asks for more, and the most it may ask for (plugins.md Q6).
pub const DEFAULT_MEMORY_MIB: u32 = 64;
pub const MAX_MEMORY_MIB: u32 = 512;

/// The rights every app knows; an app adds its own ([`App::rights`]).
pub const SHARED_RIGHTS: &[&str] = &[STORAGE];
/// The right to a storage of the plugin's own.
pub const STORAGE: &str = "storage";

const MAX_ID_CHARS: usize = 40;
const MAX_NAME_CHARS: usize = 60;
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_AUTHOR_CHARS: usize = 100;
const MAX_LIST: usize = 32;

/// What `plugin.json` says. Fields this version does not know are ignored, so a newer manifest still reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The plugin's id: 1–40 characters of `a-z`, `0-9` and `-`. Its folder is named after it.
    pub id: String,
    /// Its name as the user sees it.
    pub name: String,
    pub version: Version,
    /// The app it is for (`hauch`, `lirvel`, `glut`).
    pub app: String,
    /// The version of the app's plugin interface it was built against.
    pub interface: Version,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// The rights it asks for; it gets none it did not ask for, and the user may refuse each.
    #[serde(default)]
    pub rights: Vec<String>,
    /// The app's hooks it answers (Hauch: `filter-recognised`); the app calls no other.
    #[serde(default)]
    pub provides: Vec<String>,
    /// The memory it needs in MiB, if more than [`DEFAULT_MEMORY_MIB`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u32>,
    /// The SHA-256 of its `plugin.wasm`, lowercase hex.
    pub sha256: String,
}

impl Manifest {
    /// Reads a manifest and checks it against `app`.
    pub fn read(bytes: &[u8], app: &App) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|err| Error::Manifest(format!("not readable: {err}")))?;
        manifest.check(app)?;
        Ok(manifest)
    }

    /// The memory this plugin may use, in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.memory_mib.unwrap_or(DEFAULT_MEMORY_MIB) as usize * 1024 * 1024
    }

    /// Whether every field is well formed and fits `app`.
    pub fn check(&self, app: &App) -> Result<()> {
        let bad = |what: String| Err(Error::Manifest(what));
        if !is_id(&self.id) {
            return bad(format!(
                "the id {:?} is not 1–{MAX_ID_CHARS} characters of a-z, 0-9 and -",
                self.id
            ));
        }
        for (field, text, most) in [
            ("name", &self.name, MAX_NAME_CHARS),
            ("description", &self.description, MAX_DESCRIPTION_CHARS),
            ("author", &self.author, MAX_AUTHOR_CHARS),
        ] {
            if text.chars().count() > most || text.chars().any(|c| c.is_control() || is_format(c)) {
                return bad(format!(
                    "the {field} is longer than {most} characters or holds control characters"
                ));
            }
        }
        if self.name.trim().is_empty() {
            return bad("the name is empty".into());
        }
        if self.app != app.name {
            let app: String = self
                .app
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(40)
                .collect();
            return Err(Error::WrongApp(app));
        }
        if !app.speaks(&self.interface) {
            return Err(Error::NewerApp(self.interface.clone()));
        }
        if self.rights.len() > MAX_LIST || self.provides.len() > MAX_LIST {
            return bad("too many rights or hooks".into());
        }
        for (i, right) in self.rights.iter().enumerate() {
            if !app.knows_right(right) {
                return bad(format!("the right {right:?} is not one {} knows", app.name));
            }
            if self.rights[..i].contains(right) {
                return bad(format!("the right {right:?} is named twice"));
            }
        }
        for hook in &self.provides {
            if !app.hooks.iter().any(|known| known == hook) {
                return bad(format!("the hook {hook:?} is not one {} has", app.name));
            }
        }
        if self
            .memory_mib
            .is_some_and(|mib| mib == 0 || mib > MAX_MEMORY_MIB)
        {
            return bad(format!(
                "the memory is not between 1 and {MAX_MEMORY_MIB} MiB"
            ));
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return bad("the sha256 is not 64 lowercase hex digits".into());
        }
        Ok(())
    }
}

/// Whether `c` changes how text around it is shown without being seen itself — a direction override (U+202E), a
/// zero-width space, a line or paragraph separator: in a name or a description it could make the page say what the plugin is not.
fn is_format(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// Whether `id` can name a plugin and its folder.
pub fn is_id(id: &str) -> bool {
    (1..=MAX_ID_CHARS).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}
