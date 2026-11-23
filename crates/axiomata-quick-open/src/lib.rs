//! Quick open (Axiomata-OS `docs/plans/rust-core.md` §3e, S1–S7; the rules come from `docs/plans/editor.md` ED4, W8,
//! W14): a typed query against every file's path, fuzzily — the letters in order, not necessarily together — and
//! ranked the way one expects:
//!
//! * a match inside the **file name** beats one spread over the folders;
//! * letters at a **word start** (after `/ . _ -`, a space, or a capital in camelCase) and **runs** of consecutive
//!   letters count more;
//! * **shorter** paths win a tie, and **recently opened** files come first among equals;
//! * `name:12` also asks for line 12 ([`parse_query`]).
//!
//! The paths come from [`Indexes`]: one list of files per root, served at once and read again in the background once
//! it is older than [`INDEX_FRESH`], or at once after the app itself changed files under that root. The crate walks no
//! folder itself — the caller reads a root and hands the list in.

#![forbid(unsafe_code)]

mod indexes;
mod query;
mod rank;

pub use indexes::{INDEX_FRESH, Indexes, MAX_INDEXES, Snapshot};
pub use query::{Query, parse_query};
pub use rank::{Candidate, Matcher, PathMatch, Ranked, rank, utf16_offsets};
