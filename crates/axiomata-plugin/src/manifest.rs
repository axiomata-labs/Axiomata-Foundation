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
    /// The places in the app's window where it wants its page shown (Hauch: `sidebar`, an entry of its own in the
    /// navigation), beside the app's page "Plugins" every plugin has; the user may refuse each (plugins.md Q15).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<String>,
    /// Its sign where it has a place: a name from the app's icon set ([`App::icons`]), never a picture of its own,
    /// so it always looks like the app around it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
        if app
            .reserved
            .iter()
            .any(|own| folded(own) == folded(&self.name))
        {
            return bad(format!(
                "the name {:?} is one of {}'s own pages",
                self.name, app.name
            ));
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
        if self.rights.len() > MAX_LIST
            || self.provides.len() > MAX_LIST
            || self.places.len() > MAX_LIST
        {
            return bad("too many rights, hooks or places".into());
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
        for (i, place) in self.places.iter().enumerate() {
            if !app.places.iter().any(|known| known == place) {
                return bad(format!("the place {place:?} is not one {} has", app.name));
            }
            if self.places[..i].contains(place) {
                return bad(format!("the place {place:?} is named twice"));
            }
        }
        if let Some(icon) = &self.icon
            && !app.icons.iter().any(|known| known == icon)
        {
            return bad(format!("the icon {icon:?} is not one {} has", app.name));
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
/// zero-width space, a line or paragraph separator, a blank that is not a space (Hangul filler, Braille blank), a
/// variation selector or tag: in a name or a description it could make the page say what the plugin is not.
fn is_format(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{2800}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}'
        | '\u{FFF9}'..='\u{FFFB}' | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E0FFF}')
}

/// `name` as compared with an app's own: without case and without spaces.
fn folded(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
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
