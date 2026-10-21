//! Noticing external changes to open files (E5 of `docs/plans/editor.md`
//! §ED0): an agent writing into the file the owner has open is the case this
//! exists for.
//!
//! Three choices shape it:
//!
//! * **The parent directory is watched, not the file.** An atomic write —
//!   ours, an agent's, most editors' — renames a new file over the old one,
//!   and a watch on the old inode would go silent after the first save.
//! * **Changes are judged by content, not by event.** File-system events
//!   come in bursts (FSEvents reports a rename-over as a delete plus a
//!   create). Each burst is debounced for [`DEBOUNCE`], then the file's
//!   [`Version`] is compared with the last one seen: a new version is
//!   `modified`, a vanished file `deleted`, a returned one `created`, and an
//!   unchanged one — a `touch`, a burst that settled back — nothing at all.
//! * **Our own writes are reported too.** The watcher does not know who
//!   wrote; the subscriber does, because it holds the version its own write
//!   returned and can ignore a change that carries exactly that one.
//!
//! Subscriptions are keyed by `(root id, rel)` and counted, so two panes on
//! one file share one subscription and unsubscribing one leaves the other.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use serde::Serialize;

use crate::error::FilesError;
use crate::file::{Version, current_version};
use crate::root::Root;

/// Quiet time after the last event before a file is looked at again.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// What happened to a watched file, judged by its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Modified,
    Deleted,
    Created,
}

/// One settled change to a watched file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// The root id the file was watched under, as the subscriber passed it.
    pub root: String,
    pub rel: String,
    pub kind: ChangeKind,
    /// The version now on disk; `None` once the file is gone (or no longer
    /// passes its root's guard, which to a reader is the same thing).
    pub version: Option<Version>,
}

type Key = (String, String);

struct Subscription {
    root: Root,
    /// Where the content lives (a symlink's target, if allowed) — what
    /// events are matched against.
    target: PathBuf,
    /// The directory being watched for it.
    dir: PathBuf,
    /// Last version seen; `None` while the file does not exist.
    last: Option<Version>,
    /// How many callers asked for this subscription.
    count: usize,
}

#[derive(Default)]
struct State {
    subs: HashMap<Key, Subscription>,
    /// Watched directory → how many subscriptions live in it.
    dirs: HashMap<PathBuf, usize>,
}

/// Watches files for external changes and reports each settled change to a
/// callback, on the watcher's own thread.
pub struct FileWatcher {
    state: Arc<Mutex<State>>,
    watcher: Mutex<RecommendedWatcher>,
}

impl FileWatcher {
    /// Starts the watcher. `on_change` runs on a background thread, once per
    /// settled change; keep it short (hand the change to an event channel).
    ///
    /// Errors:
    ///     [`FilesError::Io`] if the OS watcher cannot be created.
    pub fn new(on_change: impl Fn(Change) + Send + 'static) -> Result<Self, FilesError> {
        let state = Arc::new(Mutex::new(State::default()));
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // An error event carries no paths worth acting on; a later event
            // or the next read catches up.
            if let Ok(event) = event {
                let _ = tx.send(event.paths);
            }
        })
        .map_err(notify_error)?;

        let settle_state = Arc::clone(&state);
        thread::Builder::new()
            .name("axiomata-files-watch".into())
            .spawn(move || settle_loop(&settle_state, &rx, &on_change))
            .map_err(|source| FilesError::Io {
                path: PathBuf::from("<watch thread>"),
                source,
            })?;
        Ok(Self {
            state,
            watcher: Mutex::new(watcher),
        })
    }

    /// Subscribes to `rel` under `root`, reported under the id `root_id`. The
    /// file need not exist yet (a `created` then follows its first write),
    /// but its parent directory must.
    ///
    /// Errors:
    ///     every guard failure of [`Root::resolve`], and [`FilesError::Io`] if
    ///     the directory cannot be watched.
    pub fn watch(&self, root_id: &str, root: &Root, rel: &str) -> Result<(), FilesError> {
        let key = (root_id.to_string(), rel.to_string());
        let target = root.resolve(rel)?;
        let dir = target
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| crate::error::refused(rel, "path has no parent directory"))?;
        let last = current_version(root, rel)?;

        let mut state = lock(&self.state);
        if let Some(sub) = state.subs.get_mut(&key) {
            sub.count += 1;
            return Ok(());
        }
        if !state.dirs.contains_key(&dir) {
            lock(&self.watcher)
                .watch(&dir, RecursiveMode::NonRecursive)
                .map_err(notify_error)?;
        }
        *state.dirs.entry(dir.clone()).or_default() += 1;
        state.subs.insert(
            key,
            Subscription {
                root: root.clone(),
                target,
                dir,
                last,
                count: 1,
            },
        );
        Ok(())
    }

    /// Drops one subscription to `(root_id, rel)`; the file stops being
    /// watched once nobody holds it. Unknown keys are ignored.
    pub fn unwatch(&self, root_id: &str, rel: &str) {
        let key = (root_id.to_string(), rel.to_string());
        let mut state = lock(&self.state);
        let Some(sub) = state.subs.get_mut(&key) else {
            return;
        };
        sub.count -= 1;
        if sub.count > 0 {
            return;
        }
        let dir = state.subs.remove(&key).map(|s| s.dir).unwrap_or_default();
        self.release_dir(&mut state, &dir);
    }

    /// Drops every subscription — what a reloaded webview needs, since the
    /// page that held them is gone.
    pub fn unwatch_all(&self) {
        let mut state = lock(&self.state);
        state.subs.clear();
        let dirs: Vec<PathBuf> = state.dirs.drain().map(|(dir, _)| dir).collect();
        let mut watcher = lock(&self.watcher);
        for dir in dirs {
            let _ = watcher.unwatch(&dir);
        }
    }

    /// How many distinct files are watched (for tests and diagnostics).
    pub fn watched_files(&self) -> usize {
        lock(&self.state).subs.len()
    }

    fn release_dir(&self, state: &mut State, dir: &Path) {
        let Some(count) = state.dirs.get_mut(dir) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            state.dirs.remove(dir);
            let _ = lock(&self.watcher).unwatch(dir);
        }
    }
}

/// The debounce loop: collects touched paths until [`DEBOUNCE`] has passed
/// without a new event, then settles every subscription they touch.
fn settle_loop(
    state: &Mutex<State>,
    rx: &mpsc::Receiver<Vec<PathBuf>>,
    on_change: &impl Fn(Change),
) {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut last_event = Instant::now();
    loop {
        let wait = if pending.is_empty() {
            Duration::from_secs(3600)
        } else {
            DEBOUNCE.saturating_sub(last_event.elapsed())
        };
        match rx.recv_timeout(wait) {
            Ok(paths) => {
                pending.extend(paths);
                last_event = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) if !pending.is_empty() => {
                for change in settle(state, &pending) {
                    on_change(change);
                }
                pending.clear();
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The watcher, and with it the sender, is gone: stop.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Re-reads every subscription touched by `paths` — by its own path, or by
/// an event on its directory as a whole (FSEvents coalesces sometimes) — and
/// returns what really changed.
///
/// The state lock is held only to take a snapshot and to write the result
/// back; the reads and hashes in between run unlocked, so `watch`/`unwatch`
/// from a command thread never wait on a 16 MiB file being hashed.
fn settle(state: &Mutex<State>, paths: &HashSet<PathBuf>) -> Vec<Change> {
    let touched: Vec<(Key, Root)> = lock(state)
        .subs
        .iter()
        .filter(|(_, sub)| paths.contains(&sub.target) || paths.contains(&sub.dir))
        .map(|(key, sub)| (key.clone(), sub.root.clone()))
        .collect();
    let now: Vec<(Key, Option<Version>)> = touched
        .into_iter()
        .map(|(key, root)| {
            let version = current_version(&root, &key.1).ok().flatten();
            (key, version)
        })
        .collect();

    let mut state = lock(state);
    let mut changes = Vec::new();
    for (key, now) in now {
        // Unwatched while it was being read: nobody to tell.
        let Some(sub) = state.subs.get_mut(&key) else {
            continue;
        };
        let kind = match (&sub.last, &now) {
            (Some(before), Some(after)) if before != after => ChangeKind::Modified,
            (Some(_), None) => ChangeKind::Deleted,
            (None, Some(_)) => ChangeKind::Created,
            _ => continue,
        };
        sub.last.clone_from(&now);
        let (root, rel) = key;
        changes.push(Change {
            root,
            rel,
            kind,
            version: now,
        });
    }
    changes
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn notify_error(err: notify::Error) -> FilesError {
    FilesError::Io {
        path: err.paths.first().cloned().unwrap_or_default(),
        source: std::io::Error::other(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc::Receiver;

    use super::*;
    use crate::file::{MAX_WRITE_BYTES, write_text};
    use crate::root::LinkPolicy;

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "axiomata-files-watch-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn watcher() -> (FileWatcher, Receiver<Change>) {
        let (tx, rx) = mpsc::channel();
        let watcher = FileWatcher::new(move |change| {
            let _ = tx.send(change);
        })
        .unwrap();
        (watcher, rx)
    }

    /// The next change, generous enough for FSEvents' own latency.
    fn next(rx: &Receiver<Change>) -> Change {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("expected a change")
    }

    fn quiet(rx: &Receiver<Change>) -> bool {
        rx.recv_timeout(Duration::from_millis(800)).is_err()
    }

    #[test]
    fn reports_modified_deleted_and_created_by_content() {
        let dir = temp_dir();
        fs::write(dir.join("a.md"), "one").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let (watcher, rx) = watcher();
        watcher.watch("grant:1", &root, "a.md").unwrap();

        // An atomic rename-over (what an agent's tool does) is one change.
        let v2 = write_text(&root, "a.md", "two", None, MAX_WRITE_BYTES).unwrap();
        let change = next(&rx);
        assert_eq!(
            (change.kind, change.version.as_ref()),
            (ChangeKind::Modified, Some(&v2))
        );
        assert_eq!(
            (change.root.as_str(), change.rel.as_str()),
            ("grant:1", "a.md")
        );

        fs::remove_file(dir.join("a.md")).unwrap();
        assert_eq!(next(&rx).kind, ChangeKind::Deleted);

        fs::write(dir.join("a.md"), "back").unwrap();
        let change = next(&rx);
        assert_eq!(
            (change.kind, change.version),
            (ChangeKind::Created, Some(Version::of(b"back")))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stays_quiet_for_a_touch_and_for_other_files() {
        let dir = temp_dir();
        fs::write(dir.join("a.md"), "same").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let (watcher, rx) = watcher();
        watcher.watch("grant:1", &root, "a.md").unwrap();

        // Rewriting the same bytes and writing a sibling change nothing.
        fs::write(dir.join("a.md"), "same").unwrap();
        fs::write(dir.join("b.md"), "other").unwrap();
        assert!(quiet(&rx));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn counts_subscriptions_and_stops_after_the_last_unwatch() {
        let dir = temp_dir();
        fs::write(dir.join("a.md"), "one").unwrap();
        fs::write(dir.join("b.md"), "one").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let (watcher, rx) = watcher();
        watcher.watch("grant:1", &root, "a.md").unwrap();
        watcher.watch("grant:1", &root, "a.md").unwrap();
        watcher.watch("grant:1", &root, "b.md").unwrap();
        assert_eq!(watcher.watched_files(), 2);

        watcher.unwatch("grant:1", "a.md");
        assert_eq!(watcher.watched_files(), 2, "a second holder keeps it");
        watcher.unwatch("grant:1", "a.md");
        assert_eq!(watcher.watched_files(), 1);

        fs::write(dir.join("a.md"), "two").unwrap();
        fs::write(dir.join("b.md"), "two").unwrap();
        let change = next(&rx);
        assert_eq!(change.rel, "b.md");
        assert!(quiet(&rx), "a.md is no longer watched");

        watcher.unwatch_all();
        assert_eq!(watcher.watched_files(), 0);
        watcher.unwatch("grant:1", "never-watched.md");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reports_deleted_when_the_watched_directory_itself_disappears() {
        let dir = temp_dir();
        fs::write(dir.join("a.md"), "one").unwrap();
        let root = Root::dir(&dir, LinkPolicy::Contained).unwrap();
        let (watcher, rx) = watcher();
        watcher.watch("grant:1", &root, "a.md").unwrap();

        // Not just the file: the whole watched directory goes away under the
        // subscription (e.g. a discarded worktree). `current_version` then
        // fails to resolve at all (the parent can no longer be canonicalised),
        // which must settle as `Deleted`, not be swallowed or panic.
        fs::remove_dir_all(&dir).unwrap();
        let change = next(&rx);
        assert_eq!((change.kind, change.version), (ChangeKind::Deleted, None));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn watch_applies_the_root_guard() {
        let dir = temp_dir();
        let root = Root::dir(&dir, LinkPolicy::Strict).unwrap();
        let (watcher, _rx) = watcher();
        assert_eq!(
            watcher
                .watch("workspace", &root, "../x")
                .unwrap_err()
                .kind(),
            "Refused"
        );
        assert_eq!(
            watcher
                .watch("workspace", &root, "missing/x.md")
                .unwrap_err()
                .kind(),
            "Io"
        );
        assert_eq!(watcher.watched_files(), 0);
        let _ = fs::remove_dir_all(dir);
    }
}
