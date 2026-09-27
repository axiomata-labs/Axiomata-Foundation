use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::root::LinkPolicy;

fn temp_dir(prefix: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{seq}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn tree(files: &[(&str, &[u8])]) -> (PathBuf, Root) {
    let dir = temp_dir("axiomata-search");
    for (path, bytes) in files {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }
    let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
    (dir, root)
}

fn query(pattern: &str) -> SearchQuery {
    SearchQuery {
        pattern: pattern.into(),
        ..SearchQuery::default()
    }
}

/// Every file with matches, as `rel:line:col-end`, sorted.
fn run(root: &Root, q: &SearchQuery) -> (Vec<String>, SearchSummary) {
    let mut hits = Vec::new();
    let summary = search(root, q, &AtomicBool::new(false), |f| {
        for m in f.matches {
            hits.push(format!("{}:{}:{}-{}", f.rel, m.line, m.col, m.end));
        }
    })
    .unwrap();
    hits.sort();
    (hits, summary)
}

#[test]
fn finds_matches_by_line_leaving_out_ignored_hidden_and_binary_files() {
    let (dir, root) = tree(&[
        (
            "src/main.rs",
            b"fn main() {\n    let todo = 1; // TODO\n}\n",
        ),
        ("notes.md", b"a TODO here\n"),
        (".env", b"TODO=1"),
        ("out/log.txt", b"TODO"),
        (".gitignore", b"out/\n"),
        ("image.bin", b"TODO\0\x01"),
    ]);
    let (hits, summary) = run(&root, &query("todo"));
    assert_eq!(
        hits,
        vec![
            "notes.md:0:2-6",
            "src/main.rs:1:21-25",
            "src/main.rs:1:8-12"
        ]
    );
    assert_eq!(summary.files, 2);
    assert_eq!(summary.matches, 3);
    assert!(!summary.truncated && !summary.cancelled);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn case_whole_word_and_regex_options() {
    let (dir, root) = tree(&[("a.txt", b"Todo todos TODO\nx1 x22\n")]);
    let cased = SearchQuery {
        case_sensitive: true,
        ..query("TODO")
    };
    assert_eq!(run(&root, &cased).0, vec!["a.txt:0:11-15"]);
    let word = SearchQuery {
        whole_word: true,
        ..query("todo")
    };
    assert_eq!(run(&root, &word).0, vec!["a.txt:0:0-4", "a.txt:0:11-15"]);
    let re = SearchQuery {
        regex: true,
        ..query(r"x\d+")
    };
    assert_eq!(run(&root, &re).0, vec!["a.txt:1:0-2", "a.txt:1:3-6"]);
    // A literal query is escaped: `x\d+` itself is not found.
    assert!(run(&root, &query(r"x\d+")).0.is_empty());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn include_and_exclude_globs_narrow_the_files() {
    let (dir, root) = tree(&[
        ("src/a.rs", b"hit"),
        ("src/b.ts", b"hit"),
        ("docs/c.md", b"hit"),
    ]);
    let only_rs = SearchQuery {
        include: vec!["*.rs".into(), "*.md".into()],
        exclude: vec!["docs/**".into()],
        ..query("hit")
    };
    assert_eq!(run(&root, &only_rs).0, vec!["src/a.rs:0:0-3"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn columns_are_utf16_and_a_match_across_lines_is_cut_at_its_line() {
    let (dir, root) = tree(&[("u.md", "ä😀 key\r\nfoo\nbar\n".as_bytes())]);
    // `ä` is one unit, the emoji two: `key` starts at unit 4.
    assert_eq!(run(&root, &query("key")).0, vec!["u.md:0:4-7"]);
    let spanning = SearchQuery {
        regex: true,
        ..query(r"foo\nbar")
    };
    assert_eq!(run(&root, &spanning).0, vec!["u.md:1:0-3"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn stops_at_the_match_limit_and_when_cancelled() {
    let many = "x\n".repeat(MAX_MATCHES + 5);
    let (dir, root) = tree(&[("a.txt", many.as_bytes()), ("b.txt", b"x")]);
    let (hits, summary) = run(&root, &query("x"));
    assert_eq!(hits.len(), MAX_MATCHES);
    assert!(summary.truncated);
    let cancelled = search(&root, &query("x"), &AtomicBool::new(true), |_| {}).unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.searched, 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_bad_pattern_or_glob_is_refused() {
    let (dir, root) = tree(&[("a.txt", b"x")]);
    let never = AtomicBool::new(false);
    for q in [
        query(""),
        SearchQuery {
            regex: true,
            ..query("(")
        },
        SearchQuery {
            include: vec!["{".into()],
            ..query("x")
        },
    ] {
        let err = search(&root, &q, &never, |_| {}).unwrap_err();
        assert_eq!(err.kind(), "BadPattern");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn does_not_follow_a_link_out_of_the_root() {
    let (dir, root) = tree(&[("inside.md", b"secret? no")]);
    let outside = temp_dir("axiomata-search-outside");
    fs::write(outside.join("secret.md"), "secret").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.md"), dir.join("alias.md")).unwrap();
    assert_eq!(run(&root, &query("secret")).0, vec!["inside.md:0:0-6"]);
    let _ = fs::remove_dir_all(dir);
    let _ = fs::remove_dir_all(outside);
}

#[test]
fn a_long_line_is_shown_as_a_window_around_the_match() {
    let line = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
    let pattern = compile(&query("needle")).unwrap();
    let (found, more) = matches_in(&pattern, &line, 10);
    assert!(!more);
    let m = &found[0];
    assert_eq!((m.col, m.end), (500, 506));
    assert_eq!(m.from, 460);
    assert_eq!(m.text.encode_utf16().count(), SNIPPET_UNITS);
    assert!(m.text.contains("needle"));
}

#[test]
fn a_single_file_root_searches_its_file() {
    let (dir, _) = tree(&[("one.md", b"find me"), ("two.md", b"find me")]);
    let root = Root::single_file(&dir.join("one.md")).unwrap();
    assert_eq!(run(&root, &query("find")).0, vec!["one.md:0:0-4"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn include_globs_never_bring_back_what_the_walk_leaves_out() {
    // The ED5.7 security review: `ignore` overrides would have whitelisted these past the walk's rules.
    let (dir, root) = tree(&[
        (".env", b"SECRET=token"),
        (".git/config", b"token"),
        ("keys/secret.key", b"token"),
        (".gitignore", b"keys/\n"),
        ("notes.md", b"token"),
    ]);
    let sneaky = SearchQuery {
        include: vec![
            ".env".into(),
            "**/.git/**".into(),
            "secret.key".into(),
            "keys/**".into(),
            "*.md".into(),
        ],
        ..query("token")
    };
    assert_eq!(run(&root, &sneaky).0, vec!["notes.md:0:0-5"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_glob_without_a_slash_matches_a_name_in_any_folder() {
    let (dir, root) = tree(&[
        ("a/b/notes.md", b"x"),
        ("notes.md", b"x"),
        ("a/other.md", b"x"),
    ]);
    let named = SearchQuery {
        include: vec!["notes.md".into()],
        ..query("x")
    };
    assert_eq!(
        run(&root, &named).0,
        vec!["a/b/notes.md:0:0-1", "notes.md:0:0-1"]
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn exactly_the_limit_is_not_called_truncated() {
    let exact = "x\n".repeat(MAX_MATCHES);
    let (dir, root) = tree(&[("a.txt", exact.as_bytes()), ("b.txt", b"nothing")]);
    let (hits, summary) = run(&root, &query("x"));
    assert_eq!(hits.len(), MAX_MATCHES);
    assert!(!summary.truncated);
    let _ = fs::remove_dir_all(dir);
}
