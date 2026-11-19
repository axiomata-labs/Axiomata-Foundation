//! Running a plugin: the engine, what a plugin's calls reach on the host (the log, the app's language, its own
//! storage, WASI with nothing granted), and the limits of every call — a fresh instance, one budget for all its
//! memories and tables, and a time limit through wasmtime's epochs.
//!
//! Epochs end a plugin only while its own code runs; a host function that blocks would hold the call past its
//! deadline. WASI's waiting — `poll` and a pollable's `block`, which a sleep or a read of a stream comes down to — is
//! therefore replaced by a trap: a plugin may not wait for anything.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use wasmtime::component::{HasSelf, Resource, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};
use wasmtime_wasi::p2::DynPollable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::bindings::axiomata::plugin::{log, settings, storage, ui};

/// How often the engine's epoch moves on while a call runs: the grain of every time limit.
pub(crate) const TICK: Duration = Duration::from_millis(10);
/// A storage key's length, one value and all values together.
const MAX_KEY_CHARS: usize = 64;
pub(crate) const MAX_VALUE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_STORED_BYTES: u64 = 16 * 1024 * 1024;
/// How many keys a storage holds.
const MAX_KEYS: usize = 4096;
/// One log line, and how many one call may write.
const MAX_LINE_CHARS: usize = 2000;
const MAX_LINES_PER_CALL: u32 = 100;
/// What a plugin's component may hold: instances, tables and memories — generous for a component of a few core
/// modules (a JavaScript plugin brings QuickJS and its libc) — and the elements of all its tables together.
const MAX_INSTANCES: usize = 64;
const MAX_TABLES: usize = 64;
const MAX_MEMORIES: usize = 8;
const MAX_TABLE_ELEMENTS: usize = 1_000_000;
/// The words that say a call wanted more memory than its budget; `why` finds them in the trap.
const OVER_BUDGET: &str = "the plugin wanted more memory than it may use";
/// The words of the trap a plugin's waiting becomes.
const NO_WAITING: &str = "the plugin wanted to wait, which plugins may not";
/// The most a plugin hands the host in one transfer — a page, a log line, a value to keep — counted by wasmtime
/// before it copies (its hostcall fuel, 128 MiB unless set): strings that point at one buffer are counted each time.
const MAX_TRANSFER_BYTES: usize = 16 * 1024 * 1024;
/// The version of `wasi:io` the linker defines; a component built against an older 0.2 links to it.
const WASI_POLL: &str = "wasi:io/poll@0.2.12";

/// Where a plugin's lines go: the app's own log, with the plugin's id.
pub type LogSink = Arc<dyn Fn(&str, Level, &str) + Send + Sync>;
pub use log::Level;

/// The engine every plugin of an app runs on, with the thread that moves its epoch on while calls run.
pub(crate) struct Machine {
    pub engine: Engine,
    ticking: Arc<Ticking>,
}

/// How many calls run, so the epoch thread sleeps while none does instead of waking a hundred times a second.
struct Ticking {
    calls: Mutex<usize>,
    wake: Condvar,
    stop: AtomicBool,
}

/// One call running: the epoch moves on until it ends.
pub(crate) struct Running(Arc<Ticking>);

impl Drop for Running {
    fn drop(&mut self) {
        *lock(&self.0.calls) -= 1;
    }
}

impl Machine {
    pub fn new() -> wasmtime::Result<Self> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let ticking = Arc::new(Ticking {
            calls: Mutex::new(0),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (weak, shared) = (engine.weak(), Arc::clone(&ticking));
        std::thread::Builder::new()
            .name("plugin-epoch".into())
            .spawn(move || {
                loop {
                    {
                        let mut calls = lock(&shared.calls);
                        while *calls == 0 && !shared.stop.load(Ordering::Relaxed) {
                            calls = shared
                                .wake
                                .wait(calls)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    }
                    if shared.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(TICK);
                    match weak.upgrade() {
                        Some(engine) => engine.increment_epoch(),
                        None => break,
                    }
                }
            })?;
        Ok(Self { engine, ticking })
    }

    /// Notes a call that begins; its epoch moves on until the returned guard is dropped.
    pub fn running(&self) -> Running {
        *lock(&self.ticking.calls) += 1;
        self.ticking.wake.notify_one();
        Running(Arc::clone(&self.ticking))
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.ticking.stop.store(true, Ordering::Relaxed);
        self.ticking.wake.notify_one();
    }
}

/// One budget for all memories and all tables of a call's instance: wasmtime's own limits count each memory alone.
pub(crate) struct Budget {
    memory: usize,
    memory_used: usize,
    elements_used: usize,
}

impl ResourceLimiter for Budget {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let after = self
            .memory_used
            .saturating_sub(current)
            .saturating_add(desired);
        if after > self.memory {
            return Err(wasmtime::format_err!("{OVER_BUDGET}"));
        }
        self.memory_used = after;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let after = self
            .elements_used
            .saturating_sub(current)
            .saturating_add(desired);
        if after > MAX_TABLE_ELEMENTS {
            return Err(wasmtime::format_err!("{OVER_BUDGET}"));
        }
        self.elements_used = after;
        Ok(true)
    }

    fn instances(&self) -> usize {
        MAX_INSTANCES
    }

    fn tables(&self) -> usize {
        MAX_TABLES
    }

    fn memories(&self) -> usize {
        MAX_MEMORIES
    }
}

/// What one call of one plugin may reach: the data of its store.
pub(crate) struct Plugged {
    id: String,
    wasi: WasiCtx,
    table: ResourceTable,
    pub budget: Budget,
    /// Its storage folder, only with the right `storage`.
    storage: Option<PathBuf>,
    language: String,
    log: LogSink,
    lines: u32,
}

impl Plugged {
    pub fn new(
        id: &str,
        memory_bytes: usize,
        storage: Option<PathBuf>,
        language: String,
        log: LogSink,
    ) -> Self {
        // Nothing granted: no folders, no network, no variables or arguments; standard output and error go nowhere.
        let wasi = WasiCtx::builder()
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .build();
        let budget = Budget {
            memory: memory_bytes,
            memory_used: 0,
            elements_used: 0,
        };
        Self {
            id: id.to_owned(),
            wasi,
            table: ResourceTable::new(),
            budget,
            storage,
            language,
            log,
            lines: 0,
        }
    }

    /// Opens a store for one call that ends after `limit`.
    pub fn store(self, engine: &Engine, limit: Duration) -> Store<Self> {
        let mut store = Store::new(engine, self);
        store.limiter(|plugged| &mut plugged.budget);
        store.set_hostcall_fuel(MAX_TRANSFER_BYTES);
        let ticks = (limit.as_millis() / TICK.as_millis()).max(1);
        store.set_epoch_deadline(u64::try_from(ticks).unwrap_or(u64::MAX));
        store.epoch_deadline_trap();
        store
    }
}

/// Links everything a plugin may import: WASI P2 (nothing granted, no waiting) and the shared interfaces.
pub(crate) fn linker(engine: &Engine) -> wasmtime::Result<wasmtime::component::Linker<Plugged>> {
    let mut linker = wasmtime::component::Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    linker.allow_shadowing(true);
    {
        let mut poll = linker.instance(WASI_POLL)?;
        poll.func_wrap(
            "poll",
            |_, (_,): (Vec<Resource<DynPollable>>,)| -> wasmtime::Result<(Vec<u32>,)> {
                Err(wasmtime::format_err!("{NO_WAITING}"))
            },
        )?;
        poll.func_wrap(
            "[method]pollable.block",
            |_, (_,): (Resource<DynPollable>,)| -> wasmtime::Result<()> {
                Err(wasmtime::format_err!("{NO_WAITING}"))
            },
        )?;
    }
    linker.allow_shadowing(false);
    crate::bindings::Base::add_to_linker::<Plugged, HasSelf<Plugged>>(&mut linker, |plugged| {
        plugged
    })?;
    Ok(linker)
}

/// Why a call failed, in words for the app's log and page.
pub(crate) fn why(err: &wasmtime::Error, limit: Duration) -> String {
    match err.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => format!(
            "took longer than {:.1} s and was ended",
            limit.as_secs_f32()
        ),
        _ if format!("{err:#}").contains(OVER_BUDGET) => {
            "wanted more memory than it may use".to_owned()
        }
        _ if format!("{err:#}").contains(NO_WAITING) => {
            "wanted to wait, which plugins may not".to_owned()
        }
        _ => format!("{err:#}"),
    }
}

impl WasiView for Plugged {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl log::Host for Plugged {
    fn log(&mut self, level: Level, message: String) {
        self.lines += 1;
        if self.lines > MAX_LINES_PER_CALL {
            return;
        }
        let mut line: String = message
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .take(MAX_LINE_CHARS)
            .collect();
        if self.lines == MAX_LINES_PER_CALL {
            line.push_str(" [… more lines dropped]");
        }
        (self.log)(&self.id, level, &line);
    }
}

impl settings::Host for Plugged {
    fn language(&mut self) -> String {
        self.language.clone()
    }
}

impl ui::Host for Plugged {}

impl storage::Host for Plugged {
    fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, String> {
        let path = self.key(&key)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_VALUE_BYTES as u64 => {
                std::fs::read(&path)
                    .map(Some)
                    .map_err(|err| err.to_string())
            }
            Ok(_) => Err(format!("{key} is not a value this storage holds")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), String> {
        let path = self.key(&key)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("a value may hold at most {MAX_VALUE_BYTES} bytes"));
        }
        let folder = self.folder()?;
        // One write at a time, in every plugin: two calls of one plugin at once could both pass the quota, and
        // would share the file written beside.
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = lock(&WRITING);
        std::fs::create_dir_all(folder).map_err(|err| err.to_string())?;
        let (used, keys) = used(folder)?;
        let before = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|meta| meta.is_file());
        if before.is_none() && keys >= MAX_KEYS {
            return Err(format!("the storage holds at most {MAX_KEYS} keys"));
        }
        let after = used.saturating_sub(before.map_or(0, |meta| meta.len())) + value.len() as u64;
        if after > MAX_STORED_BYTES {
            return Err(format!(
                "the storage holds at most {MAX_STORED_BYTES} bytes"
            ));
        }
        // Written beside and renamed, so a value is either the old or the new one. A key never starts with a dot,
        // so the name in between is no key.
        let partial = folder.join(format!(".{key}.partial"));
        std::fs::write(&partial, &value)
            .and_then(|()| std::fs::rename(&partial, &path))
            .map_err(|err| {
                let _ = std::fs::remove_file(&partial);
                err.to_string()
            })
    }

    fn remove(&mut self, key: String) -> Result<(), String> {
        let path = self.key(&key)?;
        match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.to_string()),
            _ => Ok(()),
        }
    }

    fn keys(&mut self) -> Result<Vec<String>, String> {
        let folder = self.folder()?;
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.to_string()),
        };
        let mut keys: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_key(name))
            .collect();
        keys.sort();
        Ok(keys)
    }
}

impl Plugged {
    fn folder(&self) -> Result<&Path, String> {
        self.storage
            .as_deref()
            .ok_or_else(|| format!("not granted: {}", crate::manifest::STORAGE))
    }

    fn key(&self, key: &str) -> Result<PathBuf, String> {
        let folder = self.folder()?;
        if !is_key(key) {
            return Err(format!(
                "a key is 1–{MAX_KEY_CHARS} characters of a-z, 0-9, '.', '_' and '-', not starting with '.'"
            ));
        }
        Ok(folder.join(key))
    }
}

/// Whether `key` can name a value: it is a file name in the plugin's folder and nothing else.
fn is_key(key: &str) -> bool {
    (1..=MAX_KEY_CHARS).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && !key.starts_with('.')
}

/// The bytes and keys a storage folder holds now.
fn used(folder: &Path) -> Result<(u64, usize), String> {
    let mut bytes = 0;
    let mut keys = 0;
    for entry in std::fs::read_dir(folder)
        .map_err(|err| err.to_string())?
        .flatten()
    {
        // A file written beside and left by a crash is no key: it counts against nothing.
        let named_key = entry.file_name().to_str().is_some_and(is_key);
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
            && named_key
        {
            bytes += meta.len();
            keys += 1;
        }
    }
    Ok((bytes, keys))
}

/// A shared lock that a panic while holding it does not poison for good.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
