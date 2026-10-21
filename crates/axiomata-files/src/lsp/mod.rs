//! Language servers for the editor (`docs/plans/editor.md`, ED6, L1–L4).
//!
//! This side does what the webview must not: it decides which program runs
//! ([`servers`], L2), starts it in the root's folder, frames what goes in and
//! out ([`framing`]), and stops it again. The protocol itself — initialize,
//! document sync, requests — is the editor engine's (`src/editor/lsp/`, L1);
//! messages pass through here whole, as JSON text.
//!
//! One server runs per (root, server id), started by the first file of its
//! language in that root. The embedder counts the documents open on it
//! ([`LspHost::opened`], [`LspHost::closed`]); ten minutes without any, it is
//! shut down (L4). A single-file root (a dialog grant) gets none.
//!
//! **What the page may send is limited here, not in the page** (security review
//! of ED6.1): only the methods the editor's client speaks ([`ALLOWED_METHODS`])
//! and answers to the server's own requests pass, so a compromised webview
//! cannot make a server run its own commands (`workspace/executeCommand` and
//! the like). The one exception is a code action's command (ED6.7, L18): it
//! passes only as an echo of a command the server itself offered in its answer
//! to the latest `textDocument/codeAction` — read here, like the location
//! answers — and each offer only once. A `codeAction/resolve` answer offers
//! nothing: the page chooses what is resolved, and a server that echoes it back
//! would otherwise "offer" whatever the page made up (ED6.7 security review). Every message, open and close must name the page that started
//! the server; at most [`MAX_SERVERS`] run at once, and another page restarts a
//! server at most every [`RESTART_MIN`].
//!
//! Every start names the page asking (a token the page picks once per load).
//! The same page starting a server that already runs gets the running one —
//! TypeScript and TSX files share one server. Another page restarts it: that
//! page is a new client (a reload), and a server initialized by the old one
//! would refuse a second `initialize`.

pub mod framing;
pub mod servers;

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::FilesError;
use crate::file::{LARGE_FILE_BYTES, TextFile, Version, text_from_bytes};
use crate::root::Root;
use rustix::fs::{FileType, Mode, OFlags};
use servers::{Overrides, Resolution};
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt;

/// How long a server may sit with no open document before it is stopped (L4).
pub const IDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How often idle servers are looked for.
const JANITOR_EVERY: Duration = Duration::from_secs(30);
/// How long a server gets to exit after `shutdown`/`exit` before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// Sent to the page when a server's output ends, so the client can tell the
/// user and forget the server. `$/` is the protocol's prefix for
/// implementation-specific messages.
pub const EXITED_NOTIFICATION: &str =
    r#"{"jsonrpc":"2.0","method":"$/axiomata/exited","params":{}}"#;

/// Where a server's messages go (the embedder's channel to the page).
pub type Sink = Arc<dyn Fn(String) + Send + Sync>;

/// The client-to-server methods the page may send — what the editor's client
/// speaks. Grows with each checkpoint that adds a request (hover, definition,
/// …); anything else, above all a server's own commands, is refused.
pub const ALLOWED_METHODS: &[&str] = &[
    "initialize",
    "initialized",
    "shutdown",
    "exit",
    "$/cancelRequest",
    "textDocument/didOpen",
    "textDocument/didChange",
    "textDocument/didClose",
    // ED6.2
    "textDocument/hover",
    "textDocument/definition",
    // ED6.3
    "textDocument/implementation",
    "textDocument/typeDefinition",
    "textDocument/references",
    // ED6.4
    "textDocument/completion",
    "completionItem/resolve",
    // ED6.5
    "textDocument/formatting",
    "textDocument/prepareRename",
    "textDocument/rename",
    // ED6.6
    "textDocument/signatureHelp",
    // ED6.7 — `workspace/executeCommand` only for an offered command (L18, [`LspHost::send`]).
    "textDocument/codeAction",
    "codeAction/resolve",
    "workspace/executeCommand",
];

/// The request whose answers offer commands: only those may be executed (L18, ED6.7).
const CODE_ACTION: &str = "textDocument/codeAction";

/// The one method that runs a server's command.
const EXECUTE_COMMAND: &str = "workspace/executeCommand";

/// The requests whose answers name places in files ([`LOCATION_METHODS`]): the
/// files they name outside the roots become readable (L11, ED6.3).
pub const LOCATION_METHODS: &[&str] = &[
    "textDocument/definition",
    "textDocument/implementation",
    "textDocument/typeDefinition",
    "textDocument/references",
];

/// Most files outside the roots one server may make readable (L11).
const MAX_FOREIGN: usize = 4096;
/// Most location and code-action requests of one server waiting for their answer.
const MAX_PENDING_ANSWERS: usize = 256;
/// Most commands one server may have on offer at once (L18).
const MAX_OFFERS: usize = 256;

/// Most servers running at once.
pub const MAX_SERVERS: usize = 8;
/// Shortest time between two starts of one server by different pages.
pub const RESTART_MIN: Duration = Duration::from_secs(2);

/// What [`LspHost::start`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Started {
    /// Running: its handle, its id, and the root folder (the client's `rootUri`).
    Running {
        handle: u64,
        server: String,
        root_path: PathBuf,
    },
    /// No server for this language, or none in this kind of root.
    None,
    /// Turned off in `~/.axiomata/lsp.json`.
    Disabled { server: String },
    /// Not installed: how to get it (the quiet hint, L10).
    Missing { server: String, install: String },
}

struct Server {
    key: (String, &'static str),
    /// The page that started it (see the module doc).
    page: String,
    root_path: PathBuf,
    child: Child,
    /// Shared so a write does not hold the host's lock (a full pipe would stall every command).
    stdin: Arc<Mutex<ChildStdin>>,
    open: usize,
    idle_since: Option<Instant>,
    /// Ids of requests in flight whose answers are read here, and what each asked.
    watched: HashMap<String, Watch>,
    /// Commands the server offered in a code-action answer, not yet executed (L18).
    offers: Vec<Offer>,
    /// How many code-action requests were sent: the latest one's answer counts ([`Watch::Actions`]).
    action_requests: u64,
    /// Files the server named in a location answer: readable, read-only (L11).
    foreign: HashSet<PathBuf>,
    /// The only folders outside the roots such a file may lie in
    /// ([`servers::toolchain_roots`]).
    toolchain: Vec<PathBuf>,
}

/// What an answer being waited for will carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watch {
    /// Places in files ([`LOCATION_METHODS`]): the ones outside the roots become readable (L11).
    Locations,
    /// Code actions (`textDocument/codeAction`, the n-th sent): the answer to the
    /// latest one replaces the commands on offer; an older one arriving late is ignored.
    Actions(u64),
}

/// A command a server offered (`Command.command` and its `arguments`, missing = null).
#[derive(Debug, Clone)]
struct Offer {
    command: String,
    arguments: serde_json::Value,
}

impl PartialEq for Offer {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command && same_json(&self.arguments, &other.arguments)
    }
}

/// JSON equality as the page can keep it: the page parses the offer in
/// JavaScript, which has one kind of number — `1.0` comes back as `1`, a large
/// integer rounded to the nearest double — so numbers compare as doubles.
fn same_json(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_json(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, x)| y.get(key).is_some_and(|y| same_json(x, y)))
        }
        _ => a == b,
    }
}

impl Offer {
    /// The command a `Command` object names, if it is one.
    fn of(command: &serde_json::Value) -> Option<Self> {
        Some(Self {
            command: command.get("command")?.as_str()?.to_string(),
            arguments: command
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        })
    }
}

#[derive(Default)]
struct Inner {
    next: u64,
    servers: HashMap<u64, Server>,
    /// When each (root, server) was last started, for [`RESTART_MIN`].
    last_start: HashMap<(String, &'static str), Instant>,
}

/// Runs the editor's language servers.
pub struct LspHost {
    inner: Arc<Mutex<Inner>>,
    overrides_file: PathBuf,
    search: Vec<PathBuf>,
    idle_limit: Duration,
}

impl LspHost {
    /// A host that reads overrides from `overrides_file` and looks for programs in `search`.
    pub fn new(overrides_file: PathBuf, search: Vec<PathBuf>) -> Self {
        Self::with_idle_limit(overrides_file, search, IDLE_LIMIT)
    }

    /// [`LspHost::new`] with another idle limit (tests).
    pub fn with_idle_limit(
        overrides_file: PathBuf,
        search: Vec<PathBuf>,
        idle_limit: Duration,
    ) -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let weak = Arc::downgrade(&inner);
        let every = JANITOR_EVERY.min(idle_limit);
        thread::Builder::new()
            .name("lsp-janitor".into())
            .spawn(move || janitor(weak, idle_limit, every))
            .expect("spawning the language-server janitor");
        Self {
            inner,
            overrides_file,
            search,
            idle_limit,
        }
    }

    /// Starts the server for `language` in `root` for `page`, sending its
    /// messages to `sink` — or returns the one this page already runs there
    /// (then `sink` is not used; the page's first sink still gets everything).
    ///
    /// Errors:
    ///     [`FilesError::Io`] when the program cannot be started.
    pub fn start(
        &self,
        root_id: &str,
        root: &Root,
        language: &str,
        page: &str,
        sink: Sink,
    ) -> Result<Started, FilesError> {
        if root.only_file().is_some() {
            return Ok(Started::None);
        }
        let overrides =
            Overrides::load(&self.overrides_file).map_err(|reason| FilesError::Refused {
                path: self.overrides_file.clone(),
                reason,
            })?;
        let (server, program, args) = match servers::resolve(language, &overrides, &self.search) {
            Resolution::Found {
                server,
                program,
                args,
            } => (server, program, args),
            Resolution::NoServer => return Ok(Started::None),
            Resolution::Disabled { server } => {
                return Ok(Started::Disabled {
                    server: server.into(),
                });
            }
            Resolution::Missing { server, install } => {
                return Ok(Started::Missing {
                    server: server.into(),
                    install,
                });
            }
        };
        let key = (root_id.to_string(), server);
        let mut inner = lock(&self.inner);
        if let Some((handle, running)) = inner
            .servers
            .iter()
            .find(|(_, s)| s.key == key && s.page == page)
        {
            return Ok(Started::Running {
                handle: *handle,
                server: server.to_string(),
                root_path: running.root_path.clone(),
            });
        }
        let restarting = inner.servers.values().any(|s| s.key == key);
        if restarting
            && inner
                .last_start
                .get(&key)
                .is_some_and(|at| at.elapsed() < RESTART_MIN)
        {
            return Err(FilesError::Refused {
                path: root.path().to_path_buf(),
                reason: format!("the {server} language server is being restarted too often"),
            });
        }
        if !restarting && inner.servers.len() >= MAX_SERVERS {
            return Err(FilesError::Refused {
                path: root.path().to_path_buf(),
                reason: format!("{MAX_SERVERS} language servers are running already"),
            });
        }
        let running: Vec<u64> = inner
            .servers
            .iter()
            .filter(|(_, s)| s.key == key)
            .map(|(id, _)| *id)
            .collect();
        for id in running {
            if let Some(old) = inner.servers.remove(&id) {
                stop(old);
            }
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let toolchain = servers::toolchain_roots(server, &program, &self.search, home.as_deref());
        let mut command = Command::new(&program);
        crate::toolenv::apply(&mut command, &self.search);
        let mut child = command
            .args(&args)
            .current_dir(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Servers log freely to stderr; nobody reads it, so it must not
            // fill a pipe and block them.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| FilesError::Io {
                path: program.clone(),
                source,
            })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        inner.next += 1;
        let handle = inner.next;
        inner.last_start.insert(key.clone(), Instant::now());
        inner.servers.insert(
            handle,
            Server {
                key,
                page: page.to_string(),
                root_path: root.path().to_path_buf(),
                child,
                stdin: Arc::new(Mutex::new(stdin)),
                open: 0,
                idle_since: Some(Instant::now()),
                watched: HashMap::new(),
                offers: Vec::new(),
                action_requests: 0,
                foreign: HashSet::new(),
                toolchain,
            },
        );
        drop(inner);
        let weak = Arc::downgrade(&self.inner);
        thread::Builder::new()
            .name(format!("lsp-{server}"))
            .spawn(move || pump(BufReader::new(stdout), sink, weak, handle))
            .map_err(|source| FilesError::Io {
                path: program.clone(),
                source,
            })?;
        Ok(Started::Running {
            handle,
            server: server.to_string(),
            root_path: root.path().to_path_buf(),
        })
    }

    /// Sends one JSON message from `page` to a running server it started.
    ///
    /// Errors:
    ///     [`FilesError::Refused`] for a message over [`framing::MAX_MESSAGE_BYTES`],
    ///     one that is not a JSON object, a method outside [`ALLOWED_METHODS`], a
    ///     server that is not running or not this page's; [`FilesError::Io`] when
    ///     writing fails.
    pub fn send(&self, handle: u64, page: &str, message: &str) -> Result<(), FilesError> {
        if message.len() > framing::MAX_MESSAGE_BYTES {
            return Err(refused(handle, "a message larger than the limit"));
        }
        let value: serde_json::Value =
            serde_json::from_str(message).map_err(|_| refused(handle, "not a JSON-RPC message"))?;
        if !allowed(&value) {
            return Err(refused(handle, "a message the editor does not send"));
        }
        let stdin = {
            let mut inner = lock(&self.inner);
            let server = inner
                .servers
                .get_mut(&handle)
                .filter(|s| s.page == page)
                .ok_or_else(|| refused(handle, "no such language server for this page"))?;
            let method = value.get("method").and_then(serde_json::Value::as_str);
            if method == Some(EXECUTE_COMMAND) {
                take_offer(&mut server.offers, value.get("params"))
                    .ok_or_else(|| refused(handle, "a command the server did not offer"))?;
            }
            let watch = match method {
                Some(m) if LOCATION_METHODS.contains(&m) => Some(Watch::Locations),
                Some(CODE_ACTION) => {
                    server.action_requests += 1;
                    Some(Watch::Actions(server.action_requests))
                }
                _ => None,
            };
            if let Some(watch) = watch
                && let Some(id) = value.get("id")
            {
                if server.watched.len() >= MAX_PENDING_ANSWERS {
                    return Err(refused(handle, "too many requests waiting for an answer"));
                }
                server.watched.insert(id.to_string(), watch);
            }
            Arc::clone(&server.stdin)
        };
        let mut stdin = stdin.lock().unwrap_or_else(|poison| poison.into_inner());
        stdin
            .write_all(&framing::encode(message))
            .and_then(|()| stdin.flush())
            .map_err(|source| FilesError::Io {
                path: PathBuf::from(format!("language server {handle}")),
                source,
            })
    }

    /// A document was opened on `page`'s server: it is in use.
    pub fn opened(&self, handle: u64, page: &str) {
        let mut inner = lock(&self.inner);
        if let Some(server) = inner.servers.get_mut(&handle).filter(|s| s.page == page) {
            server.open += 1;
            server.idle_since = None;
        }
    }

    /// A document was closed on `page`'s server; with none left its idle time starts.
    pub fn closed(&self, handle: u64, page: &str) {
        let mut inner = lock(&self.inner);
        if let Some(server) = inner.servers.get_mut(&handle).filter(|s| s.page == page) {
            server.open = server.open.saturating_sub(1);
            if server.open == 0 {
                server.idle_since = Some(Instant::now());
            }
        }
    }

    /// Reads a file outside every root that server `handle` named in a
    /// location answer (L11) — read-only, a regular file reached without any
    /// symbolic link, at most `max_bytes`, UTF-8.
    ///
    /// Errors:
    ///     [`FilesError::Refused`] for a path the server did not name, a link or
    ///     not a regular file; [`FilesError::TooLarge`], [`FilesError::NotUtf8`],
    ///     [`FilesError::Io`] as for any read.
    pub fn read_foreign(
        &self,
        handle: u64,
        path: &Path,
        max_bytes: u64,
    ) -> Result<TextFile, FilesError> {
        let named = lock(&self.inner)
            .servers
            .get(&handle)
            .is_some_and(|s| s.foreign.contains(path));
        if !named {
            return Err(FilesError::Refused {
                path: path.to_path_buf(),
                reason: "not a file this language server pointed to".into(),
            });
        }
        let io = |source| FilesError::Io {
            path: path.to_path_buf(),
            source,
        };
        let not_plain = || FilesError::Refused {
            path: path.to_path_buf(),
            reason: "not a regular file reached without links".into(),
        };
        // Opened once, and everything is checked on that open file: a path swapped
        // for a link between a check and a second open cannot redirect the read
        // (the invariant `pinned.rs` keeps for every root).
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::open(path, flags, Mode::empty()).map_err(|err| {
            if err == rustix::io::Errno::LOOP {
                not_plain()
            } else {
                io(err.into())
            }
        })?;
        let stat = rustix::fs::fstat(&fd).map_err(|err| io(err.into()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(not_plain());
        }
        // Where the open file really is: a link in a parent folder would show here.
        let actual = rustix::fs::getpath(&fd).map_err(|err| io(err.into()))?;
        if Path::new(std::ffi::OsStr::from_bytes(actual.as_bytes())) != path {
            return Err(not_plain());
        }
        if stat.st_size as u64 > max_bytes {
            return Err(FilesError::TooLarge {
                path: path.to_path_buf(),
                limit: max_bytes,
            });
        }
        let mut file = std::fs::File::from(fd);
        let mut bytes = Vec::new();
        (&mut file)
            .take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() as u64 > max_bytes {
            return Err(FilesError::TooLarge {
                path: path.to_path_buf(),
                limit: max_bytes,
            });
        }
        let modified = file.metadata().ok().and_then(|m| m.modified().ok());
        let version = Version::of(&bytes);
        let large = bytes.len() as u64 > LARGE_FILE_BYTES;
        let content = text_from_bytes(bytes).ok_or_else(|| FilesError::NotUtf8 {
            path: path.to_path_buf(),
        })?;
        Ok(TextFile {
            rel: path.display().to_string(),
            content,
            version,
            modified: modified.map(chrono::DateTime::<chrono::Utc>::from),
            large,
        })
    }

    /// Stops a server now.
    pub fn stop(&self, handle: u64) {
        let removed = lock(&self.inner).servers.remove(&handle);
        if let Some(server) = removed {
            stop(server);
        }
    }

    /// Whether a server is running (tests, diagnostics).
    pub fn is_running(&self, handle: u64) -> bool {
        lock(&self.inner).servers.contains_key(&handle)
    }

    /// The idle limit this host applies.
    pub fn idle_limit(&self) -> Duration {
        self.idle_limit
    }
}

impl Drop for LspHost {
    fn drop(&mut self) {
        let servers: Vec<Server> = lock(&self.inner).servers.drain().map(|(_, s)| s).collect();
        for server in servers {
            stop(server);
        }
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Whether the page may send `message`: a request or notification of an
/// allowed method, or an answer (`id` plus `result` or `error`) to a request the
/// server made.
fn allowed(message: &serde_json::Value) -> bool {
    let Some(object) = message.as_object() else {
        return false;
    };
    match object.get("method") {
        Some(method) => method
            .as_str()
            .is_some_and(|m| ALLOWED_METHODS.contains(&m)),
        None => {
            object.contains_key("id")
                && (object.contains_key("result") || object.contains_key("error"))
        }
    }
}

fn refused(handle: u64, reason: &str) -> FilesError {
    FilesError::Refused {
        path: PathBuf::from(format!("language server {handle}")),
        reason: reason.to_string(),
    }
}

/// Reads the server's messages into `sink` until its output ends, then tells
/// the page and forgets the server (if it is still the same one).
fn pump(
    mut stdout: BufReader<std::process::ChildStdout>,
    sink: Sink,
    inner: Weak<Mutex<Inner>>,
    handle: u64,
) {
    while let Ok(Some(message)) = framing::read_message(&mut stdout) {
        if let Some(inner) = inner.upgrade() {
            note_answer(&inner, handle, &message);
        }
        sink(message);
    }
    sink(EXITED_NOTIFICATION.to_string());
    if let Some(inner) = inner.upgrade() {
        let gone = lock(&inner).servers.remove(&handle);
        if let Some(mut server) = gone {
            let _ = server.child.kill();
            let _ = server.child.wait();
        }
    }
}

/// Removes the offer an `executeCommand`'s `params` name — exactly (L18) —
/// and returns it; `None` when nothing on offer matches.
fn take_offer(offers: &mut Vec<Offer>, params: Option<&serde_json::Value>) -> Option<Offer> {
    let wanted = Offer::of(params?)?;
    let at = offers.iter().position(|offer| *offer == wanted)?;
    Some(offers.remove(at))
}

/// If `message` answers a request being watched ([`Watch`]), reads what it
/// carries: the files a location answer names become readable (L11), the
/// commands of a code-action answer go on offer (L18). Parses only while such
/// a request is open.
fn note_answer(inner: &Mutex<Inner>, handle: u64, message: &str) {
    let waiting = lock(inner)
        .servers
        .get(&handle)
        .is_some_and(|s| !s.watched.is_empty());
    if !waiting {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(message) else {
        return;
    };
    if value.get("method").is_some() {
        return;
    }
    let Some(id) = value.get("id").map(serde_json::Value::to_string) else {
        return;
    };
    let mut guard = lock(inner);
    let Some(server) = guard.servers.get_mut(&handle) else {
        return;
    };
    let Some(watch) = server.watched.remove(&id) else {
        return;
    };
    let result = value.get("result").unwrap_or(&serde_json::Value::Null);
    if let Watch::Actions(nth) = watch {
        if nth != server.action_requests {
            return;
        }
        server.offers.clear();
        let items = match result {
            serde_json::Value::Array(items) => items.as_slice(),
            single => std::slice::from_ref(single),
        };
        for item in items {
            if server.offers.len() >= MAX_OFFERS {
                break;
            }
            // A `Command` names its command as a string; a `CodeAction` carries a `Command` object.
            let offer = match item.get("command") {
                Some(serde_json::Value::String(_)) => Offer::of(item),
                Some(command) => Offer::of(command),
                None => None,
            };
            server.offers.extend(offer);
        }
        return;
    }
    let mut paths = Vec::new();
    if let Some(result) = value.get("result") {
        collect_locations(result, &mut paths);
    }
    // Only what lies in the server's toolchain folders: a project file can make a
    // server name any path at all (ED6.2 security review).
    for path in paths {
        if server.foreign.len() >= MAX_FOREIGN {
            break;
        }
        if server.toolchain.iter().any(|root| path.starts_with(root)) {
            server.foreign.insert(path);
        }
    }
}

/// The file paths of the `uri`/`targetUri` fields in a location answer
/// (`Location`, `Location[]` or `LocationLink[]`).
fn collect_locations(value: &serde_json::Value, out: &mut Vec<PathBuf>) {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().for_each(|item| collect_locations(item, out))
        }
        serde_json::Value::Object(map) => {
            for key in ["uri", "targetUri"] {
                if let Some(path) = map
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .and_then(file_uri_path)
                {
                    out.push(path);
                }
            }
        }
        _ => {}
    }
}

/// The path of a `file://` URI, percent-decoded; `None` for anything else.
pub fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut chars = rest.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hex = [chars.next()?, chars.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let path = PathBuf::from(String::from_utf8(bytes).ok()?);
    path.is_absolute().then_some(path)
}

/// Asks a server to shut down and exit, and kills it if it does not.
fn stop(server: Server) {
    {
        let mut stdin = server
            .stdin
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for message in [
            r#"{"jsonrpc":"2.0","id":"axiomata-shutdown","method":"shutdown"}"#,
            r#"{"jsonrpc":"2.0","method":"exit"}"#,
        ] {
            let _ = stdin.write_all(&framing::encode(message));
        }
        let _ = stdin.flush();
    }
    // The last clone closes the pipe; a write in flight still holds one.
    drop(server.stdin);
    let mut child = server.child;
    thread::spawn(move || {
        let deadline = Instant::now() + EXIT_GRACE;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    });
}

/// Stops servers that sat without an open document for `idle_limit`.
fn janitor(inner: Weak<Mutex<Inner>>, idle_limit: Duration, every: Duration) {
    loop {
        thread::sleep(every);
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let idle: Vec<Server> = {
            let mut guard = lock(&inner);
            let ids: Vec<u64> = guard
                .servers
                .iter()
                .filter(|(_, s)| {
                    s.idle_since
                        .is_some_and(|since| since.elapsed() >= idle_limit)
                })
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.servers.remove(&id))
                .collect()
        };
        for server in idle {
            stop(server);
        }
    }
}

/// The override file's usual place under an Axiomata home.
pub fn overrides_path(axiomata_home: &Path) -> PathBuf {
    axiomata_home.join("lsp.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    /// A tiny stand-in language server: answers every framed message by
    /// echoing it back framed, and exits on `exit`.
    const ECHO_SERVER: &str = r#"#!/usr/bin/env python3
import sys
while True:
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        line = line.strip()
        if not line:
            break
        name, _, value = line.decode().partition(":")
        if name.lower() == "content-length":
            length = int(value)
    body = sys.stdin.buffer.read(length)
    if b'"exit"' in body:
        sys.exit(0)
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()
"#;

    fn setup(name: &str) -> (PathBuf, Root) {
        let dir =
            std::env::temp_dir().join(format!("axiomata-lsp-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let server = bin.join("rust-analyzer");
        std::fs::write(&server, ECHO_SERVER).unwrap();
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let root = Root::dir(&project, crate::LinkPolicy::Contained).unwrap();
        (dir, root)
    }

    fn channel() -> (Sink, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        (Arc::new(move |m| drop(tx.lock().unwrap().send(m))), rx)
    }

    #[test]
    fn a_server_starts_in_the_root_and_messages_pass_through_framed() {
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("python3 not installed — skipping");
            return;
        }
        let (dir, root) = setup("pass");
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, rx) = channel();
        let started = host
            .start("project:1", &root, "rust", "page-a", sink)
            .unwrap();
        let Started::Running {
            handle,
            server,
            root_path,
        } = started
        else {
            panic!("{started:?}")
        };
        assert_eq!(server, "rust-analyzer");
        assert_eq!(root_path, root.path());
        host.send(
            handle,
            "page-a",
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"x":"Grüße"}}"#,
        )
        .unwrap();
        let echoed = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(echoed.contains("Grüße"));
        assert!(host.send(handle, "page-a", "not json").is_err());
        assert!(host.send(handle, "page-a", "[1]").is_err());
        let command = r#"{"jsonrpc":"2.0","id":2,"method":"workspace/executeCommand","params":{}}"#;
        assert!(
            host.send(handle, "page-a", command).is_err(),
            "a server's own commands are refused"
        );
        let initialized = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
        assert!(
            host.send(handle, "page-b", initialized).is_err(),
            "another page cannot write"
        );
        assert!(
            host.send(
                handle,
                "page-a",
                r#"{"jsonrpc":"2.0","id":7,"result":null}"#
            )
            .is_ok()
        );

        host.stop(handle);
        // A real server answers `shutdown` (the stand-in echoes it); the page
        // ignores that answer. Then the output ends and the page is told.
        let mut last = String::new();
        while last != EXITED_NOTIFICATION {
            last = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!host.is_running(handle));
    }

    #[test]
    fn the_same_page_shares_a_server_and_another_page_restarts_it() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("restart");
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        let Started::Running { handle: first, .. } = host
            .start("project:1", &root, "rust", "page-a", sink.clone())
            .unwrap()
        else {
            panic!()
        };
        let Started::Running { handle: same, .. } = host
            .start("project:1", &root, "rust", "page-a", sink.clone())
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(first, same, "the same page gets the running server");
        assert!(
            host.start("project:1", &root, "rust", "page-b", sink.clone())
                .is_err(),
            "a restart right after the start is refused"
        );
        thread::sleep(RESTART_MIN);
        let Started::Running { handle: second, .. } = host
            .start("project:1", &root, "rust", "page-b", sink)
            .unwrap()
        else {
            panic!()
        };
        assert_ne!(first, second, "a reloaded page restarts it");
        assert!(!host.is_running(first) && host.is_running(second));
    }

    #[test]
    fn file_uris_decode_and_only_absolute_file_paths_count() {
        assert_eq!(
            file_uri_path("file:///Users/me/a%20b/%C3%BC.rs"),
            Some(PathBuf::from("/Users/me/a b/ü.rs"))
        );
        assert_eq!(file_uri_path("https://example.com/x"), None);
        assert_eq!(file_uri_path("file://relative"), None);
        assert_eq!(file_uri_path("file:///bad%zz"), None);
        let mut found = Vec::new();
        collect_locations(
            &serde_json::json!([{"uri": "file:///a.rs"}, {"targetUri": "file:///b.rs", "originSelectionRange": {}}]),
            &mut found,
        );
        assert_eq!(found, vec![PathBuf::from("/a.rs"), PathBuf::from("/b.rs")]);
    }

    #[test]
    fn only_named_files_in_the_toolchain_folders_become_readable() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("foreign");
        let tool = dir.join("toolchain");
        std::fs::create_dir_all(&tool).unwrap();
        let tool = tool.canonicalize().unwrap();
        let named = tool.join("lib.rs");
        std::fs::write(&named, "pub fn std() {}\n").unwrap();
        let other = tool.join("secret.rs");
        std::fs::write(&other, "secret").unwrap();
        let link = tool.join("link.rs");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        // What a project file can make a server name (`#[path = "…"]`): outside the toolchain.
        let elsewhere = dir.canonicalize().unwrap().join("id_ed25519");
        std::fs::write(&elsewhere, "private key").unwrap();

        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        let Started::Running { handle, .. } = host
            .start("project:1", &root, "rust", "page-a", sink)
            .unwrap()
        else {
            panic!()
        };
        lock(&host.inner)
            .servers
            .get_mut(&handle)
            .unwrap()
            .toolchain = vec![tool.clone()];
        let answer = |id: u32| {
            let uris: Vec<_> = [&named, &link, &elsewhere]
                .iter()
                .map(|p| serde_json::json!({"uri": format!("file://{}", p.display())}))
                .collect();
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": uris}).to_string()
        };
        // An answer to nothing that was asked frees nothing.
        note_answer(&host.inner, handle, &answer(9));
        assert!(host.read_foreign(handle, &named, 1024).is_err());

        let request = r#"{"jsonrpc":"2.0","id":9,"method":"textDocument/definition","params":{}}"#;
        host.send(handle, "page-a", request).unwrap();
        note_answer(&host.inner, handle, &answer(9));
        assert_eq!(
            host.read_foreign(handle, &named, 1024).unwrap().content,
            "pub fn std() {}\n"
        );
        assert!(
            host.read_foreign(handle, &elsewhere, 1024).is_err(),
            "named, but not a toolchain file"
        );
        assert!(
            host.read_foreign(handle, &other, 1024).is_err(),
            "never named"
        );
        assert!(
            host.read_foreign(handle, &link, 1024).is_err(),
            "a link, even though named"
        );
        assert!(matches!(
            host.read_foreign(handle, &named, 4),
            Err(FilesError::TooLarge { .. })
        ));
    }

    #[test]
    fn every_location_answer_frees_named_files_but_a_hover_does_not() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("kinds");
        let tool = dir.join("toolchain");
        std::fs::create_dir_all(&tool).unwrap();
        let tool = tool.canonicalize().unwrap();
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        let Started::Running { handle, .. } = host
            .start("project:1", &root, "rust", "page-a", sink)
            .unwrap()
        else {
            panic!()
        };
        lock(&host.inner)
            .servers
            .get_mut(&handle)
            .unwrap()
            .toolchain = vec![tool.clone()];
        let methods = [
            "textDocument/implementation",
            "textDocument/typeDefinition",
            "textDocument/references",
            "textDocument/hover",
        ];
        for (id, method) in methods.iter().enumerate() {
            let file = tool.join(format!("{id}.rs"));
            std::fs::write(&file, "x").unwrap();
            let request =
                serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}});
            host.send(handle, "page-a", &request.to_string()).unwrap();
            let uri = format!("file://{}", file.display());
            let answer = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": [{"uri": uri}]});
            note_answer(&host.inner, handle, &answer.to_string());
            let readable = host.read_foreign(handle, &file, 1024).is_ok();
            assert_eq!(readable, *method != "textDocument/hover", "{method}");
        }
    }

    #[test]
    fn location_requests_waiting_for_an_answer_are_capped() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("pending");
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        let Started::Running { handle, .. } = host
            .start("project:1", &root, "rust", "page-a", sink)
            .unwrap()
        else {
            panic!()
        };
        // The stand-in echoes requests back, which answers nothing: they stay waiting.
        let request = |id: usize| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"textDocument/definition","params":{{}}}}"#
            )
        };
        for id in 0..MAX_PENDING_ANSWERS {
            host.send(handle, "page-a", &request(id)).unwrap();
        }
        assert!(
            host.send(handle, "page-a", &request(MAX_PENDING_ANSWERS))
                .is_err()
        );
    }

    #[test]
    fn no_more_than_the_limit_of_servers_run_at_once() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("cap");
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        for i in 0..MAX_SERVERS {
            let started = host.start(
                &format!("project:{i}"),
                &root,
                "rust",
                "page-a",
                sink.clone(),
            );
            assert!(matches!(started, Ok(Started::Running { .. })), "{i}");
        }
        assert!(
            host.start("project:extra", &root, "rust", "page-a", sink)
                .is_err()
        );
    }

    #[test]
    fn a_server_without_open_documents_stops_after_the_idle_limit() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("idle");
        let host = LspHost::with_idle_limit(
            dir.join("lsp.json"),
            vec![dir.join("bin")],
            Duration::from_millis(200),
        );
        let (sink, _rx) = channel();
        let Started::Running { handle, .. } = host
            .start("project:1", &root, "rust", "page-a", sink)
            .unwrap()
        else {
            panic!()
        };
        host.opened(handle, "page-a");
        thread::sleep(Duration::from_millis(600));
        assert!(host.is_running(handle), "an open document keeps it");
        host.closed(handle, "page-a");
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.is_running(handle) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!host.is_running(handle));
    }

    #[test]
    fn missing_disabled_single_file_and_unknown_languages_start_nothing() {
        let (dir, root) = setup("none");
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        assert!(matches!(
            host.start("project:1", &root, "python", "page-a", sink.clone()).unwrap(),
            Started::Missing { server, .. } if server == "pyright"
        ));
        assert_eq!(
            host.start("project:1", &root, "cobol", "page-a", sink.clone())
                .unwrap(),
            Started::None
        );
        std::fs::write(
            dir.join("lsp.json"),
            r#"{"servers": {"rust-analyzer": false}}"#,
        )
        .unwrap();
        assert!(matches!(
            host.start("project:1", &root, "rust", "page-a", sink.clone())
                .unwrap(),
            Started::Disabled { .. }
        ));
        std::fs::write(root.path().join("one.rs"), "fn main() {}").unwrap();
        let single = Root::single_file(&root.path().join("one.rs")).unwrap();
        assert_eq!(
            host.start("grant:1", &single, "rust", "page-a", sink)
                .unwrap(),
            Started::None
        );
    }

    #[test]
    fn only_commands_the_server_offered_run_and_each_only_once() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let (dir, root) = setup("offers");
        let host = LspHost::new(dir.join("lsp.json"), vec![dir.join("bin")]);
        let (sink, _rx) = channel();
        let Started::Running { handle, .. } = host
            .start("project:1", &root, "rust", "page-a", sink)
            .unwrap()
        else {
            panic!()
        };
        let execute = |params: serde_json::Value| {
            let message = serde_json::json!({
                "jsonrpc": "2.0", "id": 100, "method": "workspace/executeCommand", "params": params,
            });
            host.send(handle, "page-a", &message.to_string())
        };
        // The server answers `id` (a request of `method` sent first) with `result`.
        let answer = |id: u64, method: &str, result: serde_json::Value| {
            let request =
                serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}});
            host.send(handle, "page-a", &request.to_string()).unwrap();
            let answer = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
            note_answer(&host.inner, handle, &answer.to_string());
        };

        assert!(execute(serde_json::json!({"command": "fix", "arguments": [1]})).is_err());

        // A bare `Command` and a `CodeAction` carrying one are both offered.
        answer(
            1,
            "textDocument/codeAction",
            serde_json::json!([
                {"title": "a", "command": "fix", "arguments": [1.0, {"k": "v"}]},
                {"title": "b", "kind": "quickfix", "command": {"title": "b", "command": "organize"}},
            ]),
        );
        // `1.0` comes back from the page as `1`.
        assert!(
            execute(serde_json::json!({"command": "fix", "arguments": [1, {"k": "v"}]})).is_ok()
        );
        assert!(
            execute(serde_json::json!({"command": "fix", "arguments": [1, {"k": "v"}]})).is_err(),
            "an offer runs once"
        );
        assert!(execute(serde_json::json!({"command": "organize", "arguments": []})).is_err());
        assert!(execute(serde_json::json!({"command": "organize"})).is_ok());

        // A new code-action answer replaces what was on offer.
        answer(
            2,
            "textDocument/codeAction",
            serde_json::json!([{"title": "c", "command": "old"}]),
        );
        answer(3, "textDocument/codeAction", serde_json::json!([]));
        assert!(execute(serde_json::json!({"command": "old"})).is_err());
        // A resolve answer offers nothing: the page chose what was resolved, and a server may echo it.
        answer(
            4,
            "codeAction/resolve",
            serde_json::json!({"title": "d", "command": {"title": "d", "command": "made-up", "arguments": ["x"]}}),
        );
        assert!(execute(serde_json::json!({"command": "made-up", "arguments": ["x"]})).is_err());

        // Two requests out: the older one's late answer does not replace the newer one's offers.
        for id in [6, 7] {
            let request = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/codeAction", "params": {}});
            host.send(handle, "page-a", &request.to_string()).unwrap();
        }
        let reply = |id: u64, command: &str| {
            let answer = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": [{"title": "t", "command": command}]});
            note_answer(&host.inner, handle, &answer.to_string());
        };
        reply(7, "fresh");
        reply(6, "stale");
        assert!(execute(serde_json::json!({"command": "stale"})).is_err());
        assert!(execute(serde_json::json!({"command": "fresh"})).is_ok());

        // An answer to anything else offers nothing.
        answer(
            5,
            "textDocument/hover",
            serde_json::json!({"title": "e", "command": "sneaky"}),
        );
        assert!(execute(serde_json::json!({"command": "sneaky"})).is_err());
    }
}
