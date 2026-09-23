//! Tests for the guard and the file operations, on real temporary
//! directories — the guard is only as good as its behaviour on an actual
//! file system with actual links.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::root::LinkPolicy;

/// A fresh directory under the system temp dir, unique per call.
fn temp_dir(prefix: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{seq}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A root with `notes/inbox.md` in it, plus a sibling directory outside it.
struct Fixture {
    dir: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = temp_dir("axiomata-files-root");
        fs::create_dir_all(dir.join("notes")).unwrap();
        fs::write(dir.join("notes/inbox.md"), "# Inbox\n").unwrap();
        let outside = temp_dir("axiomata-files-outside");
        fs::write(outside.join("secret.md"), "secret").unwrap();
        Self { dir, outside }
    }

    fn root(&self, policy: LinkPolicy) -> Root {
        Root::dir(&self.dir, policy).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        let _ = fs::remove_dir_all(&self.outside);
    }
}

fn kind<T: std::fmt::Debug>(result: Result<T, FilesError>) -> &'static str {
    result.expect_err("expected an error").kind()
}

#[test]
fn reads_text_with_a_content_version() {
    let fx = Fixture::new();
    let root = fx.root(LinkPolicy::Strict);
    let file = read_text(&root, "notes/inbox.md", MAX_READ_BYTES).unwrap();
    assert_eq!(file.rel, "notes/inbox.md");
    assert_eq!(file.content, "# Inbox\n");
    assert_eq!(file.version, Version::of(b"# Inbox\n"));
    assert!(file.modified.is_some());
    assert!(!file.large);
    assert_eq!(
        kind(read_text(&root, "notes/missing.md", MAX_READ_BYTES)),
        "NotFound"
    );
}

#[test]
fn a_version_follows_the_content_not_the_mtime() {
    assert_eq!(Version::of(b"abc"), Version::of(b"abc"));
    assert_ne!(Version::of(b"abc"), Version::of(b"abd"));
    assert_ne!(Version::of(b""), Version::of(b"\0"));
    // Pinned: the textual form is persisted (recovery files), so it must
    // not drift between builds.
    assert_eq!(Version::of(b"").as_str(), "0-cbf29ce484222325");
    assert_eq!(Version::of(b"a").as_str(), "1-af63dc4c8601ec8c");
}

#[test]
fn flags_a_large_file_and_refuses_one_over_the_limit() {
    let fx = Fixture::new();
    let root = fx.root(LinkPolicy::Contained);
    let big = "x".repeat(LARGE_FILE_BYTES as usize + 1);
    fs::write(fx.dir.join("big.txt"), &big).unwrap();
    assert!(read_text(&root, "big.txt", MAX_READ_BYTES).unwrap().large);
    assert_eq!(kind(read_text(&root, "big.txt", 1024)), "TooLarge");
    assert_eq!(
        kind(write_text(&root, "new.txt", &big, None, MAX_WRITE_BYTES)),
        "TooLarge"
    );
    assert!(!fx.dir.join("new.txt").exists());
}

#[test]
fn refuses_binary_content_as_not_utf8() {
    let fx = Fixture::new();
    fs::write(fx.dir.join("blob.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let root = fx.root(LinkPolicy::Strict);
    assert_eq!(
        kind(read_text(&root, "blob.bin", MAX_READ_BYTES)),
        "NotUtf8"
    );
    // The version does not care about encoding.
    assert!(current_version(&root, "blob.bin").unwrap().is_some());
}

#[test]
fn writes_with_a_matching_version_and_refuses_a_stale_one() {
    let fx = Fixture::new();
    let root = fx.root(LinkPolicy::Strict);
    let read = read_text(&root, "notes/inbox.md", MAX_READ_BYTES).unwrap();

    let v2 = write_text(
        &root,
        "notes/inbox.md",
        "mine",
        Some(&read.version),
        MAX_WRITE_BYTES,
    )
    .unwrap();
    assert_eq!(v2, Version::of(b"mine"));

    // Someone else writes; the old version is now stale.
    fs::write(fx.dir.join("notes/inbox.md"), "agent").unwrap();
    let err = write_text(
        &root,
        "notes/inbox.md",
        "mine again",
        Some(&v2),
        MAX_WRITE_BYTES,
    );
    assert_eq!(kind(err), "Conflict");
    assert_eq!(
        fs::read_to_string(fx.dir.join("notes/inbox.md")).unwrap(),
        "agent"
    );

    // Expecting a version of a file that has vanished is a conflict too.
    fs::remove_file(fx.dir.join("notes/inbox.md")).unwrap();
    assert_eq!(
        kind(write_text(
            &root,
            "notes/inbox.md",
            "x",
            Some(&v2),
            MAX_WRITE_BYTES
        )),
        "Conflict"
    );
    assert!(!fx.dir.join("notes/inbox.md").exists());

    // A blind write creates it, and no temp file is left behind.
    write_text(&root, "notes/inbox.md", "fresh", None, MAX_WRITE_BYTES).unwrap();
    assert_eq!(
        fs::read_to_string(fx.dir.join("notes/inbox.md")).unwrap(),
        "fresh"
    );
    assert!(!fx.dir.join("notes/.inbox.md.axiomata-tmp").exists());
}

#[cfg(unix)]
#[test]
fn a_write_keeps_the_permission_bits() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let script = fx.dir.join("run.sh");
    fs::write(&script, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    write_text(
        &fx.root(LinkPolicy::Contained),
        "run.sh",
        "#!/bin/sh\necho hi\n",
        None,
        MAX_WRITE_BYTES,
    )
    .unwrap();
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn refuses_climbing_out_absolute_empty_and_directory_paths() {
    let fx = Fixture::new();
    for policy in [LinkPolicy::Strict, LinkPolicy::Contained] {
        let root = fx.root(policy);
        for rel in [
            "../x.md",
            "notes/../../x.md",
            "/etc/hosts",
            "",
            "  ",
            "notes",
            ".",
        ] {
            assert_eq!(
                kind(read_text(&root, rel, MAX_READ_BYTES)),
                "Refused",
                "{policy:?} {rel:?}"
            );
            assert_eq!(
                kind(write_text(&root, rel, "x", None, MAX_WRITE_BYTES)),
                "Refused",
                "{policy:?} {rel:?}"
            );
        }
    }
    // A missing parent is an I/O error, not a silent `create_dir_all`.
    let root = fx.root(LinkPolicy::Strict);
    assert_eq!(
        kind(write_text(
            &root,
            "no/such/dir.md",
            "x",
            None,
            MAX_WRITE_BYTES
        )),
        "Io"
    );
}

#[cfg(unix)]
#[test]
fn refuses_a_fifo_instead_of_blocking_on_it() {
    let fx = Fixture::new();
    let status = std::process::Command::new("mkfifo")
        .arg(fx.dir.join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());
    let root = fx.root(LinkPolicy::Contained);
    assert_eq!(kind(read_text(&root, "pipe", MAX_READ_BYTES)), "Refused");
}

#[cfg(unix)]
#[test]
fn strict_refuses_every_symlink_and_hard_link() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    symlink(fx.dir.join("notes/inbox.md"), fx.dir.join("inner.md")).unwrap();
    symlink(fx.outside.join("secret.md"), fx.dir.join("outer.md")).unwrap();
    symlink(&fx.outside, fx.dir.join("dir-link")).unwrap();
    fs::hard_link(fx.outside.join("secret.md"), fx.dir.join("notes/linked.md")).unwrap();

    let root = fx.root(LinkPolicy::Strict);
    for rel in [
        "inner.md",
        "outer.md",
        "dir-link/secret.md",
        "notes/linked.md",
    ] {
        assert_eq!(
            kind(read_text(&root, rel, MAX_READ_BYTES)),
            "Refused",
            "{rel}"
        );
        assert_eq!(
            kind(write_text(&root, rel, "x", None, MAX_WRITE_BYTES)),
            "Refused",
            "{rel}"
        );
        assert_eq!(kind(delete(&root, rel)), "Refused", "{rel}");
    }
    assert_eq!(
        fs::read_to_string(fx.outside.join("secret.md")).unwrap(),
        "secret"
    );
}

#[cfg(unix)]
#[test]
fn contained_follows_an_inner_symlink_and_writes_through_to_its_target() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    symlink("notes/inbox.md", fx.dir.join("CLAUDE.md")).unwrap();
    let root = fx.root(LinkPolicy::Contained);

    assert_eq!(
        read_text(&root, "CLAUDE.md", MAX_READ_BYTES)
            .unwrap()
            .content,
        "# Inbox\n"
    );
    write_text(
        &root,
        "CLAUDE.md",
        "through the link",
        None,
        MAX_WRITE_BYTES,
    )
    .unwrap();
    assert!(
        fs::symlink_metadata(fx.dir.join("CLAUDE.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(fx.dir.join("notes/inbox.md")).unwrap(),
        "through the link"
    );

    // Deleting the link removes the link, not what it points to.
    delete(&root, "CLAUDE.md").unwrap();
    assert!(fs::symlink_metadata(fx.dir.join("CLAUDE.md")).is_err());
    assert!(fx.dir.join("notes/inbox.md").exists());
}

#[cfg(unix)]
#[test]
fn contained_refuses_links_that_leave_the_root_or_dangle() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    symlink(fx.outside.join("secret.md"), fx.dir.join("outer.md")).unwrap();
    symlink(&fx.outside, fx.dir.join("dir-link")).unwrap();
    symlink(fx.dir.join("gone.md"), fx.dir.join("dangling.md")).unwrap();
    symlink(fx.dir.join("notes"), fx.dir.join("notes-link")).unwrap();

    let root = fx.root(LinkPolicy::Contained);
    for rel in [
        "outer.md",
        "dir-link/secret.md",
        "dir-link/new.md",
        "dangling.md",
        "notes-link",
    ] {
        assert_eq!(
            kind(read_text(&root, rel, MAX_READ_BYTES)),
            "Refused",
            "{rel}"
        );
        assert_eq!(
            kind(write_text(&root, rel, "x", None, MAX_WRITE_BYTES)),
            "Refused",
            "{rel}"
        );
    }
    assert!(!fx.dir.join("gone.md").exists());
    assert!(!fx.outside.join("new.md").exists());
    assert_eq!(
        fs::read_to_string(fx.outside.join("secret.md")).unwrap(),
        "secret"
    );

    // A symlinked directory that stays inside is fine.
    assert_eq!(
        read_text(&root, "notes-link/inbox.md", MAX_READ_BYTES)
            .unwrap()
            .content,
        "# Inbox\n"
    );
}

#[cfg(unix)]
#[test]
fn contained_allows_a_hard_link() {
    let fx = Fixture::new();
    fs::hard_link(fx.dir.join("notes/inbox.md"), fx.dir.join("store-copy.md")).unwrap();
    let root = fx.root(LinkPolicy::Contained);
    assert_eq!(
        read_text(&root, "store-copy.md", MAX_READ_BYTES)
            .unwrap()
            .content,
        "# Inbox\n"
    );
    // The atomic rename replaces the directory entry, so the other name
    // (a package store's file) keeps its content.
    write_text(&root, "store-copy.md", "edited", None, MAX_WRITE_BYTES).unwrap();
    assert_eq!(
        fs::read_to_string(fx.dir.join("notes/inbox.md")).unwrap(),
        "# Inbox\n"
    );
}

#[cfg(unix)]
#[test]
fn never_follows_a_planted_temp_symlink() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    symlink(
        fx.outside.join("secret.md"),
        fx.dir.join("notes/.inbox.md.axiomata-tmp"),
    )
    .unwrap();
    let root = fx.root(LinkPolicy::Contained);
    assert_eq!(
        kind(write_text(
            &root,
            "notes/inbox.md",
            "clobber",
            None,
            MAX_WRITE_BYTES
        )),
        "Io"
    );
    assert_eq!(
        fs::read_to_string(fx.outside.join("secret.md")).unwrap(),
        "secret"
    );
    assert_eq!(
        fs::read_to_string(fx.dir.join("notes/inbox.md")).unwrap(),
        "# Inbox\n"
    );
    // The planted link is not ours to remove.
    assert!(fs::symlink_metadata(fx.dir.join("notes/.inbox.md.axiomata-tmp")).is_ok());
}

#[test]
fn a_single_file_root_gives_out_that_file_only() {
    let fx = Fixture::new();
    fs::write(fx.dir.join("notes/other.md"), "other").unwrap();
    let root = Root::single_file(&fx.dir.join("notes/inbox.md")).unwrap();
    assert_eq!(root.only_file(), Some(std::path::Path::new("inbox.md")));

    assert_eq!(
        read_text(&root, "inbox.md", MAX_READ_BYTES)
            .unwrap()
            .content,
        "# Inbox\n"
    );
    assert_eq!(
        read_text(&root, "./inbox.md", MAX_READ_BYTES)
            .unwrap()
            .content,
        "# Inbox\n"
    );
    write_text(&root, "inbox.md", "saved", None, MAX_WRITE_BYTES).unwrap();
    for rel in ["other.md", "../notes/inbox.md", "sub/inbox.md", "new.md"] {
        assert_eq!(
            kind(read_text(&root, rel, MAX_READ_BYTES)),
            "Refused",
            "{rel}"
        );
        assert_eq!(
            kind(write_text(&root, rel, "x", None, MAX_WRITE_BYTES)),
            "Refused",
            "{rel}"
        );
    }
    assert_eq!(kind(Root::single_file(&fx.dir.join("notes"))), "Refused");
}

#[test]
fn a_directory_root_must_be_a_directory() {
    let fx = Fixture::new();
    assert_eq!(
        kind(Root::dir(
            &fx.dir.join("notes/inbox.md"),
            LinkPolicy::Strict
        )),
        "Refused"
    );
    assert_eq!(
        kind(Root::dir(&fx.dir.join("missing"), LinkPolicy::Strict)),
        "Io"
    );
    assert_eq!(
        kind(Root::dir(std::path::Path::new("/"), LinkPolicy::Contained)),
        "Refused"
    );
}

#[test]
fn creates_one_new_top_level_directory_but_no_deeper_chain() {
    let fx = Fixture::new();
    let root = fx.root(LinkPolicy::Strict);
    ensure_top_level_dir(&root, "Mail/topics.md").unwrap();
    assert!(fx.dir.join("Mail").is_dir());
    ensure_top_level_dir(&root, "Brand/New/deep.md").unwrap();
    assert!(!fx.dir.join("Brand").exists());
    ensure_top_level_dir(&root, "root.md").unwrap();
    for rel in ["../Escape/x.md", "/etc/Escape/x.md"] {
        assert_eq!(kind(ensure_top_level_dir(&root, rel)), "Refused", "{rel}");
    }
}

#[test]
fn reads_an_image_and_refuses_unknown_types_before_touching_disk() {
    use base64::Engine as _;
    let fx = Fixture::new();
    let bytes: Vec<u8> = (0..=255).collect();
    fs::write(fx.dir.join("photo.PNG"), &bytes).unwrap();
    let root = fx.root(LinkPolicy::Strict);
    let img = read_image(&root, "photo.PNG", MAX_IMAGE_BYTES).unwrap();
    assert_eq!(img.mime, "image/png");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&img.base64)
            .unwrap(),
        bytes
    );
    assert_eq!(kind(read_image(&root, "photo.PNG", 10)), "TooLarge");
    assert_eq!(
        kind(read_image(&root, "../outside.svg", MAX_IMAGE_BYTES)),
        "Refused"
    );
    assert_eq!(
        kind(read_image(&root, "missing.jpg", MAX_IMAGE_BYTES)),
        "NotFound"
    );
}

#[test]
fn delete_needs_an_existing_file() {
    let fx = Fixture::new();
    let root = fx.root(LinkPolicy::Strict);
    delete(&root, "notes/inbox.md").unwrap();
    assert!(!fx.dir.join("notes/inbox.md").exists());
    assert_eq!(kind(delete(&root, "notes/inbox.md")), "NotFound");
    assert_eq!(kind(delete(&root, "notes")), "Refused");
}

#[cfg(unix)]
#[test]
fn a_directory_swapped_for_a_symlink_after_the_guard_is_not_followed() {
    use std::os::unix::fs::symlink;

    use crate::pinned;

    let fx = Fixture::new();
    fs::write(fx.outside.join("inbox.md"), "outside").unwrap();
    for policy in [LinkPolicy::Strict, LinkPolicy::Contained] {
        let root = fx.root(policy);
        // The guard passes while `notes` is a real directory…
        let target = root.resolve("notes/inbox.md").unwrap();
        // …and then `notes` is swapped for a symlink leading out of the root.
        fs::rename(fx.dir.join("notes"), fx.dir.join("notes-real")).unwrap();
        symlink(&fx.outside, fx.dir.join("notes")).unwrap();

        let rel = "notes/inbox.md";
        let read = pinned::open_read(&root, rel, &target).map(|_| ());
        assert_eq!(kind(read), "Refused", "{policy:?} read");
        assert_eq!(
            kind(pinned::replace(&root, rel, &target, b"x")),
            "Refused",
            "{policy:?} write"
        );
        assert_eq!(
            kind(pinned::unlink(&root, rel, &target)),
            "Refused",
            "{policy:?} delete"
        );
        assert_eq!(
            fs::read_to_string(fx.outside.join("inbox.md")).unwrap(),
            "outside"
        );
        assert!(!fx.outside.join(".inbox.md.axiomata-tmp").exists());

        fs::remove_file(fx.dir.join("notes")).unwrap();
        fs::rename(fx.dir.join("notes-real"), fx.dir.join("notes")).unwrap();
    }
}
