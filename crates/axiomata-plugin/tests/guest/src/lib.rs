//! The tests' plugin (`../probe.wasm`): each hook shows one way a plugin can behave; its page keeps what was typed.

wit_bindgen::generate!({ path: "wit", world: "probe", generate_all });

use axiomata::plugin::{log, settings, storage};
use exports::axiomata::plugin::page;
use exports::axiomata::probe::hooks;
use axiomata::plugin::ui::{Action, Block, Button, Event, Field, Node, Page};

/// Where the page keeps what was typed.
const NOTE: &str = "note";
/// Present: the page is too long.
const BIG: &str = "big";

struct Probe;

impl hooks::Guest for Probe {
    fn echo(text: String) -> String {
        text
    }

    fn spin() {
        let mut n = 0u64;
        loop {
            n = std::hint::black_box(n.wrapping_add(1));
        }
    }

    fn nap() {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }

    fn grow(mib: u32) -> u32 {
        let mut held = Vec::new();
        for _ in 0..mib {
            held.push(vec![1u8; 1 << 20]);
        }
        std::hint::black_box(&held);
        held.len() as u32
    }

    fn save(key: String, value: String) -> Result<(), String> {
        storage::set(&key, value.as_bytes())
    }

    fn load(key: String) -> Result<Option<String>, String> {
        Ok(storage::get(&key)?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn work(rounds: u64) -> u64 {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for i in 0..rounds {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x = x.wrapping_add(i);
        }
        x
    }

    fn shout(times: u32) {
        for i in 0..times {
            log::log(log::Level::Info, &format!("line {i}"));
        }
    }

    fn language() -> String {
        settings::language()
    }
}

impl page::Guest for Probe {
    fn show() -> Page {
        let kept = storage::get(NOTE).ok().flatten().map(|b| String::from_utf8_lossy(&b).into_owned());
        // A page whose text is longer than a page may hold, when the tests ask for one.
        if storage::get(BIG).ok().flatten().is_some() {
            return Page { nodes: vec![Node { parent: None, block: Block::Text("x".repeat(20_000)) }] };
        }
        let nodes = vec![
            Node { parent: None, block: Block::Card("Probe".into()) },
            Node { parent: Some(0), block: Block::Text(kept.unwrap_or_default()) },
            Node {
                parent: Some(0),
                block: Block::Field(Field {
                    id: "note".into(),
                    label: "Note".into(),
                    value: String::new(),
                    placeholder: String::new(),
                }),
            },
            Node {
                parent: Some(0),
                block: Block::Button(Button { id: "keep".into(), label: "Keep".into(), primary: true }),
            },
        ];
        Page { nodes }
    }

    fn handle(event: Event) -> Page {
        if let Action::Clicked(id) = &event.action
            && id == "keep"
            && let Some((_, value)) = event.fields.iter().find(|(field, _)| field == "note")
        {
            let _ = storage::set(NOTE, value.as_bytes());
        }
        Self::show()
    }
}

export!(Probe);
