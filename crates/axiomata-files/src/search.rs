//! Searching the files of a root (`docs/plans/editor.md`, ED5, T4, T5, T14):
//! the project search behind the file app's Search tab and the IDE's Search
//! pane.
//!
//! * **Which files**: the ones quick open lists ([`crate::index`] — no
//!   dotfiles, `node_modules`, `target` or `.gitignore`d files), narrowed by
//!   the query's include and exclude globs. The globs only ever *narrow*: they
//!   filter what the walk hands over, so an include glob cannot bring back a
//!   file the walk leaves out (`.env`, a `.gitignore`d key). Binary files (not
//!   UTF-8, or with a NUL) and files over [`MAX_READ_BYTES`] are skipped.
//! * **How they are read**: through [`read_text`], the editor's own guarded
//!   read — every file is resolved against the root and opened from a pinned
//!   parent directory, so a search cannot be redirected out of the root any
//!   more than an open can.
//! * **The pattern** is the `regex` crate's: no backtracking, so a search
//!   takes time in proportion to the text and needs no time limit (T5). A
//!   plain query is escaped; "whole word" wraps it in `\b…\b`. A pattern with
//!   a line break may match across lines; a match is reported at the line it
//!   starts on.
//! * **Results stream** file by file to the caller as they are found, and a
//!   search stops as soon as its `cancel` flag is set (a newer query) or
//!   [`MAX_MATCHES`] matches were found.
//!
//! Columns are UTF-16 code units, like everywhere in the editor.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::FilesError;
use crate::file::{MAX_READ_BYTES, read_text};
use crate::index::walk_files;
use crate::root::Root;

/// Matches one search reports at most; the rest is left out (`truncated`).
pub const MAX_MATCHES: usize = 10_000;

/// A line longer than this many UTF-16 units is shown as a window around the match.
pub const SNIPPET_UNITS: usize = 240;

/// How much of the line before a match a shortened snippet keeps.
const SNIPPET_LEAD: usize = 40;

/// Size limits for a compiled pattern: a pathological pattern is refused, not built.
const PATTERN_SIZE_LIMIT: usize = 16 * 1024 * 1024;

/// What to search for, as the search view sends it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub pattern: String,
    /// `pattern` is a regular expression; otherwise it is taken literally.
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    /// Only files matching one of these globs (none: every file).
    #[serde(default)]
    pub include: Vec<String>,
    /// No file matching one of these globs.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One match, on the line it starts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineMatch {
    /// Zero-based line.
    pub line: u32,
    /// The match on that line, `[col, end)` in UTF-16 units; `end` is the
    /// line's end when the match goes on past it.
    pub col: u32,
    pub end: u32,
    /// The line (or a window of it, see [`SNIPPET_UNITS`]) to show.
    pub text: String,
    /// The UTF-16 column `text` starts at in the line.
    pub from: u32,
}

/// The matches in one file, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileMatches {
    pub rel: String,
    pub matches: Vec<LineMatch>,
}

/// How a search went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SearchSummary {
    /// Files with at least one match, and matches in all of them.
    pub files: usize,
    pub matches: usize,
    /// Files looked into.
    pub searched: usize,
    /// Stopped at [`MAX_MATCHES`], or the walk hit its own limits.
    pub truncated: bool,
    /// Stopped by a newer query.
    pub cancelled: bool,
}

/// The query's pattern as a regex.
///
/// Errors:
///     [`FilesError::BadPattern`] for an empty pattern, or one that does not
///     compile (or would compile too large).
pub fn compile(query: &SearchQuery) -> Result<Regex, FilesError> {
    if query.pattern.is_empty() {
        return Err(FilesError::BadPattern("the pattern is empty".into()));
    }
    let body = if query.regex {
        query.pattern.clone()
    } else {
        regex::escape(&query.pattern)
    };
    let body = if query.whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };
    RegexBuilder::new(&body)
        .case_insensitive(!query.case_sensitive)
        .multi_line(true)
        .size_limit(PATTERN_SIZE_LIMIT)
        .dfa_size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|err| FilesError::BadPattern(err.to_string()))
}

/// The include or exclude globs as one set (`None`: no globs given). A glob
/// without a `/` matches a file name in any folder (`*.rs`, `notes.md`), as in
/// a `.gitignore`; one with a `/` matches the path from the root.
fn glob_set(globs: &[String]) -> Result<Option<GlobSet>, FilesError> {
    let globs: Vec<&str> = globs
        .iter()
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .collect();
    if globs.is_empty() {
        return Ok(None);
    }
    // Named as a glob, so the view does not blame the search pattern; the error's kind carries no path.
    let bad = |glob: &str, err: globset::Error| {
        FilesError::BadPattern(format!("include/exclude glob `{glob}`: {}", err.kind()))
    };
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        let glob = glob.trim_start_matches('/');
        builder.add(Glob::new(glob).map_err(|e| bad(glob, e))?);
        if !glob.contains('/') {
            builder.add(Glob::new(&format!("**/{glob}")).map_err(|e| bad(glob, e))?);
        }
    }
    builder.build().map(Some).map_err(|e| bad("", e))
}

/// Whether `rel` passes the include and exclude globs.
fn wanted(rel: &str, include: Option<&GlobSet>, exclude: Option<&GlobSet>) -> bool {
    include.is_none_or(|set| set.is_match(rel)) && !exclude.is_some_and(|set| set.is_match(rel))
}

/// Searches `root` for `query`, handing each file's matches to `found` as it
/// is done, until the walk ends, `cancel` is set or [`MAX_MATCHES`] is reached.
///
/// Errors:
///     [`FilesError::BadPattern`] for a pattern or glob that does not compile.
///     Files that cannot be read (gone, binary, too large) are skipped.
pub fn search(
    root: &Root,
    query: &SearchQuery,
    cancel: &AtomicBool,
    mut found: impl FnMut(FileMatches),
) -> Result<SearchSummary, FilesError> {
    let pattern = compile(query)?;
    let include = glob_set(&query.include)?;
    let exclude = glob_set(&query.exclude)?;
    let mut summary = SearchSummary::default();
    let mut look = |rel: &str, summary: &mut SearchSummary| -> ControlFlow<()> {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            return ControlFlow::Break(());
        }
        let Ok(file) = read_text(root, rel, MAX_READ_BYTES) else {
            return ControlFlow::Continue(());
        };
        summary.searched += 1;
        let room = MAX_MATCHES - summary.matches;
        let (matches, more) = matches_in(&pattern, &file.content, room);
        if !matches.is_empty() {
            summary.files += 1;
            summary.matches += matches.len();
            found(FileMatches {
                rel: rel.to_owned(),
                matches,
            });
        }
        // At the limit the walk goes on with no room: only a further match proves something was left out.
        if more {
            summary.truncated = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    };
    if let Some(only) = root.only_file() {
        if let Some(rel) = only.to_str() {
            let _ = look(rel, &mut summary);
        }
        return Ok(summary);
    }
    let stopped = walk_files(root, |rel| {
        if wanted(rel, include.as_ref(), exclude.as_ref()) {
            look(rel, &mut summary)
        } else {
            ControlFlow::Continue(())
        }
    });
    if stopped && !summary.cancelled {
        summary.truncated = true;
    }
    Ok(summary)
}

/// The matches of `pattern` in `text`, at most `room` of them, and whether
/// there were more. Empty matches (`^`, `\b`) are skipped.
pub fn matches_in(pattern: &Regex, text: &str, room: usize) -> (Vec<LineMatch>, bool) {
    let mut out = Vec::new();
    // Where the line holding the last match begins, and its number — matches come in order.
    let mut line_start = 0;
    let mut line_no = 0u32;
    for m in pattern.find_iter(text) {
        if m.start() == m.end() {
            continue;
        }
        if out.len() >= room {
            return (out, true);
        }
        let before = &text[line_start..m.start()];
        if let Some(last_break) = before.rfind('\n') {
            line_no += before.matches('\n').count() as u32;
            line_start += last_break + 1;
        }
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |i| line_start + i);
        let line = text[line_start..line_end].trim_end_matches('\r');
        let col = utf16_len(&text[line_start..m.start()]);
        let end = utf16_len(&text[line_start..m.end().min(line_start + line.len())]);
        let (snippet, from) = snippet(line, col);
        out.push(LineMatch {
            line: line_no,
            col,
            end: end.max(col),
            text: snippet,
            from,
        });
    }
    (out, false)
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// `line` whole, or for a long one a window of [`SNIPPET_UNITS`] starting a
/// little before `col` — with the column the window starts at.
fn snippet(line: &str, col: u32) -> (String, u32) {
    if utf16_len(line) as usize <= SNIPPET_UNITS {
        return (line.to_owned(), 0);
    }
    let from = (col as usize).saturating_sub(SNIPPET_LEAD);
    let mut units = 0usize;
    let mut out = String::new();
    let mut start = None;
    for ch in line.chars() {
        let width = ch.len_utf16();
        if units >= from && units - from + width > SNIPPET_UNITS {
            break;
        }
        if units >= from {
            start.get_or_insert(units);
            out.push(ch);
        }
        units += width;
    }
    (out, start.unwrap_or(from) as u32)
}

#[cfg(test)]
mod tests;
