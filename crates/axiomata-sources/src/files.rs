//! The owner's files (Lirvel `docs/plans/companion.md` §8j S2c, Q17): found through Spotlight (`mdfind`), read as text —
//! plain text, Markdown, code and PDFs. Everything goes through the [`Fence`]: a found file it closes is dropped before
//! anyone sees its name, and a file is read only after the fence judged the place its opened descriptor really is.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::fence::Fence;

/// The most files one search returns.
pub const MAX_FOUND: usize = 50;
/// The largest file read: more is no note, and a PDF of that size is a book.
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
/// The most characters of text one read returns.
pub const MAX_TEXT_CHARS: usize = 200_000;
/// The most pages of a PDF read.
#[cfg(target_os = "macos")]
const MAX_PDF_PAGES: usize = 300;
/// The most found paths judged on the disk: past them a search is too wide to be told anyway.
const MAX_JUDGED: usize = 2000;
/// How long Spotlight gets.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest query taken.
const MAX_QUERY_CHARS: usize = 200;

/// A file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    /// Last changed, in seconds since 1970.
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    /// The fence closes it, or it is not there: the two are not told apart, so a closed place is not seen.
    Closed,
    /// No text could be read from it (an image, a program, a scanned PDF).
    NoText,
    /// Larger than [`MAX_READ_BYTES`].
    TooLarge,
    /// Spotlight did not answer in time.
    TimedOut,
    /// A question that is none.
    Invalid(&'static str),
    Failed(String),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "there is no such file, or it may not be read"),
            Self::NoText => write!(f, "no text can be read from it"),
            Self::TooLarge => write!(f, "the file is too large"),
            Self::TimedOut => write!(f, "the search took too long"),
            Self::Invalid(why) => write!(f, "{why}"),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for FileError {}

/// Searches the home folder through Spotlight for `query` — names and contents — newest first, at most `limit`, only
/// what `fence` allows.
///
/// # Errors
///
/// [`FileError::Invalid`] for an empty or overlong query, [`FileError::TimedOut`] when Spotlight takes too long.
pub fn search(query: &str, fence: &Fence, limit: usize) -> Result<Vec<Found>, FileError> {
    let query = query.trim();
    // A leading "-" would be one of Spotlight's options.
    if query.is_empty()
        || query.starts_with('-')
        || query.chars().count() > MAX_QUERY_CHARS
        || query.chars().any(char::is_control)
    {
        return Err(FileError::Invalid("say what to look for, in a few words"));
    }
    let home = std::env::home_dir().ok_or(FileError::Failed("no home folder".into()))?;
    // The words as one argument, never through a shell; `-onlyin` keeps Spotlight inside the home folder.
    let mut child = Command::new("/usr/bin/mdfind")
        .arg("-onlyin")
        .arg(&home)
        .arg(query)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| FileError::Failed(format!("Spotlight did not start: {err}")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(FileError::Failed("no output".into()))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        // At most a few MB of paths: more is a search far too wide.
        let _ = (&mut stdout).take(4 * 1024 * 1024).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SEARCH_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FileError::TimedOut);
            }
        }
    }
    let out = reader.join().unwrap_or_default();
    let mut found: Vec<Found> = String::from_utf8_lossy(&out)
        .lines()
        .map(PathBuf::from)
        // A name with a hidden or invisible character is no name the owner gave a file to find: it could forge
        // lines in what she is told.
        .filter(|path| plain_name(path))
        // First what needs no disk: the fence as written, and the build output a search of a developer's home
        // folder is mostly made of.
        .filter(|path| !fence.closes(path) && !built(path))
        .take(MAX_JUDGED)
        .filter(|path| fence.allows(path))
        .filter_map(|path| {
            let meta = std::fs::metadata(&path)
                .ok()
                .filter(std::fs::Metadata::is_file)?;
            let modified = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|at| at.as_secs());
            Some(Found { path, modified })
        })
        .collect();
    found.sort_by_key(|found| std::cmp::Reverse(found.modified));
    found.truncate(limit.min(MAX_FOUND));
    Ok(found)
}

/// The text of the file at `path`: plain text, Markdown, code, or a PDF's text. The fence judges the path, and again
/// the place the opened file really is — a link swapped in between does not lead into a closed place.
///
/// # Errors
///
/// [`FileError::Closed`] for a closed or missing file, [`FileError::NoText`] for a file without readable text,
/// [`FileError::TooLarge`] beyond [`MAX_READ_BYTES`].
pub fn read_text(path: &Path, fence: &Fence) -> Result<String, FileError> {
    let bytes = read_fenced(path, fence)?;
    let pdf = path
        .extension()
        .is_some_and(|ending| ending.eq_ignore_ascii_case("pdf"));
    let text = if pdf {
        pdf_text(&bytes).ok_or(FileError::NoText)?
    } else {
        plain_text(&bytes).ok_or(FileError::NoText)?
    };
    let text: String = text
        .chars()
        .filter(|c| !crate::calendar::invisible(*c))
        .take(MAX_TEXT_CHARS)
        .collect();
    if text.trim().is_empty() {
        return Err(FileError::NoText);
    }
    Ok(text)
}

/// Folders of build output and fetched packages: thousands of files nobody looks for by name.
const BUILT: [&str; 4] = ["target", "node_modules", "DerivedData", "__pycache__"];

/// Whether `path` lies in build output.
fn built(path: &Path) -> bool {
    path.components()
        .any(|part| BUILT.iter().any(|built| part.as_os_str() == *built))
}

/// Whether every part of `path` is free of control and invisible characters.
fn plain_name(path: &Path) -> bool {
    !path
        .to_string_lossy()
        .chars()
        .any(|c| c.is_control() || crate::calendar::invisible(c))
}

/// The place `path` really is, when it is a plain file the fence allows: opened without following a link at its
/// end, judged where the open file really is. Hand that place on — never `path` — so a link swapped in after the
/// check leads nowhere new.
///
/// # Errors
///
/// [`FileError::Closed`] for a closed, missing or special file.
pub fn real_file(path: &Path, fence: &Fence) -> Result<PathBuf, FileError> {
    let (file, real) = open_fenced(path, fence)?;
    drop(file);
    Ok(real)
}

/// Reads `path` when it is a plain file the fence allows, judged where the open file really is.
fn read_fenced(path: &Path, fence: &Fence) -> Result<Vec<u8>, FileError> {
    let (file, _) = open_fenced(path, fence)?;
    let meta = file.metadata().map_err(|_| FileError::Closed)?;
    if meta.len() > MAX_READ_BYTES {
        return Err(FileError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|err| FileError::Failed(err.to_string()))?;
    Ok(bytes)
}

/// `path` opened for reading without following a link at its end, and the place it really is — when that is a
/// plain file the fence allows.
fn open_fenced(path: &Path, fence: &Fence) -> Result<(std::fs::File, PathBuf), FileError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    if !plain_name(path) || !fence.allows(path) {
        return Err(FileError::Closed);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits() as i32 | rustix::fs::OFlags::NONBLOCK.bits() as i32,
        )
        .open(path)
        .map_err(|_| FileError::Closed)?;
    let real = real_path(&file).ok_or(FileError::Closed)?;
    let meta = file.metadata().map_err(|_| FileError::Closed)?;
    if !meta.is_file() || !fence.allows(&real) {
        return Err(FileError::Closed);
    }
    Ok((file, real))
}

/// Where the open `file` really is, as the kernel says.
#[cfg(target_os = "macos")]
fn real_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;

    let path = rustix::fs::getpath(file).ok()?;
    Some(PathBuf::from(std::ffi::OsString::from_vec(
        path.into_bytes(),
    )))
}

#[cfg(not(target_os = "macos"))]
fn real_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd as _;

    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

/// `bytes` as text when they are text: UTF-8 without NUL bytes (a NUL is a binary file's mark).
fn plain_text(bytes: &[u8]) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// The text of a PDF, through PDFKit.
#[cfg(target_os = "macos")]
fn pdf_text(bytes: &[u8]) -> Option<String> {
    use objc2::AllocAnyThread as _;
    use objc2_foundation::NSData;
    use objc2_pdf_kit::PDFDocument;

    let data = NSData::with_bytes(bytes);
    // SAFETY: PDFKit reads the bytes it was handed; the document and its pages live on this thread only.
    unsafe {
        let document = PDFDocument::initWithData(PDFDocument::alloc(), &data)?;
        // Page by page, and only as far as the text taken reaches: a book of a PDF is not read to its end.
        let mut text = String::new();
        for index in 0..document.pageCount().min(MAX_PDF_PAGES) {
            if text.len() > MAX_TEXT_CHARS * 4 {
                break;
            }
            if let Some(page) = document.pageAtIndex(index)
                && let Some(words) = page.string()
            {
                text.push_str(&words.to_string());
                text.push('\n');
            }
        }
        Some(text)
    }
}

#[cfg(not(target_os = "macos"))]
fn pdf_text(_bytes: &[u8]) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("axiomata-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("Documents/Privat")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(
            home.join("Documents/note.md"),
            "# Urlaub\nWir fahren im Mai nach Lissabon.",
        )
        .unwrap();
        std::fs::write(home.join("Documents/Privat/diary.md"), "geheim").unwrap();
        std::fs::write(home.join(".ssh/id_ed25519"), "key").unwrap();
        std::fs::write(
            home.join("Documents/bild.png"),
            [0x89, b'P', b'N', b'G', 0, 1, 2],
        )
        .unwrap();
        std::fs::canonicalize(home).unwrap()
    }

    #[test]
    fn text_is_read_only_where_the_fence_allows() {
        let home = home("read");
        let fence = Fence::new(&home, [home.join("Documents/Privat")]);
        assert_eq!(
            read_text(&home.join("Documents/note.md"), &fence).unwrap(),
            "# Urlaub\nWir fahren im Mai nach Lissabon."
        );
        assert_eq!(
            read_text(&home.join("Documents/Privat/diary.md"), &fence),
            Err(FileError::Closed)
        );
        assert_eq!(
            read_text(&home.join(".ssh/id_ed25519"), &fence),
            Err(FileError::Closed)
        );
        assert_eq!(
            read_text(&home.join("Documents/gone.md"), &fence),
            Err(FileError::Closed)
        );
        assert_eq!(
            read_text(&home.join("Documents/bild.png"), &fence),
            Err(FileError::NoText)
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_link_at_the_end_is_not_followed_even_into_an_open_place() {
        let home = home("link");
        let fence = Fence::new(&home, [home.join("Documents/Privat")]);
        std::os::unix::fs::symlink(
            home.join("Documents/Privat/diary.md"),
            home.join("Documents/way.md"),
        )
        .unwrap();
        assert_eq!(
            read_text(&home.join("Documents/way.md"), &fence),
            Err(FileError::Closed)
        );
        std::os::unix::fs::symlink(
            home.join("Documents/note.md"),
            home.join("Documents/also.md"),
        )
        .unwrap();
        assert_eq!(
            read_text(&home.join("Documents/also.md"), &fence),
            Err(FileError::Closed)
        );
        assert_eq!(
            real_file(&home.join("Documents/also.md"), &fence),
            Err(FileError::Closed)
        );
        assert_eq!(
            real_file(&home.join("Documents/note.md"), &fence),
            Ok(home.join("Documents/note.md"))
        );
        std::fs::write(home.join("Documents/bad\u{202E}dm.exe"), "x").unwrap();
        assert_eq!(
            real_file(&home.join("Documents/bad\u{202E}dm.exe"), &fence),
            Err(FileError::Closed)
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn build_output_is_no_find() {
        assert!(built(Path::new(
            "/Users/x/Dev/Lirvel/target/debug/deps/a.d"
        )));
        assert!(built(Path::new("/Users/x/app/node_modules/x/README.md")));
        assert!(!built(Path::new("/Users/x/Documents/targets.md")));
    }

    #[test]
    fn a_question_that_is_none_is_refused() {
        let fence = Fence::new(Path::new("/nowhere"), []);
        assert!(matches!(
            search("  ", &fence, 5),
            Err(FileError::Invalid(_))
        ));
        assert!(matches!(
            search(&"x".repeat(300), &fence, 5),
            Err(FileError::Invalid(_))
        ));
        assert!(matches!(
            search("a\nb", &fence, 5),
            Err(FileError::Invalid(_))
        ));
        assert!(matches!(
            search("-live", &fence, 5),
            Err(FileError::Invalid(_))
        ));
    }

    #[test]
    fn invisible_characters_leave_the_text() {
        let home = home("invisible");
        let fence = Fence::new(&home, []);
        std::fs::write(
            home.join("Documents/hidden.md"),
            "Pass\u{200B}wort\u{E0041}",
        )
        .unwrap();
        assert_eq!(
            read_text(&home.join("Documents/hidden.md"), &fence).unwrap(),
            "Passwort"
        );
        let _ = std::fs::remove_dir_all(home);
    }
}

#[cfg(test)]
mod on_this_mac {
    use super::*;

    /// Searches this Mac's home folder: run by hand (`cargo test -p axiomata-sources on_this_mac -- --ignored
    /// --nocapture`).
    #[test]
    #[ignore = "searches the owner's files"]
    fn a_search_in_this_home_folder() {
        let fence = Fence::for_home([]).unwrap();
        let started = Instant::now();
        let found = search("Lirvel", &fence, 10).unwrap();
        println!("{} found in {:?}", found.len(), started.elapsed());
        for file in &found {
            println!("{}", file.path.display());
        }
    }
}
