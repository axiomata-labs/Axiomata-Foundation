//! Plugins for the Axiomata apps (`docs/plans/plugins.md`): WebAssembly components with a WIT interface, written in
//! any language that builds a component, run in wasmtime. Hauch is the first app; Lirvel, Glut and the Studio follow.
//!
//! * **From a release** ([`Source`]): the newest release of a GitHub repository, a direct address of a release's
//!   `plugin.json`, or a folder while a plugin is written. A release carries `plugin.json` ([`Manifest`]) and
//!   `plugin.wasm`; the manifest names the component's SHA-256. The app never builds source.
//! * **Asked, then installed** ([`Plugins::fetch`] → [`Offer`] → [`Plugins::install`]): fetching checks the manifest,
//!   the checksum and the component — what it imports, what it exports — and translates it into machine code, so the
//!   user is only asked about a plugin that can run. The app shows the rights it asks for; installing grants them.
//!   The machine code is kept with its checksum and translated again when an app update changed wasmtime.
//! * **Rights** ([`manifest::SHARED_RIGHTS`] and [`App::rights`]): a plugin starts with none. A component that
//!   imports an interface whose right its manifest does not ask for is refused at fetch; a right the user takes back
//!   later turns every call of that interface into an error (`not granted: …`).
//! * **Every call meets a fresh instance**, with at most the manifest's memory and [`Limits::call`] of time (wasmtime's
//!   epochs; a hanging plugin is ended and the app goes on). Wasm memory never shrinks, so nothing is kept between
//!   calls; what must last goes into the plugin's storage, a folder only it reaches.
//! * **Versions** ([`App::interfaces`]): a plugin names the interface version it was built against; an app keeps
//!   running the older ones it still links. Updates ([`Plugins::check`], [`Plugins::check_due`]) offer the newest
//!   release; one that asks for no new right may install itself where the user allowed it. The previous version
//!   stays, one step back ([`Plugins::roll_back`]).
//!
//! Each app adds its world (`hauch:plugin`) on top of the shared one, [`WIT`]: its hooks are exported from
//! `<package>/hooks` and called through [`Plugins::call`].
//!
//! **Places** ([`App::places`], [`Manifest::places`]): every plugin's page is on the app's page "Plugins"; a plugin may
//! ask for more places in the app's window (Hauch: `sidebar`, an entry of its own in the navigation), with a sign from
//! the app's icon set ([`Manifest::icon`]). The user is asked for them like for rights, may take each back
//! ([`Plugins::set_place`]), and an update that wants a new one asks again; [`Installed::shows_at`] says where to draw
//! it. The page is the same blocks wherever it is shown.
//!
//! **An app under the hardened runtime** must be signed with the entitlement
//! `com.apple.security.cs.allow-unsigned-executable-memory`: wasmtime makes the machine code executable itself, and
//! without it macOS kills the app at the first call ("Code Signature Invalid"). `allow-jit` alone is not enough.
//! Tests run unsigned and never see this.
//!
//! On disk, under the app's plugin folder: `<id>/state.json`, `<id>/versions/<version>/` (manifest, component, machine
//! code; the current and the previous version) and `<id>/data/`, the plugin's storage.

mod host;
pub mod manifest;
mod page;
mod source;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasmtime::Store;
use wasmtime::component::{
    Component, ComponentNamedList, Instance, InstancePre, Lift, Linker, Lower, types::ComponentItem,
};

pub use host::{Level, LogSink};
use host::{Machine, Plugged, lock};
pub use manifest::Manifest;
use manifest::{MANIFEST, STORAGE, WASM};
/// The versions a manifest names, for an app to name its own.
pub use semver;
pub use source::{OURS, Source};

/// The shared WIT package `axiomata:plugin`, for an app's world to include and for the plugins' templates.
pub const WIT: &str = include_str!("../wit/plugin.wit");

/// The building blocks of a plugin's page, as the app draws them.
pub mod ui {
    pub use crate::bindings::axiomata::plugin::ui::*;
}

mod bindings {
    wasmtime::component::bindgen!({ path: "wit", world: "base" });
}

/// How long an update check holds before the next is due ([`Plugins::check_due`]).
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const STATE: &str = "state.json";
const VERSIONS: &str = "versions";
const DATA: &str = "data";
const MACHINE_CODE: &str = "plugin.cwasm";
/// The interface every app's world includes, and the one its page is exported from.
const SHARED_PACKAGE: &str = "axiomata:plugin";
/// Shared interfaces that need no right.
const FREE_INTERFACES: &[&str] = &["log", "settings", "ui"];

/// Everything that can go wrong; each message names what happened in words for the app's page.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Source(String),
    #[error("could not fetch {what}: {why}")]
    Fetch { what: String, why: String },
    #[error("the manifest is not valid: {0}")]
    Manifest(String),
    #[error("the plugin's code does not match the checksum its manifest names")]
    Checksum,
    #[error("the plugin is for {0}, not for this app")]
    WrongApp(String),
    #[error("the plugin needs a newer app (it was built against interface {0})")]
    NewerApp(Version),
    #[error("the plugin cannot run here: {0}")]
    Component(String),
    #[error("{0}")]
    Conflict(String),
    #[error("no plugin {0} is installed")]
    Missing(String),
    #[error("the plugin {0} is switched off")]
    Off(String),
    #[error("the plugin {id} failed: {why}")]
    Run { id: String, why: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The app whose plugins these are.
#[derive(Debug, Clone)]
pub struct App {
    /// Its name in a manifest's `app` (`hauch`).
    pub name: String,
    /// Its WIT package (`hauch:plugin`): its hooks are exported from `<package>/hooks`, and an import from the
    /// package needs the right named like the interface.
    pub package: String,
    /// The versions of its world it links. A plugin built against an older compatible version runs (same major
    /// version, and the same minor while the major is 0).
    pub interfaces: Vec<Version>,
    /// Its hooks, each a function of `<package>/hooks`.
    pub hooks: Vec<String>,
    /// Its own rights, on top of [`manifest::SHARED_RIGHTS`].
    pub rights: Vec<String>,
    /// The places in its window it gives plugins (Hauch: `sidebar`); every plugin's page is on its page "Plugins"
    /// anyway (plugins.md Q15).
    pub places: Vec<String>,
    /// The names of the icons a plugin may take as its sign ([`Manifest::icon`]), from the app's own set — none
    /// of those its own pages wear.
    pub icons: Vec<String>,
    /// Names a plugin may not take, regardless of case and spaces: the app's own pages in every language, so a
    /// plugin's entry never passes for one of them.
    pub reserved: Vec<String>,
}

impl App {
    /// Whether a plugin built against `interface` runs here.
    pub fn speaks(&self, interface: &Version) -> bool {
        self.interfaces.iter().any(|linked| {
            linked.major == interface.major
                && (linked.major > 0 || linked.minor == interface.minor)
                && interface <= linked
        })
    }

    /// Whether `right` is one a plugin of this app can ask for.
    pub fn knows_right(&self, right: &str) -> bool {
        manifest::SHARED_RIGHTS.contains(&right) || self.rights.iter().any(|own| own == right)
    }

    fn hooks_interface(&self) -> String {
        format!("{}/hooks", self.package)
    }
}

/// The limits of every call.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// How long one call may take, its fresh instance included.
    pub call: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            call: Duration::from_secs(2),
        }
    }
}

/// What the app keeps about an installed plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub source: Source,
    pub current: Version,
    /// The version before, kept for one step back.
    #[serde(default)]
    pub previous: Option<Version>,
    pub enabled: bool,
    /// The rights the user granted: always among those the current manifest asks for.
    pub granted: Vec<String>,
    /// Whether an update that asks for no new right installs itself.
    #[serde(default)]
    pub auto_update: bool,
    /// When updates were last looked for, in seconds since 1970.
    #[serde(default)]
    pub checked: Option<u64>,
    /// The SHA-256 of each version's machine code, checked before it is loaded.
    #[serde(default)]
    pub compiled: BTreeMap<String, String>,
    /// A newer release the last check found, until it is installed: the app shows "Update verfügbar" from it, also
    /// after a restart.
    #[serde(default)]
    pub available: Option<Version>,
    /// The version the user went back from: updates offer only what is newer, so going back is not undone by the
    /// next check.
    #[serde(default)]
    pub held_back: Option<Version>,
    /// The places the user gave it, each asked for by a manifest of it — after going back, maybe not the current
    /// one's: [`Installed::shows_at`] asks both.
    #[serde(default)]
    pub placed: Vec<String>,
}

/// An installed plugin, as the app's page shows it.
#[derive(Debug, Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub state: State,
}

impl Installed {
    /// Whether its page is shown at `place`: switched on, asked for by the current version, and given by the user.
    pub fn shows_at(&self, place: &str) -> bool {
        self.state.enabled
            && self.manifest.places.iter().any(|asked| asked == place)
            && self.state.placed.iter().any(|given| given == place)
    }

    /// Who publishes it: the GitHub account, the address's host, or "local" — from where it came, never from what
    /// its manifest says.
    pub fn publisher(&self) -> String {
        self.state.source.publisher()
    }

    /// Whether it is one of ours ("Von Axiomata").
    pub fn is_ours(&self) -> bool {
        self.state.source.is_ours()
    }
}

/// A plugin fetched and checked, waiting for the user's yes.
pub struct Offer {
    pub source: Source,
    pub manifest: Manifest,
    /// The version it would replace, if one is installed.
    pub replaces: Option<Version>,
    /// The rights it asks for that the installed version did not: an update with any waits for the user.
    pub new_rights: Vec<String>,
    /// The app's hooks it answers that the installed version did not: they reach what the app hands them (Hauch:
    /// everything it recognises), so an update with any waits for the user too.
    pub new_hooks: Vec<String>,
    /// The places it wants that the installed version did not: an update with any waits for the user.
    pub new_places: Vec<String>,
    /// Whether it has a place and comes under another name or sign than the installed version: what the user sees
    /// in the app's window changes, so it waits for the user.
    pub renamed: bool,
    /// Whether it asks for more memory than the installed version.
    pub more_memory: bool,
    manifest_bytes: Vec<u8>,
    wasm: Vec<u8>,
    machine_code: Vec<u8>,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer")
            .field("source", &self.source)
            .field("manifest", &self.manifest)
            .field("replaces", &self.replaces)
            .field("new_rights", &self.new_rights)
            .field("new_hooks", &self.new_hooks)
            .field("new_places", &self.new_places)
            .field("renamed", &self.renamed)
            .field("more_memory", &self.more_memory)
            .finish_non_exhaustive()
    }
}

impl Offer {
    pub fn publisher(&self) -> String {
        self.source.publisher()
    }

    pub fn is_ours(&self) -> bool {
        self.source.is_ours()
    }

    /// Whether it asks for more than the installed version — a right, a hook, a place, memory, or another name or
    /// sign where it has a place: then the user is asked, even where updates install themselves.
    pub fn asks_more(&self) -> bool {
        !self.new_rights.is_empty()
            || !self.new_hooks.is_empty()
            || !self.new_places.is_empty()
            || self.renamed
            || self.more_memory
    }
}

/// What looking for an update of one plugin came to ([`Plugins::check_due`]).
#[derive(Debug)]
pub enum Checked {
    /// The newest release is the one installed.
    Current(String),
    /// A newer one installed itself (asking for nothing more, updating by itself allowed).
    Updated {
        id: String,
        version: Version,
    },
    /// A newer one waits for the user's click.
    Waiting(Box<Offer>),
    Failed {
        id: String,
        error: Error,
    },
}

/// A plugin ready to run: its component linked, what it exports, what it may reach.
struct Loaded {
    manifest: Manifest,
    pre: InstancePre<Plugged>,
    exports: Vec<String>,
    storage: Option<PathBuf>,
}

/// An enabled plugin's id and the hooks it answers.
type Provider = (String, Vec<String>);

/// The plugins of one app.
pub struct Plugins {
    root: PathBuf,
    app: App,
    limits: Limits,
    machine: Machine,
    linker: Linker<Plugged>,
    language: Mutex<String>,
    log: LogSink,
    loaded: Mutex<HashMap<String, Arc<Loaded>>>,
    /// Counts every change of a plugin's state or files: a plugin loaded while one happened is not kept.
    generation: AtomicU64,
    /// The enabled plugins and the hooks they answer, read once after every change: a hook is called for every
    /// recognised text, and the folder need not be read for each.
    providers: Mutex<Option<Vec<Provider>>>,
    /// Held while a plugin's state or files change.
    changing: Mutex<()>,
}

impl Plugins {
    /// Opens the plugins of `app` kept under `root` (`~/.hauch/plugins`). `language` is the app's (`de`), `log`
    /// where the plugins' lines go.
    pub fn open(
        root: PathBuf,
        app: App,
        limits: Limits,
        language: &str,
        log: LogSink,
    ) -> Result<Self> {
        let machine =
            Machine::new().map_err(|err| Error::Component(format!("no engine: {err:#}")))?;
        let linker = host::linker(&machine.engine)
            .map_err(|err| Error::Component(format!("no linker: {err:#}")))?;
        Ok(Self {
            root,
            app,
            limits,
            machine,
            linker,
            language: Mutex::new(language.to_owned()),
            log,
            loaded: Mutex::default(),
            generation: AtomicU64::new(0),
            providers: Mutex::default(),
            changing: Mutex::default(),
        })
    }

    /// The app whose plugins these are.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Tells the plugins the app's language from now on.
    pub fn set_language(&self, language: &str) {
        *lock(&self.language) = language.to_owned();
    }

    /// Every installed plugin, by name. One whose files cannot be read is left out and logged.
    pub fn list(&self) -> Vec<Installed> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut list: Vec<Installed> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|id| manifest::is_id(id))
            .filter_map(|id| match self.installed(&id) {
                Ok(installed) => Some(installed),
                Err(err) => {
                    (self.log)(&id, Level::Error, &format!("left out: {err}"));
                    None
                }
            })
            .collect();
        list.sort_by_key(|installed| installed.manifest.name.to_lowercase());
        list
    }

    /// One installed plugin.
    pub fn installed(&self, id: &str) -> Result<Installed> {
        let state = self.state(id)?;
        let manifest = self.manifest(id, &state.current)?;
        Ok(Installed { manifest, state })
    }

    /// The enabled plugins that answer `hook`, by id.
    pub fn providing(&self, hook: &str) -> Vec<String> {
        let mut providers = lock(&self.providers);
        let providers = providers.get_or_insert_with(|| {
            let mut all: Vec<Provider> = self
                .list()
                .into_iter()
                .filter(|installed| installed.state.enabled)
                .map(|installed| (installed.manifest.id, installed.manifest.provides))
                .collect();
            all.sort();
            all
        });
        providers
            .iter()
            .filter(|(_, hooks)| hooks.iter().any(|h| h == hook))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Fetches what the user typed (`owner/repo`, an address, a folder) and checks it: nothing is installed yet.
    /// Blocks on the network for a GitHub repository or an address.
    pub fn fetch(&self, typed: &str) -> Result<Offer> {
        self.offer(Source::parse(typed)?)
    }

    /// Installs a fetched plugin after the user's yes, granting the rights it asks for — those it asked before stay
    /// as the user left them. Returns it as installed.
    pub fn install(&self, offer: Offer) -> Result<Installed> {
        let _changing = lock(&self.changing);
        let id = offer.manifest.id.clone();
        let earlier = self.state(&id).ok();
        if let Some(earlier) = &earlier
            && earlier.source != offer.source
        {
            return Err(conflict(&id, &earlier.source));
        }
        let version = offer.manifest.version.clone();
        private_folder(&self.root)?;
        let versions = self.root.join(&id).join(VERSIONS);
        std::fs::create_dir_all(&versions)?;
        let partial = versions.join(format!(".{version}.partial"));
        let _ = std::fs::remove_dir_all(&partial);
        std::fs::create_dir_all(&partial)?;
        std::fs::write(partial.join(MANIFEST), &offer.manifest_bytes)?;
        std::fs::write(partial.join(WASM), &offer.wasm)?;
        std::fs::write(partial.join(MACHINE_CODE), &offer.machine_code)?;
        let folder = versions.join(version.to_string());
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::rename(&partial, &folder)?;

        let rights = &offer.manifest.rights;
        let mut state = match earlier {
            Some(earlier) => {
                let mut granted: Vec<String> = earlier
                    .granted
                    .iter()
                    .filter(|right| rights.contains(right))
                    .cloned()
                    .collect();
                granted.extend(offer.new_rights.iter().cloned());
                let places = &offer.manifest.places;
                let mut placed: Vec<String> = earlier
                    .placed
                    .iter()
                    .filter(|place| places.contains(place) && !offer.new_places.contains(place))
                    .cloned()
                    .collect();
                placed.extend(offer.new_places.iter().cloned());
                State {
                    previous: (earlier.current != version)
                        .then_some(earlier.current.clone())
                        .or(earlier.previous),
                    current: version.clone(),
                    granted,
                    placed,
                    available: None,
                    held_back: None,
                    ..earlier
                }
            }
            None => State {
                source: offer.source.clone(),
                current: version.clone(),
                previous: None,
                enabled: true,
                granted: rights.clone(),
                auto_update: false,
                checked: Some(now()),
                compiled: BTreeMap::new(),
                available: None,
                held_back: None,
                placed: offer.manifest.places.clone(),
            },
        };
        state
            .compiled
            .insert(version.to_string(), sha256(&offer.machine_code));
        self.keep_two(&id, &mut state)?;
        self.write_state(&id, &state)?;
        self.forget(&id);
        self.installed(&id)
    }

    /// Looks for a newer release of `id` — newer than the installed one and than one the user went back from;
    /// `None` when there is none. Notes the time and what it found.
    pub fn check(&self, id: &str) -> Result<Option<Offer>> {
        let state = self.state(id)?;
        let offer = self.offer(state.source.clone())?;
        let version = &offer.manifest.version;
        let newer =
            version > &state.current && state.held_back.as_ref().is_none_or(|held| version > held);
        let available = newer.then(|| version.clone());
        self.change(id, |state| {
            state.checked = Some(now());
            state.available = available;
        })?;
        Ok(newer.then_some(offer))
    }

    /// Looks for updates of every plugin not checked within [`CHECK_EVERY`]; an update that asks for nothing more
    /// ([`Offer::asks_more`]) installs itself where the user allowed it, every other waits. Blocks on the network.
    pub fn check_due(&self) -> Vec<Checked> {
        let due = self.list().into_iter().filter(|installed| {
            installed
                .state
                .checked
                .is_none_or(|checked| now().saturating_sub(checked) >= CHECK_EVERY.as_secs())
        });
        due.map(|installed| {
            let id = installed.manifest.id.clone();
            match self.check(&id) {
                Ok(None) => Checked::Current(id),
                Ok(Some(offer)) if installed.state.auto_update && !offer.asks_more() => {
                    let version = offer.manifest.version.clone();
                    match self.install(offer) {
                        Ok(_) => Checked::Updated { id, version },
                        Err(error) => Checked::Failed { id, error },
                    }
                }
                Ok(Some(offer)) => Checked::Waiting(Box::new(offer)),
                Err(error) => Checked::Failed { id, error },
            }
        })
        .collect()
    }

    /// Goes back to the version before (and forward again with a second call). The grants stay as they are: a right
    /// the other version does not ask for reaches nothing there, since its component cannot import it. The version
    /// gone back from is held back: updates offer only what is newer.
    pub fn roll_back(&self, id: &str) -> Result<Installed> {
        let _changing = lock(&self.changing);
        let mut state = self.state(id)?;
        let Some(previous) = state.previous.take() else {
            return Err(Error::Conflict(format!("{id} has no version before")));
        };
        self.manifest(id, &previous)?;
        let left = std::mem::replace(&mut state.current, previous);
        state.held_back = (left > state.current).then(|| left.clone());
        state.available = None;
        state.previous = Some(left);
        self.write_state(id, &state)?;
        self.forget(id);
        self.installed(id)
    }

    /// Removes `id` with its storage, after the app asked the user.
    pub fn remove(&self, id: &str) -> Result<()> {
        let _changing = lock(&self.changing);
        self.state(id)?;
        self.forget(id);
        std::fs::remove_dir_all(self.root.join(id))?;
        Ok(())
    }

    /// Switches `id` on or off.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.change(id, |state| state.enabled = enabled)
    }

    /// Lets `id` update itself, or not.
    pub fn set_auto_update(&self, id: &str, auto: bool) -> Result<()> {
        self.change(id, |state| state.auto_update = auto)
    }

    /// Grants or takes back one of the rights `id` asks for.
    pub fn set_right(&self, id: &str, right: &str, granted: bool) -> Result<()> {
        let installed = self.installed(id)?;
        if !installed.manifest.rights.iter().any(|asked| asked == right) {
            return Err(Error::Conflict(format!(
                "{id} does not ask for the right {right}"
            )));
        }
        self.change(id, |state| {
            state.granted.retain(|had| had != right);
            if granted {
                state.granted.push(right.to_owned());
            }
        })
    }

    /// Gives `id` one of the places it asks for, or takes it back: its page is then only on the page "Plugins".
    pub fn set_place(&self, id: &str, place: &str, given: bool) -> Result<()> {
        let installed = self.installed(id)?;
        if !installed.manifest.places.iter().any(|asked| asked == place) {
            return Err(Error::Conflict(format!(
                "{id} does not ask for the place {place}"
            )));
        }
        self.change(id, |state| {
            state.placed.retain(|had| had != place);
            if given {
                state.placed.push(place.to_owned());
            }
        })
    }

    /// The plugin's page as it is now.
    pub fn page(&self, id: &str) -> Result<ui::Page> {
        let page = self.run(id, |store, instance, _| {
            bindings::Base::new(&mut *store, instance)?
                .axiomata_plugin_page()
                .call_show(&mut *store)
        })?;
        page::check(&page).map_err(|why| Error::Run {
            id: id.to_owned(),
            why,
        })?;
        Ok(page)
    }

    /// The user did something on the plugin's page; returns the page after.
    pub fn handle(&self, id: &str, event: &ui::Event) -> Result<ui::Page> {
        let page = self.run(id, |store, instance, _| {
            bindings::Base::new(&mut *store, instance)?
                .axiomata_plugin_page()
                .call_handle(&mut *store, event)
        })?;
        page::check(&page).map_err(|why| Error::Run {
            id: id.to_owned(),
            why,
        })?;
        Ok(page)
    }

    /// Calls `function` of the app's hooks (`<package>/hooks`) in `id`, with `params` and its results as a tuple
    /// (`(String,)` → `(Option<String>,)`). The app calls only hooks the plugin's manifest provides
    /// ([`Plugins::providing`]).
    pub fn call<Params, Results>(&self, id: &str, function: &str, params: Params) -> Result<Results>
    where
        Params: ComponentNamedList + Lower + Send + Sync,
        Results: ComponentNamedList + Lift + Send + Sync,
    {
        let hooks = self.app.hooks_interface();
        self.run(id, |store, instance, loaded| {
            Export::find(instance, store, loaded, &hooks, function)?
                .typed::<Params, Results>(store)?
                .call(&mut *store, params)
        })
    }

    /// One call of `id` on a fresh instance within the limits.
    fn run<R>(
        &self,
        id: &str,
        call: impl FnOnce(&mut Store<Plugged>, &Instance, &Loaded) -> wasmtime::Result<R>,
    ) -> Result<R> {
        let loaded = self.loaded(id)?;
        let _running = self.machine.running();
        let language = lock(&self.language).clone();
        let plugged = Plugged::new(
            id,
            loaded.manifest.memory_bytes(),
            loaded.storage.clone(),
            language,
            Arc::clone(&self.log),
        );
        let mut store = plugged.store(&self.machine.engine, self.limits.call);
        let failed = |err: wasmtime::Error| Error::Run {
            id: id.to_owned(),
            why: host::why(&err, self.limits.call),
        };
        let instance = loaded.pre.instantiate(&mut store).map_err(failed)?;
        call(&mut store, &instance, &loaded).map_err(failed)
    }

    /// The ready plugin `id`, loaded on its first call.
    fn loaded(&self, id: &str) -> Result<Arc<Loaded>> {
        if let Some(loaded) = lock(&self.loaded).get(id) {
            return Ok(Arc::clone(loaded));
        }
        let seen = self.generation.load(Ordering::SeqCst);
        let state = self.state(id)?;
        if !state.enabled {
            return Err(Error::Off(id.to_owned()));
        }
        let manifest = self.manifest(id, &state.current)?;
        let component = self.machine_code(id, &state, &manifest)?;
        let pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(|err| Error::Component(format!("{err:#}")))?;
        let exports = component
            .component_type()
            .exports(&self.machine.engine)
            .map(|(name, _)| name.to_owned())
            .collect();
        let storage = state
            .granted
            .iter()
            .any(|right| right == STORAGE)
            .then(|| self.root.join(id).join(DATA));
        let loaded = Arc::new(Loaded {
            manifest,
            pre,
            exports,
            storage,
        });
        // Kept only if nothing changed while it was read: a plugin switched off or a right taken back meanwhile is
        // read again at its next call.
        let mut cache = lock(&self.loaded);
        if self.generation.load(Ordering::SeqCst) == seen {
            cache.insert(id.to_owned(), Arc::clone(&loaded));
        }
        Ok(loaded)
    }

    /// The machine code of the current version, checked against its checksum; translated again from the component
    /// when it is missing, changed, or made by another wasmtime.
    fn machine_code(&self, id: &str, state: &State, manifest: &Manifest) -> Result<Component> {
        let folder = self.version_folder(id, &state.current);
        let engine = &self.machine.engine;
        if let Ok(bytes) = std::fs::read(folder.join(MACHINE_CODE))
            && state.compiled.get(&state.current.to_string()) == Some(&sha256(&bytes))
        {
            // SAFETY: wasmtime made these bytes for this app (`Engine::precompile_component`), and they are the ones
            // whose checksum it noted then; a file changed since is translated again below instead. Bytes from
            // another wasmtime version or configuration are refused by `deserialize` itself.
            if let Ok(component) = unsafe { Component::deserialize(engine, &bytes) } {
                return Ok(component);
            }
        }
        let wasm = std::fs::read(folder.join(WASM))?;
        if sha256(&wasm) != manifest.sha256 {
            return Err(Error::Checksum);
        }
        let bytes = engine
            .precompile_component(&wasm)
            .map_err(|err| Error::Component(format!("{err:#}")))?;
        std::fs::write(folder.join(MACHINE_CODE), &bytes)?;
        let digest = sha256(&bytes);
        let current = state.current.to_string();
        self.change(id, |state| {
            state.compiled.insert(current, digest);
        })?;
        // SAFETY: made just now by this engine.
        unsafe { Component::deserialize(engine, &bytes) }
            .map_err(|err| Error::Component(format!("{err:#}")))
    }

    /// Fetches and checks a plugin from `source`, translated into machine code.
    fn offer(&self, source: Source) -> Result<Offer> {
        let fetched = source.fetch()?;
        let manifest = Manifest::read(&fetched.manifest, &self.app)?;
        if sha256(&fetched.wasm) != manifest.sha256 {
            return Err(Error::Checksum);
        }
        let engine = &self.machine.engine;
        let machine_code = engine
            .precompile_component(&fetched.wasm)
            .map_err(|err| Error::Component(format!("{err:#}")))?;
        // SAFETY: made just now by this engine.
        let component = unsafe { Component::deserialize(engine, &machine_code) }
            .map_err(|err| Error::Component(format!("{err:#}")))?;
        self.check_component(&component, &manifest)?;
        let earlier = self.installed(&manifest.id).ok();
        if let Some(earlier) = &earlier
            && earlier.state.source != source
        {
            return Err(conflict(&manifest.id, &earlier.state.source));
        }
        let new_rights = manifest
            .rights
            .iter()
            .filter(|right| {
                earlier
                    .as_ref()
                    .is_none_or(|earlier| !earlier.manifest.rights.contains(right))
            })
            .cloned()
            .collect();
        let new_hooks = manifest
            .provides
            .iter()
            .filter(|hook| {
                earlier
                    .as_ref()
                    .is_none_or(|earlier| !earlier.manifest.provides.contains(hook))
            })
            .cloned()
            .collect();
        let new_places = manifest
            .places
            .iter()
            .filter(|place| {
                earlier
                    .as_ref()
                    .is_none_or(|earlier| !earlier.manifest.places.contains(place))
            })
            .cloned()
            .collect();
        let renamed = earlier.as_ref().is_some_and(|earlier| {
            !manifest.places.is_empty()
                && (manifest.name != earlier.manifest.name
                    || manifest.icon != earlier.manifest.icon)
        });
        let more_memory = earlier
            .as_ref()
            .is_some_and(|earlier| manifest.memory_bytes() > earlier.manifest.memory_bytes());
        Ok(Offer {
            source,
            replaces: earlier.map(|earlier| earlier.state.current),
            new_rights,
            new_hooks,
            new_places,
            renamed,
            more_memory,
            manifest,
            manifest_bytes: fetched.manifest,
            wasm: fetched.wasm,
            machine_code,
        })
    }

    /// Whether the component imports only what its rights cover and what this app links, and exports its page and
    /// every hook its manifest provides.
    fn check_component(&self, component: &Component, manifest: &Manifest) -> Result<()> {
        let engine = &self.machine.engine;
        let kind = component.component_type();
        for (name, _) in kind.imports(engine) {
            let interface = name.split('@').next().unwrap_or(name);
            if interface.starts_with("wasi:") {
                continue;
            }
            let right = if let Some(shared) = interface.strip_prefix(&format!("{SHARED_PACKAGE}/"))
            {
                (!FREE_INTERFACES.contains(&shared)).then_some(shared)
            } else if let Some(own) = interface.strip_prefix(&format!("{}/", self.app.package)) {
                Some(own)
            } else {
                return Err(Error::Component(format!(
                    "it imports {name}, which {} does not offer",
                    self.app.name
                )));
            };
            if let Some(right) = right
                && !manifest.rights.iter().any(|asked| asked == right)
            {
                return Err(Error::Component(format!(
                    "it imports {name} without asking for the right {right}"
                )));
            }
        }
        let exports: Vec<(String, ComponentItem)> = kind
            .exports(engine)
            .map(|(name, item)| (name.to_owned(), item.ty))
            .collect();
        let interface = |wanted: &str| {
            exports
                .iter()
                .find(|(name, _)| name == wanted || name.starts_with(&format!("{wanted}@")))
        };
        if interface(&format!("{SHARED_PACKAGE}/page")).is_none() {
            return Err(Error::Component(format!(
                "it exports no {SHARED_PACKAGE}/page"
            )));
        }
        if !manifest.provides.is_empty() {
            let hooks = self.app.hooks_interface();
            let Some((_, ComponentItem::ComponentInstance(instance))) = interface(&hooks) else {
                return Err(Error::Component(format!(
                    "it provides hooks but exports no {hooks}"
                )));
            };
            let functions: Vec<&str> = instance.exports(engine).map(|(name, _)| name).collect();
            if let Some(hook) = manifest
                .provides
                .iter()
                .find(|hook| !functions.contains(&hook.as_str()))
            {
                return Err(Error::Component(format!(
                    "it provides {hook} but does not export it"
                )));
            }
        }
        self.linker
            .instantiate_pre(component)
            .map_err(|err| Error::Component(format!("{err:#}")))?;
        Ok(())
    }

    fn state(&self, id: &str) -> Result<State> {
        if !manifest::is_id(id) {
            return Err(Error::Missing(id.to_owned()));
        }
        let path = self.root.join(id).join(STATE);
        let bytes = match std::fs::read(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Missing(id.to_owned()));
            }
            other => other?,
        };
        serde_json::from_slice(&bytes)
            .map_err(|err| Error::Io(std::io::Error::other(format!("{}: {err}", path.display()))))
    }

    fn manifest(&self, id: &str, version: &Version) -> Result<Manifest> {
        let bytes = std::fs::read(self.version_folder(id, version).join(MANIFEST))?;
        let manifest = Manifest::read(&bytes, &self.app)?;
        if manifest.id != id || &manifest.version != version {
            return Err(Error::Manifest(format!(
                "the files of {id} {version} hold another plugin"
            )));
        }
        Ok(manifest)
    }

    fn version_folder(&self, id: &str, version: &Version) -> PathBuf {
        self.root.join(id).join(VERSIONS).join(version.to_string())
    }

    /// Changes `id`'s state on disk.
    fn change(&self, id: &str, change: impl FnOnce(&mut State)) -> Result<()> {
        let _changing = lock(&self.changing);
        let mut state = self.state(id)?;
        change(&mut state);
        self.write_state(id, &state)?;
        self.forget(id);
        Ok(())
    }

    fn write_state(&self, id: &str, state: &State) -> Result<()> {
        let folder = self.root.join(id);
        write_atomically(
            &folder.join(STATE),
            &serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?,
        )
    }

    /// Deletes every version but the current and the previous one.
    fn keep_two(&self, id: &str, state: &mut State) -> Result<()> {
        let keep: Vec<String> = std::iter::once(&state.current)
            .chain(&state.previous)
            .map(Version::to_string)
            .collect();
        state.compiled.retain(|version, _| keep.contains(version));
        for entry in std::fs::read_dir(self.root.join(id).join(VERSIONS))?.flatten() {
            if let Ok(name) = entry.file_name().into_string()
                && !keep.contains(&name)
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    fn forget(&self, id: &str) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.loaded).remove(id);
        *lock(&self.providers) = None;
    }
}

/// A function a plugin exports, found by its interface at whatever version the plugin was built against.
struct Export<'a> {
    instance: &'a Instance,
    index: wasmtime::component::ComponentExportIndex,
}

impl<'a> Export<'a> {
    fn find(
        instance: &'a Instance,
        store: &mut Store<Plugged>,
        loaded: &Loaded,
        interface: &str,
        function: &str,
    ) -> wasmtime::Result<Self> {
        let missing = || wasmtime::format_err!("it does not export {interface}.{function}");
        let versioned = format!("{interface}@");
        let name = loaded
            .exports
            .iter()
            .find(|name| *name == interface || name.starts_with(&versioned))
            .ok_or_else(missing)?;
        let outer = instance
            .get_export_index(&mut *store, None, name)
            .ok_or_else(missing)?;
        let index = instance
            .get_export_index(&mut *store, Some(&outer), function)
            .ok_or_else(missing)?;
        Ok(Self { instance, index })
    }

    fn typed<Params, Results>(
        &self,
        store: &mut Store<Plugged>,
    ) -> wasmtime::Result<wasmtime::component::TypedFunc<Params, Results>>
    where
        Params: ComponentNamedList + Lower,
        Results: ComponentNamedList + Lift,
    {
        self.instance
            .get_typed_func::<Params, Results>(&mut *store, &self.index)
    }
}

/// Makes `folder` if it is not there, readable by its owner alone: the plugins' machine code is loaded from it.
fn private_folder(folder: &Path) -> Result<()> {
    std::fs::create_dir_all(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn conflict(id: &str, source: &Source) -> Error {
    Error::Conflict(format!(
        "a plugin named {id} is already installed from {}",
        source.shown()
    ))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
