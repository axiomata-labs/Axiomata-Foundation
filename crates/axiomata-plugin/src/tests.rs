//! The plugin host against the tests' plugin (`tests/probe.wasm`, built by `tests/guest/build.sh`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

const PROBE: &[u8] = include_bytes!("../tests/probe.wasm");
const HOOKS: &[&str] = &[
    "echo", "spin", "nap", "grow", "save", "load", "work", "shout", "language",
];

type Saved = (Result<(), String>,);
type Loaded_ = (Result<Option<String>, String>,);

fn app() -> App {
    App {
        name: "probe".into(),
        package: "axiomata:probe".into(),
        interfaces: vec![Version::new(0, 1, 0)],
        hooks: HOOKS.iter().map(|hook| (*hook).to_owned()).collect(),
        rights: Vec::new(),
    }
}

fn manifest(version: &str) -> Manifest {
    Manifest {
        id: "probe".into(),
        name: "Probe".into(),
        version: version.parse().expect("a version"),
        app: "probe".into(),
        interface: Version::new(0, 1, 0),
        description: "The tests' plugin".into(),
        author: "Axiomata".into(),
        rights: vec![STORAGE.into()],
        provides: HOOKS.iter().map(|hook| (*hook).to_owned()).collect(),
        memory_mib: Some(16),
        sha256: sha256(PROBE),
    }
}

/// A plugin's release in a folder.
fn release(folder: &Path, manifest: &Manifest, wasm: &[u8]) {
    std::fs::create_dir_all(folder).expect("a folder");
    std::fs::write(
        folder.join(MANIFEST),
        serde_json::to_vec_pretty(manifest).expect("json"),
    )
    .expect("written");
    std::fs::write(folder.join(WASM), wasm).expect("written");
}

struct Bench {
    dir: tempfile::TempDir,
    plugins: Plugins,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Bench {
    fn new(limit: Duration) -> Self {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&lines);
        let log: LogSink = Arc::new(move |id, _, line| lock(&kept).push(format!("{id}: {line}")));
        let plugins = Plugins::open(
            dir.path().join("plugins"),
            app(),
            Limits { call: limit },
            "de",
            log,
        )
        .expect("the plugins open");
        Self {
            dir,
            plugins,
            lines,
        }
    }

    /// Puts `manifest` with the probe into the release folder and returns what the user would type for it.
    fn release(&self, manifest: &Manifest) -> String {
        let folder = self.dir.path().join("release");
        release(&folder, manifest, PROBE);
        folder.display().to_string()
    }

    fn installed(limit: Duration) -> Self {
        let bench = Self::new(limit);
        let typed = bench.release(&manifest("0.1.0"));
        let offer = bench.plugins.fetch(&typed).expect("fetched");
        bench.plugins.install(offer).expect("installed");
        bench
    }

    fn echo(&self, text: &str) -> Result<String> {
        self.plugins
            .call::<(String,), (String,)>("probe", "echo", (text.to_owned(),))
            .map(|(text,)| text)
    }

    fn save(&self, key: &str, value: &str) -> Result<(), String> {
        self.plugins
            .call::<(String, String), Saved>("probe", "save", (key.into(), value.into()))
            .expect("a call")
            .0
    }

    fn load(&self, key: &str) -> Result<Option<String>, String> {
        self.plugins
            .call::<(String,), Loaded_>("probe", "load", (key.into(),))
            .expect("a call")
            .0
    }
}

#[test]
fn a_plugin_from_a_folder_is_offered_with_its_rights_then_installed_and_answers() {
    let bench = Bench::new(Duration::from_secs(2));
    let offer = bench
        .plugins
        .fetch(&bench.release(&manifest("0.1.0")))
        .expect("fetched");
    assert_eq!(offer.manifest.rights, vec![STORAGE.to_owned()]);
    assert_eq!(offer.new_rights, vec![STORAGE.to_owned()]);
    assert_eq!(offer.replaces, None);
    assert_eq!(offer.publisher(), "local");
    assert!(!offer.is_ours());
    assert!(
        bench.plugins.list().is_empty(),
        "nothing is installed before the yes"
    );

    let installed = bench.plugins.install(offer).expect("installed");
    assert!(installed.state.enabled);
    assert_eq!(installed.state.granted, vec![STORAGE.to_owned()]);
    assert_eq!(bench.plugins.list().len(), 1);
    assert_eq!(bench.plugins.providing("echo"), vec!["probe".to_owned()]);
    assert_eq!(bench.echo("Grüß dich").expect("an answer"), "Grüß dich");
}

#[test]
fn a_hanging_plugin_is_ended_at_its_time_limit_and_the_next_call_runs() {
    let limit = Duration::from_millis(200);
    let bench = Bench::installed(limit);
    let started = Instant::now();
    let err = bench
        .plugins
        .call::<(), ()>("probe", "spin", ())
        .expect_err("it never returns by itself");
    let took = started.elapsed();
    assert!(err.to_string().contains("took longer than 0.2 s"), "{err}");
    assert!(took >= limit && took < limit * 3, "ended after {took:?}");
    assert_eq!(bench.echo("weiter").expect("the next call runs"), "weiter");
}

#[test]
fn a_plugin_that_waits_on_the_host_is_ended_at_once() {
    let bench = Bench::installed(Duration::from_secs(2));
    let started = Instant::now();
    let err = bench
        .plugins
        .call::<(), ()>("probe", "nap", ())
        .expect_err("it may not sleep");
    assert!(err.to_string().contains("wanted to wait"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "ended after {:?}",
        started.elapsed()
    );
    assert_eq!(bench.echo("wach").expect("the next call runs"), "wach");
}

#[test]
fn a_page_longer_than_a_page_may_hold_is_not_read() {
    let bench = Bench::installed(Duration::from_secs(2));
    bench.save("big", "yes").expect("saved");
    let err = bench.plugins.page("probe").expect_err("too long");
    assert!(err.to_string().contains("longer than"), "{err}");
}

#[test]
fn a_greedy_plugin_gets_no_more_memory_than_its_manifest_says() {
    let bench = Bench::installed(Duration::from_secs(2));
    let (got,) = bench
        .plugins
        .call::<(u32,), (u32,)>("probe", "grow", (4,))
        .expect("4 MiB fit into 16");
    assert_eq!(got, 4);
    let err = bench
        .plugins
        .call::<(u32,), (u32,)>("probe", "grow", (64,))
        .expect_err("64 MiB do not");
    assert!(
        err.to_string().contains("more memory than it may use"),
        "{err}"
    );
    assert_eq!(
        bench.echo("noch da").expect("the next call runs"),
        "noch da"
    );
}

#[test]
fn storage_lasts_from_call_to_call_and_a_right_taken_back_closes_it() {
    let bench = Bench::installed(Duration::from_secs(2));
    assert_eq!(bench.save("words", "Lirfel → Lirvel"), Ok(()));
    assert_eq!(
        bench.load("words"),
        Ok(Some("Lirfel → Lirvel".into())),
        "a fresh instance reads it"
    );
    assert_eq!(bench.load("missing"), Ok(None));
    assert!(
        bench.dir.path().join("plugins/probe/data/words").is_file(),
        "kept in its own folder"
    );

    for bad in ["../escape", ".hidden", "Großbuchstaben", "", "a/b"] {
        assert!(bench.save(bad, "x").is_err(), "{bad:?} is no key");
    }
    assert!(!bench.dir.path().join("plugins/probe/escape").exists());

    bench
        .plugins
        .set_right("probe", STORAGE, false)
        .expect("taken back");
    assert_eq!(bench.load("words"), Err("not granted: storage".into()));
    bench
        .plugins
        .set_right("probe", STORAGE, true)
        .expect("granted again");
    assert_eq!(bench.load("words"), Ok(Some("Lirfel → Lirvel".into())));
    assert!(
        bench.plugins.set_right("probe", "network", true).is_err(),
        "not a right it asked for"
    );
}

#[test]
fn its_page_keeps_what_was_typed_when_the_button_is_clicked() {
    let bench = Bench::installed(Duration::from_secs(2));
    let page = bench.plugins.page("probe").expect("a page");
    assert!(matches!(&page.nodes[0].block, ui::Block::Card(title) if title == "Probe"));
    let event = ui::Event {
        action: ui::Action::Clicked("keep".into()),
        fields: vec![("note".into(), "Kaffee holen".into())],
    };
    let page = bench
        .plugins
        .handle("probe", &event)
        .expect("the page after");
    assert!(matches!(&page.nodes[1].block, ui::Block::Text(text) if text == "Kaffee holen"));
    let page = bench.plugins.page("probe").expect("the page again");
    assert!(matches!(&page.nodes[1].block, ui::Block::Text(text) if text == "Kaffee holen"));
}

#[test]
fn a_loud_plugin_writes_at_most_a_hundred_lines_a_call_and_hears_the_app_s_language() {
    let bench = Bench::installed(Duration::from_secs(2));
    bench
        .plugins
        .call::<(u32,), ()>("probe", "shout", (150,))
        .expect("it shouts");
    let lines = lock(&bench.lines).clone();
    assert_eq!(lines.len(), 100);
    assert!(lines[0].starts_with("probe: line 0"));
    assert!(lines[99].ends_with("[… more lines dropped]"));

    let language = || {
        bench
            .plugins
            .call::<(), (String,)>("probe", "language", ())
            .expect("a call")
            .0
    };
    assert_eq!(language(), "de");
    bench.plugins.set_language("en");
    assert_eq!(language(), "en");
}

#[test]
fn a_plugin_switched_off_is_not_called() {
    let bench = Bench::installed(Duration::from_secs(2));
    bench
        .plugins
        .set_enabled("probe", false)
        .expect("switched off");
    assert!(matches!(bench.echo("hallo"), Err(Error::Off(_))));
    assert!(bench.plugins.providing("echo").is_empty());
    bench
        .plugins
        .set_enabled("probe", true)
        .expect("switched on");
    assert_eq!(bench.echo("hallo").expect("an answer"), "hallo");
}

#[test]
fn what_does_not_fit_is_refused_before_the_user_is_asked() {
    let bench = Bench::new(Duration::from_secs(2));
    let refused = |manifest: &Manifest, wasm: &[u8]| {
        let folder = bench.dir.path().join("refused");
        let _ = std::fs::remove_dir_all(&folder);
        release(&folder, manifest, wasm);
        bench
            .plugins
            .fetch(&folder.display().to_string())
            .expect_err("refused")
    };

    let mut tampered = PROBE.to_vec();
    tampered.push(0);
    assert!(matches!(
        refused(&manifest("0.1.0"), &tampered),
        Error::Checksum
    ));

    let without_right = Manifest {
        rights: Vec::new(),
        ..manifest("0.1.0")
    };
    let err = refused(&without_right, PROBE);
    assert!(
        err.to_string()
            .contains("without asking for the right storage"),
        "{err}"
    );

    let other_app = Manifest {
        app: "glut".into(),
        ..manifest("0.1.0")
    };
    assert!(matches!(refused(&other_app, PROBE), Error::WrongApp(app) if app == "glut"));

    let newer = Manifest {
        interface: Version::new(0, 2, 0),
        ..manifest("0.1.0")
    };
    assert!(matches!(refused(&newer, PROBE), Error::NewerApp(_)));

    let mut unknown_hook = manifest("0.1.0");
    unknown_hook.provides.push("translate".into());
    assert!(matches!(refused(&unknown_hook, PROBE), Error::Manifest(_)));

    let unknown_right = Manifest {
        rights: vec!["network".into()],
        ..manifest("0.1.0")
    };
    assert!(matches!(refused(&unknown_right, PROBE), Error::Manifest(_)));

    let bad_id = Manifest {
        id: "../probe".into(),
        ..manifest("0.1.0")
    };
    assert!(matches!(refused(&bad_id, PROBE), Error::Manifest(_)));

    let not_wasm = b"\0asm not really";
    let not_wasm_manifest = Manifest {
        sha256: sha256(not_wasm),
        ..manifest("0.1.0")
    };
    assert!(matches!(
        refused(&not_wasm_manifest, not_wasm),
        Error::Component(_)
    ));
    assert!(bench.plugins.list().is_empty());
}

#[test]
fn an_update_keeps_the_version_before_and_its_storage_and_goes_back_and_forth() {
    let bench = Bench::installed(Duration::from_secs(2));
    bench.save("k", "v").expect("saved");
    assert!(
        bench.plugins.check("probe").expect("checked").is_none(),
        "no newer release yet"
    );

    bench.release(&manifest("0.2.0"));
    let offer = bench
        .plugins
        .check("probe")
        .expect("checked")
        .expect("a newer release");
    let noted = bench
        .plugins
        .installed("probe")
        .expect("installed")
        .state
        .available;
    assert_eq!(
        noted,
        Some(Version::new(0, 2, 0)),
        "what a check found lasts beyond the window"
    );
    assert_eq!(offer.replaces, Some(Version::new(0, 1, 0)));
    assert!(offer.new_rights.is_empty());
    let installed = bench.plugins.install(offer).expect("updated");
    assert_eq!(installed.state.current, Version::new(0, 2, 0));
    assert_eq!(installed.state.previous, Some(Version::new(0, 1, 0)));
    assert_eq!(installed.state.available, None);

    let back = bench.plugins.roll_back("probe").expect("back");
    assert_eq!(back.state.current, Version::new(0, 1, 0));
    assert_eq!(back.state.previous, Some(Version::new(0, 2, 0)));
    assert_eq!(bench.load("k"), Ok(Some("v".into())), "the storage stays");
    assert_eq!(back.state.held_back, Some(Version::new(0, 2, 0)));
    assert!(
        bench.plugins.check("probe").expect("checked").is_none(),
        "going back is not undone by a check"
    );
    let forth = bench.plugins.roll_back("probe").expect("forth again");
    assert_eq!(forth.state.current, Version::new(0, 2, 0));
    assert_eq!(forth.state.held_back, None);

    bench.release(&manifest("0.3.0"));
    bench
        .plugins
        .install(
            bench
                .plugins
                .check("probe")
                .expect("checked")
                .expect("newer"),
        )
        .expect("updated");
    let versions: Vec<String> = std::fs::read_dir(bench.dir.path().join("plugins/probe/versions"))
        .expect("versions")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        versions.len(),
        2,
        "only the current and the previous version stay: {versions:?}"
    );
}

#[test]
fn an_update_installs_itself_only_where_the_user_allowed_it() {
    let bench = Bench::installed(Duration::from_secs(2));
    bench
        .plugins
        .set_auto_update("probe", true)
        .expect("allowed");
    bench
        .plugins
        .change("probe", |state| state.checked = None)
        .expect("due");

    bench.release(&manifest("0.2.0"));
    let checked = bench.plugins.check_due();
    assert!(
        matches!(&checked[..], [Checked::Updated { version, .. }] if version.minor == 2),
        "{checked:?}"
    );
    assert!(
        bench.plugins.check_due().is_empty(),
        "checked within the day"
    );

    let more = Manifest {
        memory_mib: Some(32),
        ..manifest("0.2.1")
    };
    bench.release(&more);
    bench
        .plugins
        .change("probe", |state| state.checked = None)
        .expect("due");
    let checked = bench.plugins.check_due();
    assert!(
        matches!(&checked[..], [Checked::Waiting(offer)] if offer.more_memory),
        "{checked:?}"
    );

    bench
        .plugins
        .set_auto_update("probe", false)
        .expect("not allowed");
    bench
        .plugins
        .change("probe", |state| state.checked = None)
        .expect("due");
    bench.release(&manifest("0.3.0"));
    let checked = bench.plugins.check_due();
    assert!(
        matches!(&checked[..], [Checked::Waiting(offer)] if offer.manifest.version.minor == 3),
        "{checked:?}"
    );
}

#[test]
fn machine_code_changed_on_disk_is_translated_again_from_the_checked_component() {
    let bench = Bench::installed(Duration::from_secs(2));
    let folder = bench.dir.path().join("plugins/probe/versions/0.1.0");
    let code = folder.join(MACHINE_CODE);
    let mut bytes = std::fs::read(&code).expect("machine code");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&code, &bytes).expect("changed");
    bench.plugins.forget("probe");

    assert_eq!(bench.echo("trotzdem").expect("it runs"), "trotzdem");
    let state = bench.plugins.state("probe").expect("a state");
    let now = std::fs::read(&code).expect("machine code");
    assert_eq!(
        state.compiled.get("0.1.0"),
        Some(&sha256(&now)),
        "translated again and noted"
    );

    std::fs::write(folder.join(WASM), b"changed").expect("changed");
    std::fs::write(&code, b"changed").expect("changed");
    bench.plugins.forget("probe");
    assert!(
        matches!(bench.echo("nein"), Err(Error::Checksum)),
        "a changed component never runs"
    );
}

#[test]
fn another_plugin_under_the_same_id_is_refused_and_removing_takes_its_storage() {
    let bench = Bench::installed(Duration::from_secs(2));
    let elsewhere = bench.dir.path().join("elsewhere");
    release(&elsewhere, &manifest("0.2.0"), PROBE);
    let err = bench
        .plugins
        .fetch(&elsewhere.display().to_string())
        .expect_err("another source");
    assert!(matches!(err, Error::Conflict(_)), "{err}");

    bench.save("k", "v").expect("saved");
    bench.plugins.remove("probe").expect("removed");
    assert!(!bench.dir.path().join("plugins/probe").exists());
    assert!(matches!(bench.echo("weg"), Err(Error::Missing(_))));
}

#[test]
fn what_the_user_types_names_a_source() {
    let github = |owner: &str, repo: &str| Source::GitHub {
        owner: owner.into(),
        repo: repo.into(),
    };
    let parsed = |typed: &str| Source::parse(typed).expect("a source");
    assert_eq!(
        parsed("axiomata-labs/hauch-wortfilter"),
        github("axiomata-labs", "hauch-wortfilter")
    );
    assert_eq!(
        parsed(" https://github.com/someone/thing.git/ "),
        github("someone", "thing")
    );
    assert!(parsed("axiomata-labs/hauch-wortfilter").is_ours());
    assert!(!parsed("someone/axiomata-labs").is_ours());
    assert_eq!(
        parsed("https://example.org/releases/v1/plugin.json"),
        Source::Address {
            url: "https://example.org/releases/v1/plugin.json".into()
        }
    );
    assert_eq!(
        parsed("https://example.org/plugin.json").publisher(),
        "example.org"
    );
    for refused in [
        "http://example.org/plugin.json",
        "https://example.org/plugin.wasm",
        "https://user:secret@example.org/plugin.json",
        "https://example.org/plugin.json?x=1",
        "a/b/c",
        "x/y z",
        ".hidden/repo",
        "nothing",
    ] {
        assert!(Source::parse(refused).is_err(), "{refused:?}");
    }
    assert!(matches!(parsed("/tmp/plugin"), Source::Folder { .. }));
    assert!(matches!(parsed("../plugin/dist"), Source::Folder { path } if path.is_absolute()));
    assert_eq!(parsed("someone/thing.git"), github("someone", "thing"));
    let shared = parsed("https://github.com/evil/x/releases/download/v1/plugin.json");
    assert_eq!(
        shared.publisher(),
        "github.com/evil",
        "a shared host is named with whose path it is"
    );
    assert!(!shared.is_ours());
}

#[test]
fn an_app_runs_plugins_built_against_an_older_compatible_interface() {
    let app = App {
        interfaces: vec![Version::new(0, 1, 3), Version::new(1, 4, 0)],
        ..app()
    };
    assert!(app.speaks(&Version::new(0, 1, 0)));
    assert!(app.speaks(&Version::new(0, 1, 3)));
    assert!(!app.speaks(&Version::new(0, 1, 4)), "newer than the app");
    assert!(
        !app.speaks(&Version::new(0, 2, 0)),
        "another minor while the major is 0"
    );
    assert!(app.speaks(&Version::new(1, 0, 0)));
    assert!(!app.speaks(&Version::new(2, 0, 0)));
}

#[test]
fn a_page_whose_blocks_do_not_fit_together_is_not_drawn() {
    let node = |parent: Option<u32>, block: ui::Block| ui::Node { parent, block };
    let text = |text: &str| ui::Block::Text(text.into());
    let fine = ui::Page {
        nodes: vec![
            node(None, ui::Block::Card("A".into())),
            node(Some(0), text("b")),
        ],
    };
    assert!(page::check(&fine).is_ok());
    let forward = ui::Page {
        nodes: vec![node(Some(1), text("a")), node(None, ui::Block::Row)],
    };
    assert!(page::check(&forward).is_err());
    let in_text = ui::Page {
        nodes: vec![node(None, text("a")), node(Some(0), text("b"))],
    };
    assert!(page::check(&in_text).is_err());
    let picture = ui::Block::Picture(ui::Picture {
        bytes: b"<svg/>".to_vec(),
        description: "x".into(),
    });
    assert!(
        page::check(&ui::Page {
            nodes: vec![node(None, picture)]
        })
        .is_err()
    );
    let choice = ui::Block::Choice(ui::Choice {
        id: "c".into(),
        label: "C".into(),
        options: vec!["a".into()],
        chosen: Some(1),
    });
    assert!(
        page::check(&ui::Page {
            nodes: vec![node(None, choice)]
        })
        .is_err()
    );
}

/// plugins.md Q6, measured: what wasmtime's epochs cost a busy plugin, and a fresh instance per call. Run with
/// `cargo test --release -p axiomata-plugin -- --ignored --nocapture measure`.
#[test]
#[ignore = "a measurement, not a test"]
fn measure_the_cost_of_interrupting_and_of_a_fresh_instance() {
    use wasmtime::component::Component;
    use wasmtime::{Config, Engine};

    const ROUNDS: u64 = 200_000_000;
    const TIMES: usize = 7;
    let median = |mut samples: Vec<Duration>| {
        samples.sort();
        samples[samples.len() / 2]
    };
    let log: LogSink = Arc::new(|_, _, _| {});
    for epochs in [false, true] {
        let mut config = Config::new();
        config.epoch_interruption(epochs);
        let engine = Engine::new(&config).expect("an engine");
        let component = Component::new(&engine, PROBE).expect("the probe");
        let linker = host::linker(&engine).expect("a linker");
        let pre = linker.instantiate_pre(&component).expect("linked");
        let fresh = || {
            let plugged = Plugged::new("probe", 64 << 20, None, "de".into(), Arc::clone(&log));
            let mut store = plugged.store(&engine, Duration::from_secs(60));
            let instance = pre.instantiate(&mut store).expect("an instance");
            (store, instance)
        };
        let call = |rounds: u64| {
            let (mut store, instance) = fresh();
            let hooks = instance
                .get_export_index(&mut store, None, "axiomata:probe/hooks@0.1.0")
                .expect("hooks");
            let work = instance
                .get_export_index(&mut store, Some(&hooks), "work")
                .expect("work");
            let typed = instance
                .get_typed_func::<(u64,), (u64,)>(&mut store, &work)
                .expect("typed");
            let started = Instant::now();
            typed.call(&mut store, (rounds,)).expect("worked");
            started.elapsed()
        };
        let work = median((0..TIMES).map(|_| call(ROUNDS)).collect());
        let instance = median(
            (0..TIMES * 20)
                .map(|_| {
                    let started = Instant::now();
                    drop(fresh());
                    started.elapsed()
                })
                .collect(),
        );
        println!(
            "epochs {epochs}: {ROUNDS} rounds {:.1} ms, fresh instance {:.1} µs",
            work.as_secs_f64() * 1e3,
            instance.as_secs_f64() * 1e6
        );
    }
}

#[test]
fn a_picture_is_measured_from_its_header_and_a_page_nests_at_most_eight_deep() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&640u32.to_be_bytes());
    png.extend_from_slice(&480u32.to_be_bytes());
    assert_eq!(page::dimensions(&png), Some((640, 480)));
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8, 0x01, 0xE0, 0x02, 0x80, 3,
    ];
    assert_eq!(page::dimensions(&jpeg), Some((640, 480)));
    assert_eq!(page::dimensions(b"<svg/>"), None);

    let mut huge = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    huge.extend_from_slice(&100_000u32.to_be_bytes());
    huge.extend_from_slice(&100_000u32.to_be_bytes());
    let picture = ui::Block::Picture(ui::Picture {
        bytes: huge,
        description: "x".into(),
    });
    let node = |parent: Option<u32>, block: ui::Block| ui::Node { parent, block };
    assert!(
        page::check(&ui::Page {
            nodes: vec![node(None, picture)]
        })
        .is_err(),
        "too large to decode"
    );

    let deep: Vec<ui::Node> = (0..10u32)
        .map(|level| node(level.checked_sub(1), ui::Block::Card(format!("{level}"))))
        .collect();
    assert!(
        page::check(&ui::Page {
            nodes: deep[..8].to_vec()
        })
        .is_ok()
    );
    assert!(page::check(&ui::Page { nodes: deep }).is_err());
}

#[test]
fn a_name_that_hides_what_it_says_is_refused() {
    let hidden = Manifest {
        name: "Wortfilter\u{202E}retliftroW".into(),
        ..manifest("0.1.0")
    };
    assert!(matches!(hidden.check(&app()), Err(Error::Manifest(_))));
}
