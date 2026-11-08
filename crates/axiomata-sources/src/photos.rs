//! The owner's photos (§8m S5c), read only from the Photos library where it is — `~/Pictures/Photos
//! Library.photoslibrary` — which an app may read only with Full Disk Access, granted by hand. Two of Photos' own
//! stores are read, never written: `database/Photos.sqlite` for each picture's date, kind, favourite mark and albums,
//! and `database/search/leo.sqlite`, the index behind the search field of Photos, for the words a picture carries —
//! people, places, what is in it, holidays — in the language Photos speaks. Hidden pictures, those in the trash and
//! the takes of a burst Photos does not show are never read; nor any in an album the caller closed, nor one Photos
//! found an identity document, a passport or a barcode in. Screenshots only when asked for, and then only they. Of a
//! picture the caller learns its date, kind, place (town and country) and where its small preview lies — never the
//! picture itself, nor the text Photos read in it. Both stores are Photos' own and undocumented: when one does not
//! look as expected, a read fails as [`PhotoError::UnknownFormat`] rather than guess.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The most pictures one read returns.
pub const MAX_READ: usize = 60;
/// The most words one read looks for.
pub const MAX_WORDS: usize = 4;
/// The longest word looked for.
const MAX_WORD_CHARS: usize = 60;
/// Seconds from 1970 to 2001, where Photos' clock starts.
const APPLE_EPOCH: i64 = 978_307_200;
/// The largest preview read.
const MAX_THUMBNAIL_BYTES: u64 = 4 * 1024 * 1024;
/// The longest name of a place or an album kept.
const MAX_NAME_CHARS: usize = 80;
/// How long Photos may take to show a picture.
#[cfg(target_os = "macos")]
const SHOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The index's categories a word is looked for in: places (2010–2220), holidays, seasons and times of day (1030–1050),
/// people, pets and ages (3000–3030), what is in a picture (4000–4020, 4090, 4100), concerts and shows (2240–2260),
/// and the kinds of a picture Photos names (5000–5150, screenshots aside: those are asked for apart). Never the text
/// read in a picture (4110/4120), documents (4080/4130, 11000–11020), file names, captions, keywords, apps or
/// cameras (6000–8170), nor albums (read from the library itself, where the closed ones are known).
const SEARCHED: &[i64] = &[
    1030, 1040, 1050, 2010, 2030, 2050, 2060, 2070, 2080, 2090, 2100, 2110, 2120, 2130, 2140, 2160,
    2170, 2180, 2190, 2210, 2220, 2230, 2240, 2250, 2260, 3000, 3010, 3030, 4000, 4010, 4020, 4090,
    4100, 5000, 5010, 5050, 5060, 5080, 5090, 5120, 5140, 5150,
];
/// A person's name: a word of it is enough ("Jonas" finds "Jonas Weber").
const PERSON: i64 = 3000;
/// A town, and a country, as a picture's place is told.
const TOWN: i64 = 2090;
const COUNTRY: i64 = 2160;
/// What marks a picture never read: documents, receipts and handwriting (4080), an identity document, a passport, a
/// barcode (4130), an identity card or an address Photos read in it (11000, 11010, 11020).
const NEVER: &[i64] = &[4080, 4130, 11000, 11010, 11020];
/// The index's kind of an entry that is a picture (others are albums, memories, …).
const ITEM_ASSET: i64 = 1;
/// Photos' own kind of an album the owner made.
const USER_ALBUM: i64 = 2;
/// Photos' subtype of a screenshot.
const SCREENSHOT_SUBTYPE: i64 = 10;

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhotoError {
    /// The library cannot be read: the app has no Full Disk Access.
    NoDiskAccess,
    /// There is no Photos library where Photos keeps it.
    NoLibrary,
    /// The library's stores do not look as this reader knows them (a newer Photos).
    UnknownFormat,
    /// A picture's id, a word or an album that is none.
    Invalid(&'static str),
    /// The album asked for is closed, or one closed is not found any more (renamed or deleted): nothing is read.
    AlbumClosed,
    /// Photos did not answer in time.
    TimedOut,
    /// Reading or showing failed; only what, never a store's own text.
    Failed(String),
}

impl std::fmt::Display for PhotoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhotoError::NoDiskAccess => write!(
                f,
                "the photos cannot be read: allow Full Disk Access in System Settings → Privacy & Security"
            ),
            PhotoError::NoLibrary => write!(f, "there is no Photos library on this Mac"),
            PhotoError::UnknownFormat => write!(
                f,
                "this version of Photos keeps its library in a form that cannot be read yet"
            ),
            PhotoError::Invalid(why) => write!(f, "{why}"),
            PhotoError::AlbumClosed => write!(
                f,
                "that album is closed, or an album closed on the page is not found in Photos any more (the owner \
                 reopens it there); nothing was read"
            ),
            PhotoError::TimedOut => write!(f, "Photos did not answer in time"),
            PhotoError::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for PhotoError {}

/// What a picture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Photo,
    Video,
    /// A screenshot, by Photos' own mark or its detection: read only when asked for.
    Screenshot,
}

/// A picture as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Photo {
    /// Photos' own id of it.
    pub uuid: String,
    /// When it was taken, seconds since 1970.
    pub at: i64,
    pub kind: Kind,
    pub favorite: bool,
    /// The town it was taken in, when Photos knows it.
    pub town: Option<String>,
    pub country: Option<String>,
}

/// What a read found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The pictures, the newest first; at most the read's limit.
    pub photos: Vec<Photo>,
    /// How many pictures fit in all.
    pub total: usize,
    /// The words the index knows nothing by, and an album asked for that is none: what was looked for in vain.
    pub unknown: Vec<String>,
}

/// What to read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Only what was taken from then on, seconds since 1970.
    pub since: i64,
    /// Only what was taken before then.
    pub until: i64,
    /// Only pictures that carry every one of these words (a person, a place, what is in it), as Photos' search
    /// knows them; at most [`MAX_WORDS`].
    pub words: Vec<String>,
    /// Only the pictures of the owner's album of this name.
    pub album: Option<String>,
    pub favorites: bool,
    /// Only screenshots — else never one.
    pub screenshots: bool,
    /// At most this many, the newest; at most [`MAX_READ`].
    pub limit: usize,
}

/// Where Photos keeps its library.
pub fn library_path() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join("Pictures/Photos Library.photoslibrary"))
}

/// Whether this app may read the Photos library: it has Full Disk Access, and the library is there.
pub fn readable() -> bool {
    library_path().is_some_and(|library| open(&library.join("database/Photos.sqlite")).is_ok())
}

/// A store of the library, read only where it is: its journal (WAL) is read too, nothing is written.
fn open(path: &Path) -> Result<rusqlite::Connection, PhotoError> {
    use rusqlite::OpenFlags;
    if let Err(err) = std::fs::File::open(path) {
        return Err(match err.kind() {
            std::io::ErrorKind::PermissionDenied => PhotoError::NoDiskAccess,
            std::io::ErrorKind::NotFound => PhotoError::NoLibrary,
            _ => PhotoError::Failed("the Photos library could not be read".into()),
        });
    }
    let db = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| store_error(&err))?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|err| store_error(&err))?;
    // Read only twice over: no statement of this connection may write.
    db.pragma_update(None, "query_only", true)
        .map_err(|err| store_error(&err))?;
    Ok(db)
}

fn store_error(err: &rusqlite::Error) -> PhotoError {
    let text = err.to_string().to_lowercase();
    if text.contains("authoriz") || text.contains("not permitted") {
        PhotoError::NoDiskAccess
    } else if text.contains("no such") {
        PhotoError::UnknownFormat
    } else {
        PhotoError::Failed("the Photos library could not be read".into())
    }
}

/// The columns of `table`, empty when there is no such table.
fn columns(db: &rusqlite::Connection, table: &str) -> HashSet<String> {
    let Ok(mut statement) = db.prepare("SELECT name FROM pragma_table_info(?1)") else {
        return HashSet::new();
    };
    statement
        .query_map([table], |row| row.get::<_, String>(0))
        .map(|rows| {
            rows.filter_map(Result::ok)
                .map(|c| c.to_uppercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `table` has every one of `needed`.
fn has(db: &rusqlite::Connection, table: &str, needed: &[&str]) -> bool {
    let found = columns(db, table);
    needed.iter().all(|column| found.contains(*column))
}

/// The table that ties albums to pictures and its two columns — named by Core Data after each entity's number,
/// which differs between versions of Photos (`Z_34ASSETS`: `Z_34ALBUMS`, `Z_3ASSETS`).
fn album_link(db: &rusqlite::Connection) -> Result<(String, String, String), PhotoError> {
    let number = |name: &str| -> Result<i64, PhotoError> {
        db.query_row(
            "SELECT Z_ENT FROM Z_PRIMARYKEY WHERE Z_NAME = ?1",
            [name],
            |row| row.get(0),
        )
        .map_err(|_| PhotoError::UnknownFormat)
    };
    let (album, asset) = (number("Album")?, number("Asset")?);
    let (table, albums, assets) = (
        format!("Z_{album}ASSETS"),
        format!("Z_{album}ALBUMS"),
        format!("Z_{asset}ASSETS"),
    );
    if has(db, &table, &[albums.as_str(), assets.as_str()]) {
        Ok((table, albums, assets))
    } else {
        Err(PhotoError::UnknownFormat)
    }
}

/// Whether both stores look as this reader knows them.
fn known(library: &rusqlite::Connection, index: &rusqlite::Connection) -> Result<(), PhotoError> {
    let asset = [
        "Z_PK",
        "ZUUID",
        "ZDATECREATED",
        "ZKIND",
        "ZKINDSUBTYPE",
        "ZFAVORITE",
        "ZHIDDEN",
        "ZTRASHEDSTATE",
        "ZVISIBILITYSTATE",
    ];
    let album = ["Z_PK", "ZKIND", "ZTITLE", "ZTRASHEDSTATE"];
    let ok = has(library, "ZASSET", &asset)
        && has(library, "ZGENERICALBUM", &album)
        && has(index, "ITEMS", &["IDENTIFIER", "TYPE", "LEXEME_IDS"])
        && has(index, "LEXICON", &["LEXEME_ID", "CATEGORY", "CONTENT"]);
    if ok {
        Ok(())
    } else {
        Err(PhotoError::UnknownFormat)
    }
}

/// The owner's albums by their names, to close one or ask for its pictures.
///
/// # Errors
///
/// See [`PhotoError`].
pub fn albums() -> Result<Vec<String>, PhotoError> {
    let library = library_path().ok_or(PhotoError::NoLibrary)?;
    albums_of(&open(&library.join("database/Photos.sqlite"))?)
}

fn albums_of(db: &rusqlite::Connection) -> Result<Vec<String>, PhotoError> {
    if !has(db, "ZGENERICALBUM", &["ZKIND", "ZTITLE", "ZTRASHEDSTATE"]) {
        return Err(PhotoError::UnknownFormat);
    }
    let mut statement = db
        .prepare("SELECT ZTITLE FROM ZGENERICALBUM WHERE ZKIND = ?1 AND ZTRASHEDSTATE = 0 AND ZTITLE IS NOT NULL")
        .map_err(|err| store_error(&err))?;
    let mut names: Vec<String> = statement
        .query_map([USER_ALBUM], |row| row.get::<_, String>(0))
        .map_err(|err| store_error(&err))?
        .filter_map(Result::ok)
        .map(|name| clean(&name))
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    Ok(names)
}

/// Reads what `query` asks for, the newest first; never a picture of an album named in `closed`.
///
/// # Errors
///
/// [`PhotoError::NoDiskAccess`] without Full Disk Access, [`PhotoError::UnknownFormat`] for stores of a form not
/// known, else see [`PhotoError`].
pub fn read(query: &Query, closed: &[String]) -> Result<Found, PhotoError> {
    let library = library_path().ok_or(PhotoError::NoLibrary)?;
    let photos = open(&library.join("database/Photos.sqlite"))?;
    let index = open(&library.join("database/search/leo.sqlite"))?;
    read_from(&photos, &index, query, closed)
}

/// One entry of the index's lexicon, as far as a read needs it.
struct Lexeme {
    category: i64,
    content: String,
}

/// [`read`] on the stores `library` and `index`.
fn read_from(
    library: &rusqlite::Connection,
    index: &rusqlite::Connection,
    query: &Query,
    closed: &[String],
) -> Result<Found, PhotoError> {
    known(library, index)?;
    let words: Vec<String> = query
        .words
        .iter()
        .map(|word| word.trim().to_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    if words.len() > MAX_WORDS
        || words
            .iter()
            .any(|word| word.chars().count() > MAX_WORD_CHARS)
    {
        return Err(PhotoError::Invalid(
            "too many or too long words to look for",
        ));
    }
    if words.iter().any(|word| {
        word.chars()
            .any(|c| c.is_control() || crate::calendar::invisible(c))
    }) {
        return Err(PhotoError::Invalid("a word holds hidden characters"));
    }
    let mut unknown = Vec::new();

    // The lexicon, as far as it is looked in: the categories searched, the places told and those never read.
    let mut lexicon: HashMap<u32, Vec<Lexeme>> = HashMap::new();
    {
        let mut statement = index
            .prepare("SELECT lexeme_id, category, content FROM lexicon")
            .map_err(|err| store_error(&err))?;
        let mut rows = statement.query([]).map_err(|err| store_error(&err))?;
        while let Some(row) = rows.next().map_err(|err| store_error(&err))? {
            let category: i64 = row.get(1).unwrap_or(0);
            if !SEARCHED.contains(&category) && !NEVER.contains(&category) {
                continue;
            }
            // A mark of what is never read counts without its word too; one that cannot be read fails the read.
            let never = NEVER.contains(&category);
            let content = match row.get::<_, Option<String>>(2) {
                Ok(Some(content)) => content,
                Ok(None) | Err(_) if never => String::new(),
                _ => continue,
            };
            let id = match row
                .get::<_, i64>(0)
                .ok()
                .and_then(|id| u32::try_from(id).ok())
            {
                Some(id) => id,
                None if never => return Err(PhotoError::UnknownFormat),
                None => continue,
            };
            lexicon
                .entry(id)
                .or_default()
                .push(Lexeme { category, content });
        }
    }
    // Each word, as the set of the lexicon's entries that name it.
    let mut wanted: Vec<HashSet<u32>> = Vec::new();
    for word in &words {
        let ids: HashSet<u32> = lexicon
            .iter()
            .filter(|(_, entries)| {
                entries.iter().any(|entry| {
                    SEARCHED.contains(&entry.category) && {
                        let content = entry.content.to_lowercase();
                        content == *word
                            || (entry.category == PERSON
                                && content.split_whitespace().any(|part| part == word))
                    }
                })
            })
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            unknown.push(word.clone());
        }
        wanted.push(ids);
    }
    if !unknown.is_empty() {
        return Ok(Found {
            unknown,
            ..Found::default()
        });
    }

    // The index's entry of every picture: the words it carries.
    let mut carried: HashMap<String, Vec<u32>> = HashMap::new();
    {
        let mut statement = index
            .prepare("SELECT identifier, lexeme_ids FROM items WHERE type = ?1")
            .map_err(|err| store_error(&err))?;
        let mut rows = statement
            .query([ITEM_ASSET])
            .map_err(|err| store_error(&err))?;
        while let Some(row) = rows.next().map_err(|err| store_error(&err))? {
            // A picture's entry that cannot be read is no entry to trust: its marks could be missing.
            let (Ok(id), Ok(blob)) = (row.get::<_, String>(0), row.get::<_, Vec<u8>>(1)) else {
                return Err(PhotoError::UnknownFormat);
            };
            // Little-endian 32-bit ids, one after the other; a blob of another length is no list of them.
            if blob.len() % 4 != 0 {
                return Err(PhotoError::UnknownFormat);
            }
            let ids = blob
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| u32::from_le_bytes(*bytes))
                .collect();
            carried.insert(id.to_uppercase(), ids);
        }
    }
    let never = |ids: &[u32]| {
        ids.iter().any(|id| {
            lexicon
                .get(id)
                .is_some_and(|entries| entries.iter().any(|entry| NEVER.contains(&entry.category)))
        })
    };
    let first_of = |ids: &[u32], category: i64| -> Option<String> {
        ids.iter().find_map(|id| {
            lexicon.get(id).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.category == category)
                    .map(|entry| short(&clean(&entry.content)))
            })
        })
    };

    // The albums: those closed, and the one asked for.
    let (link, link_album, link_asset) = album_link(library)?;
    let album_titles: Vec<(i64, String)> = {
        let mut statement = library
            .prepare(
                "SELECT Z_PK, ZTITLE FROM ZGENERICALBUM
                 WHERE ZKIND = ?1 AND ZTRASHEDSTATE = 0 AND ZTITLE IS NOT NULL",
            )
            .map_err(|err| store_error(&err))?;
        statement
            .query_map([USER_ALBUM], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|err| store_error(&err))?
            .collect::<Result<_, _>>()
            .map_err(|err| store_error(&err))?
    };
    let members = |album: i64| -> Result<HashSet<i64>, PhotoError> {
        let mut statement = library
            .prepare(&format!(
                "SELECT {link_asset} FROM {link} WHERE {link_album} = ?1"
            ))
            .map_err(|err| store_error(&err))?;
        statement
            .query_map([album], |row| row.get::<_, i64>(0))
            .map_err(|err| store_error(&err))?
            .collect::<Result<_, _>>()
            .map_err(|err| store_error(&err))
    };
    let same = |a: &str, b: &str| clean(a).to_lowercase() == clean(b).to_lowercase();
    // A closed album that is not found any more (renamed, deleted) reads nothing: closing must not fail open.
    if closed
        .iter()
        .any(|name| !album_titles.iter().any(|(_, title)| same(name, title)))
    {
        return Err(PhotoError::AlbumClosed);
    }
    let mut shut: HashSet<i64> = HashSet::new();
    for (album, title) in &album_titles {
        if closed.iter().any(|name| same(name, title)) {
            shut.extend(members(*album)?);
        }
    }
    let only: Option<HashSet<i64>> = match query
        .album
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        None => None,
        Some(name) => {
            if closed.iter().any(|closed| same(closed, name)) {
                return Err(PhotoError::AlbumClosed);
            }
            let mut pictures = HashSet::new();
            let mut any = false;
            for (album, title) in &album_titles {
                if same(name, title) {
                    any = true;
                    pictures.extend(members(*album)?);
                }
            }
            if !any {
                return Ok(Found {
                    unknown: vec![format!("album {}", short(&clean(name)))],
                    ..Found::default()
                });
            }
            Some(pictures)
        }
    };

    let since = (query.since - APPLE_EPOCH) as f64;
    let until = (query.until - APPLE_EPOCH) as f64;
    let screenshot_column = columns(library, "ZASSET").contains("ZISDETECTEDSCREENSHOT");
    let mut statement = library
        .prepare(&format!(
            "SELECT Z_PK, ZUUID, ZDATECREATED, ZKIND, ZKINDSUBTYPE, ZFAVORITE, {}
             FROM ZASSET
             WHERE ZTRASHEDSTATE = 0 AND ZHIDDEN = 0 AND ZVISIBILITYSTATE = 0
               AND ZDATECREATED >= ?1 AND ZDATECREATED < ?2 AND ZUUID IS NOT NULL
             ORDER BY ZDATECREATED DESC",
            if screenshot_column {
                "ZISDETECTEDSCREENSHOT"
            } else {
                "0"
            }
        ))
        .map_err(|err| store_error(&err))?;
    let mut rows = statement
        .query(rusqlite::params![since, until])
        .map_err(|err| store_error(&err))?;
    let limit = query.limit.clamp(1, MAX_READ);
    let mut found = Found::default();
    let mut unindexed = 0_usize;
    while let Some(row) = rows.next().map_err(|err| store_error(&err))? {
        let key: i64 = row.get(0).unwrap_or(0);
        if shut.contains(&key) || only.as_ref().is_some_and(|only| !only.contains(&key)) {
            continue;
        }
        let Ok(uuid) = row.get::<_, String>(1) else {
            continue;
        };
        let uuid = uuid.to_uppercase();
        if !plain_uuid(&uuid) {
            continue;
        }
        let kind_code: i64 = row.get(3).unwrap_or(0);
        let subtype: i64 = row.get(4).unwrap_or(0);
        let detected = row.get::<_, Option<i64>>(6).ok().flatten().unwrap_or(0) != 0;
        let kind = if subtype == SCREENSHOT_SUBTYPE || detected {
            Kind::Screenshot
        } else if kind_code == 1 {
            Kind::Video
        } else {
            Kind::Photo
        };
        if (kind == Kind::Screenshot) != query.screenshots {
            continue;
        }
        let favorite = row.get::<_, Option<i64>>(5).ok().flatten().unwrap_or(0) != 0;
        if query.favorites && !favorite {
            continue;
        }
        // A picture the index does not know yet is never read: unknown, it could be a document as well.
        let Some(ids) = carried.get(&uuid).map(Vec::as_slice) else {
            unindexed += 1;
            continue;
        };
        if never(ids) {
            continue;
        }
        if !wanted
            .iter()
            .all(|word| ids.iter().any(|id| word.contains(id)))
        {
            continue;
        }
        found.total += 1;
        if found.photos.len() < limit {
            let date: f64 = row.get(2).unwrap_or(0.0);
            found.photos.push(Photo {
                uuid,
                at: date as i64 + APPLE_EPOCH,
                kind,
                favorite,
                town: first_of(ids, TOWN),
                country: first_of(ids, COUNTRY),
            });
        }
    }
    // An index that knows none of the pictures asked for is no index to trust: a newer Photos may name them otherwise.
    if found.total == 0 && unindexed > 0 && carried.is_empty() {
        return Err(PhotoError::UnknownFormat);
    }
    Ok(found)
}

/// Whether `uuid` is one of Photos' ids: 36 characters, hex digits and four dashes where they belong.
pub fn plain_uuid(uuid: &str) -> bool {
    uuid.len() == 36
        && uuid.char_indices().all(|(n, c)| {
            if matches!(n, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// The small preview Photos keeps of the picture `uuid`, as JPEG bytes; `None` when there is none on this Mac (a
/// library that keeps its pictures in iCloud may not have it yet).
pub fn thumbnail(uuid: &str) -> Option<Vec<u8>> {
    let library = library_path()?;
    thumbnail_in(&library, uuid)
}

fn thumbnail_in(library: &Path, uuid: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let uuid = uuid.to_uppercase();
    if !plain_uuid(&uuid) {
        return None;
    }
    let first = &uuid[..1];
    let derivatives = library.join("resources/derivatives");
    let places = [
        derivatives.join(first).join(format!("{uuid}_1_105_c.jpeg")),
        derivatives
            .join("masters")
            .join(first)
            .join(format!("{uuid}_4_5005_c.jpeg")),
    ];
    for path in places {
        // Never through a link: the preview is a plain file of the library, or none.
        let Ok(file) = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) else {
            continue;
        };
        let file = std::fs::File::from(file);
        let Ok(meta) = file.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_THUMBNAIL_BYTES {
            continue;
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_THUMBNAIL_BYTES)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.starts_with(&[0xFF, 0xD8])
        {
            return Some(bytes);
        }
    }
    None
}

/// The script that shows a picture in Photos: its id is the script's argument.
#[cfg(target_os = "macos")]
const SHOW: &str = r#"
on run argv
  tell application "Photos"
    activate
    spotlight media item id (item 1 of argv)
  end tell
  return "shown"
end run
"#;

/// Opens Photos at the picture `uuid`. Photos asks the owner once whether this app may use it.
///
/// # Errors
///
/// [`PhotoError::Invalid`] for an id that is none, else see [`PhotoError`].
pub fn show(uuid: &str) -> Result<(), PhotoError> {
    let uuid = uuid.to_uppercase();
    if !plain_uuid(&uuid) {
        return Err(PhotoError::Invalid("no picture of Photos"));
    }
    run_show(&format!("{uuid}/L0/001"))
}

#[cfg(target_os = "macos")]
fn run_show(id: &str) -> Result<(), PhotoError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(SHOW)
        .arg(id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PhotoError::Failed("Photos could not be opened".into()))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < SHOW_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PhotoError::TimedOut);
            }
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(PhotoError::Failed(
            "Photos could not show the picture; it may not allow this app yet (System Settings → Privacy & Security → \
             Automation)"
                .into(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn run_show(_id: &str) -> Result<(), PhotoError> {
    Err(PhotoError::Failed("macOS only".into()))
}

/// A name as it may be told: one line, no hidden or control characters.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !crate::calendar::invisible(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn short(text: &str) -> String {
    text.chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0506E705-B535-4E7B-8FBF-0F9A3FBF258D";
    const B: &str = "9192D28C-13A4-42EC-B6CC-682CEB501066";
    const C: &str = "91BA582A-8459-416E-B5C7-8C59AFF4EFC8";
    const D: &str = "C5DF5BBE-3F4C-42C2-A41A-E4D2FADC14E0";
    const E: &str = "DE391818-CE1A-4ECE-9FD6-98CFBC76C09C";
    /// 2026-11-08 12:00 UTC, as seconds since 1970.
    const NOON: i64 = 1_794_139_200;

    fn blob(ids: &[u32]) -> Vec<u8> {
        ids.iter().flat_map(|id| id.to_le_bytes()).collect()
    }

    /// A picture's row: pk, uuid, hours before noon, kind, subtype, favourite, hidden, trashed, visibility, detected
    /// screenshot.
    type Row = (i64, &'static str, i64, i64, i64, i64, i64, i64, i64, i64);

    /// A library and an index as Photos keeps them, cut down to what is read.
    fn stores() -> (rusqlite::Connection, rusqlite::Connection) {
        let library = rusqlite::Connection::open_in_memory().unwrap();
        library
            .execute_batch(
                "CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
                 INSERT INTO Z_PRIMARYKEY VALUES (3, 'Asset'), (34, 'Album');
                 CREATE TABLE ZASSET (Z_PK INTEGER, ZUUID TEXT, ZDATECREATED REAL, ZKIND INTEGER, ZKINDSUBTYPE INTEGER,
                    ZFAVORITE INTEGER, ZHIDDEN INTEGER, ZTRASHEDSTATE INTEGER, ZVISIBILITYSTATE INTEGER,
                    ZISDETECTEDSCREENSHOT INTEGER);
                 CREATE TABLE ZGENERICALBUM (Z_PK INTEGER, ZKIND INTEGER, ZTITLE TEXT, ZTRASHEDSTATE INTEGER);
                 CREATE TABLE Z_34ASSETS (Z_34ALBUMS INTEGER, Z_3ASSETS INTEGER);
                 INSERT INTO ZGENERICALBUM VALUES (1, 2, 'Urlaub', 0), (2, 2, 'Privat', 0), (3, 2, 'Alt', 1);
                 INSERT INTO Z_34ASSETS VALUES (1, 1), (1, 2), (2, 3), (3, 4);",
            )
            .unwrap();
        let at = |hours: i64| (NOON - APPLE_EPOCH - hours * 3600) as f64;
        let assets: [Row; 7] = [
            (1, A, 1, 0, 0, 1, 0, 0, 0, 0),
            (2, B, 2, 1, 0, 0, 0, 0, 0, 0),
            (3, C, 3, 0, 0, 0, 0, 0, 0, 0),
            (4, D, 4, 0, 10, 0, 0, 0, 0, 0),
            (5, E, 5, 0, 0, 0, 0, 0, 0, 0),
            (
                6,
                "11111111-2222-3333-4444-555555555555",
                6,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
            ),
            (
                7,
                "11111111-2222-3333-4444-666666666666",
                7,
                0,
                0,
                0,
                0,
                1,
                2,
                0,
            ),
        ];
        for (pk, uuid, hours, kind, subtype, favorite, hidden, trashed, visibility, detected) in
            assets
        {
            library
                .execute(
                    "INSERT INTO ZASSET VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        pk,
                        uuid,
                        at(hours),
                        kind,
                        subtype,
                        favorite,
                        hidden,
                        trashed,
                        visibility,
                        detected
                    ],
                )
                .unwrap();
        }
        let index = rusqlite::Connection::open_in_memory().unwrap();
        index
            .execute_batch(
                "CREATE TABLE lexicon (pk INTEGER, lexeme_id INTEGER, type INTEGER, category INTEGER, content TEXT);
                 CREATE TABLE items (pk INTEGER, identifier TEXT, type INTEGER, lexeme_ids BLOB);
                 INSERT INTO lexicon VALUES
                    (1, 10, 1, 4000, 'Strand'), (2, 10, 2, 4000, 'Strände'),
                    (3, 20, 1, 3000, 'Jonas Weber'),
                    (4, 30, 1, 2090, 'Saarbrücken'), (5, 31, 1, 2160, 'Deutschland'),
                    (6, 40, 1, 4120, 'kontonummer'),
                    (7, 50, 1, 4130, 'Reisepass'),
                    (8, 60, 1, 8050, 'IMG_1.heic');",
            )
            .unwrap();
        let items: [(&str, &[u32]); 5] = [
            (A, &[10, 20, 30, 31]),
            (B, &[10, 30]),
            (C, &[20, 40, 60]),
            (D, &[10]),
            (E, &[10, 50]),
        ];
        for (uuid, ids) in items {
            index
                .execute(
                    "INSERT INTO items (identifier, type, lexeme_ids) VALUES (?1, 1, ?2)",
                    rusqlite::params![uuid, blob(ids)],
                )
                .unwrap();
        }
        (library, index)
    }

    fn query(words: &[&str]) -> Query {
        Query {
            since: NOON - 86_400,
            until: NOON + 86_400,
            words: words.iter().map(|w| (*w).to_owned()).collect(),
            limit: MAX_READ,
            ..Query::default()
        }
    }

    fn uuids(found: &Found) -> Vec<&str> {
        found.photos.iter().map(|p| p.uuid.as_str()).collect()
    }

    #[test]
    fn hidden_trashed_documents_and_screenshots_are_never_read_unasked() {
        let (library, index) = stores();
        let found = read_from(&library, &index, &query(&[]), &[]).unwrap();
        // Newest first; D is a screenshot, E shows a passport, 6 is hidden, 7 in the trash.
        assert_eq!(uuids(&found), [A, B, C]);
        assert_eq!(found.total, 3);
        assert_eq!(found.photos[0].town.as_deref(), Some("Saarbrücken"));
        assert_eq!(found.photos[0].country.as_deref(), Some("Deutschland"));
        assert!(found.photos[0].favorite);
        assert_eq!(found.photos[1].kind, Kind::Video);
        let shots = Query {
            screenshots: true,
            ..query(&[])
        };
        let found = read_from(&library, &index, &shots, &[]).unwrap();
        assert_eq!(uuids(&found), [D]);
        assert_eq!(found.photos[0].kind, Kind::Screenshot);
    }

    #[test]
    fn words_are_found_as_photos_search_knows_them_but_never_in_the_text_of_a_picture() {
        let (library, index) = stores();
        let found = read_from(&library, &index, &query(&["strände"]), &[]).unwrap();
        assert_eq!(uuids(&found), [A, B]);
        // A word of a person's name, and two words at once.
        let found = read_from(&library, &index, &query(&["Jonas", "Strand"]), &[]).unwrap();
        assert_eq!(uuids(&found), [A]);
        // What Photos read in a picture, and its file name, are no words to look for.
        for word in ["Kontonummer", "IMG_1.heic", "Reisepass", "Mond"] {
            let found = read_from(&library, &index, &query(&[word]), &[]).unwrap();
            assert!(found.photos.is_empty(), "{word}");
            assert_eq!(found.unknown, [word.to_lowercase()]);
        }
        assert!(read_from(&library, &index, &query(&["a", "b", "c", "d", "e"]), &[]).is_err());
        assert!(read_from(&library, &index, &query(&["Str\u{200b}and"]), &[]).is_err());
    }

    #[test]
    fn a_closed_album_is_never_read_and_one_album_can_be_asked_for() {
        let (library, index) = stores();
        let found = read_from(&library, &index, &query(&[]), &["privat".into()]).unwrap();
        assert_eq!(uuids(&found), [A, B]);
        let urlaub = Query {
            album: Some("urlaub".into()),
            ..query(&[])
        };
        assert_eq!(
            uuids(&read_from(&library, &index, &urlaub, &[]).unwrap()),
            [A, B]
        );
        let favorites = Query {
            favorites: true,
            ..urlaub.clone()
        };
        assert_eq!(
            uuids(&read_from(&library, &index, &favorites, &[]).unwrap()),
            [A]
        );
        let closed = Query {
            album: Some("Privat".into()),
            ..query(&[])
        };
        assert_eq!(
            read_from(&library, &index, &closed, &["Privat".into()]),
            Err(PhotoError::AlbumClosed)
        );
        let none = Query {
            album: Some("Alt".into()),
            ..query(&[])
        };
        assert_eq!(
            read_from(&library, &index, &none, &[]).unwrap().unknown,
            ["album Alt"]
        );
        assert_eq!(albums_of(&library).unwrap(), ["Privat", "Urlaub"]);
    }

    #[test]
    fn a_picture_the_index_does_not_know_and_a_lost_closed_album_read_nothing() {
        let (library, index) = stores();
        library
            .execute(
                "INSERT INTO ZASSET VALUES (8, '22222222-2222-3333-4444-555555555555', ?1, 0, 0, 0, 0, 0, 0, 0)",
                [(NOON - APPLE_EPOCH) as f64 - 60.0],
            )
            .unwrap();
        // Taken just now and not indexed yet: it could be a passport.
        assert_eq!(
            uuids(&read_from(&library, &index, &query(&[]), &[]).unwrap()),
            [A, B, C]
        );
        // An album closed under a name Photos no longer has: renamed, it must not open again.
        assert_eq!(
            read_from(&library, &index, &query(&[]), &["Geheim".into()]),
            Err(PhotoError::AlbumClosed)
        );
        // An index that knows no picture at all is of a form not known.
        index.execute_batch("DELETE FROM items").unwrap();
        assert_eq!(
            read_from(&library, &index, &query(&[]), &[]),
            Err(PhotoError::UnknownFormat)
        );
    }

    #[test]
    fn the_limit_keeps_the_newest_and_counts_all() {
        let (library, index) = stores();
        let one = Query {
            limit: 1,
            ..query(&[])
        };
        let found = read_from(&library, &index, &one, &[]).unwrap();
        assert_eq!((uuids(&found), found.total), (vec![A], 3));
        let earlier = Query {
            until: NOON - 2 * 3600 + 1,
            ..query(&[])
        };
        assert_eq!(
            uuids(&read_from(&library, &index, &earlier, &[]).unwrap()),
            [B, C]
        );
    }

    #[test]
    fn a_store_of_another_form_is_not_guessed_at() {
        let (library, index) = stores();
        library.execute_batch("DROP TABLE Z_34ASSETS").unwrap();
        assert_eq!(
            read_from(&library, &index, &query(&[]), &[]),
            Err(PhotoError::UnknownFormat)
        );
        let (library, index) = stores();
        index
            .execute_batch("ALTER TABLE items RENAME TO things")
            .unwrap();
        assert_eq!(
            read_from(&library, &index, &query(&[]), &[]),
            Err(PhotoError::UnknownFormat)
        );
        let (library, index) = stores();
        index
            .execute(
                "UPDATE items SET lexeme_ids = x'010203' WHERE identifier = ?1",
                [A],
            )
            .unwrap();
        assert_eq!(
            read_from(&library, &index, &query(&[]), &[]),
            Err(PhotoError::UnknownFormat)
        );
    }

    #[test]
    fn a_preview_is_a_plain_jpeg_of_the_library_or_none() {
        assert!(
            plain_uuid(A) && !plain_uuid("../../etc/passwd") && !plain_uuid(&A.replace('-', "x"))
        );
        let library = std::env::temp_dir().join(format!("axiomata-photos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&library);
        let place = library.join("resources/derivatives/0");
        std::fs::create_dir_all(&place).unwrap();
        std::fs::write(
            place.join(format!("{A}_1_105_c.jpeg")),
            [0xFF, 0xD8, 0xFF, 1],
        )
        .unwrap();
        assert_eq!(
            thumbnail_in(&library, &A.to_lowercase()),
            Some(vec![0xFF, 0xD8, 0xFF, 1])
        );
        let outside = library.join("outside.jpeg");
        std::fs::write(&outside, [0xFF, 0xD8]).unwrap();
        let linked = library.join("resources/derivatives/9");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(&outside, linked.join(format!("{B}_1_105_c.jpeg"))).unwrap();
        assert_eq!(thumbnail_in(&library, B), None);
        std::fs::write(
            library.join(format!("resources/derivatives/9/{C}_1_105_c.jpeg")),
            b"<html>",
        )
        .unwrap();
        assert_eq!(thumbnail_in(&library, C), None);
        assert!(show("not-a-picture").is_err());
    }

    #[test]
    #[ignore = "reads this Mac's Photos library; run by hand"]
    fn on_this_mac() {
        let started = std::time::Instant::now();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let words: Vec<String> = std::env::var("PHOTO_WORDS")
            .map(|w| w.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let found = read(
            &Query {
                since: now - 3650 * 86_400,
                until: now,
                words,
                limit: 10,
                ..Query::default()
            },
            &[],
        )
        .unwrap();
        eprintln!(
            "{} found in {:?}, unknown {:?}",
            found.total,
            started.elapsed(),
            found.unknown
        );
        for photo in &found.photos {
            eprintln!(
                "{} {:?} {:?} {:?} thumb {}",
                photo.at,
                photo.kind,
                photo.town,
                photo.country,
                thumbnail(&photo.uuid).map_or(0, |t| t.len())
            );
        }
        eprintln!("albums: {}", albums().unwrap().len());
    }
}
