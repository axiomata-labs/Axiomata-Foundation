//! How fast the text core is on large files (rust-core.md R5): opening, typing, undoing, and one very long line.
//!
//! `cargo run --release -p axiomata-editor --example bench [-- FILE…]` measures generated texts and, if given, real
//! files. Every number is the median of a few runs, in milliseconds, or in microseconds for one keystroke.

use std::time::{Duration, Instant};

use axiomata_editor::{
    Change, Document, EditKind, Indent, IndentKind, Millis, Pos, Range, Selection,
};

const FALLBACK: Indent = Indent {
    kind: IndentKind::Spaces,
    size: 4,
};
/// Keystrokes per typing run.
const KEYS: usize = 2000;
const RUNS: usize = 5;

/// A small deterministic generator, so two runs type at the same places.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap_or(0) % n.max(1)
    }
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn us(d: Duration) -> String {
    format!("{:.2} µs", d.as_secs_f64() * 1_000_000.0)
}

/// One typed character at the cursor, as the editor's insert command does it.
fn type_char(doc: &mut Document, ch: &str, now: u64) {
    let at = doc.selection().head;
    let after = Selection::cursor(Pos::new(at.line, at.col + ch.len()));
    let change = Change {
        range: Range::at(at),
        text: ch.to_owned(),
    };
    if let Err(e) = doc.edit(&[change], after, EditKind::Typing, Millis(now)) {
        eprintln!("an edit was refused: {e}");
    }
}

/// Opens `text`, types `KEYS` characters at random lines (a jump every 20 keys), undoes them all, and reports.
fn measure(name: &str, text: &str) {
    let mut open = Vec::new();
    let mut typing = Vec::new();
    let mut undo = Vec::new();
    let mut save = Vec::new();
    for run in 0..RUNS {
        let t = Instant::now();
        let mut doc = Document::new(text, FALLBACK);
        open.push(t.elapsed());

        let mut rnd = Lcg(run as u64 + 1);
        let t = Instant::now();
        for k in 0..KEYS {
            if k % 20 == 0 {
                let line = rnd.below(doc.rope().line_count());
                let len = doc.rope().line(line).map_or(0, str::len);
                let col = doc.rope().clamp(Pos::new(line, rnd.below(len + 1))).col;
                doc.set_selection(Selection::cursor(Pos::new(line, col)), false);
            }
            type_char(&mut doc, "x", k as u64 * 100);
        }
        typing.push(t.elapsed() / u32::try_from(KEYS).unwrap_or(1));

        let t = Instant::now();
        while doc.undo().unwrap_or(false) {}
        undo.push(t.elapsed());

        let t = Instant::now();
        let saved = doc.text_for_save();
        save.push(t.elapsed());
        // A file with mixed line endings or a lone CR is saved as LF, so only a clean one comes back byte for byte.
        let shape = axiomata_editor::detect_shape(text);
        let lone_cr = text.matches('\r').count() > text.matches("\r\n").count();
        if !shape.mixed_eol && !lone_cr && saved != text && run == 0 {
            eprintln!("{name}: undoing everything did not give the file back");
        }
    }
    println!(
        "{name:<34} open {:>10}   per key {:>10}   undo {KEYS} keys {:>10}   text for save {:>10}",
        ms(median(open)),
        us(median(typing)),
        ms(median(undo)),
        ms(median(save)),
    );
}

/// Types `KEYS` characters into the middle of one very long line.
fn measure_long_line(bytes: usize) {
    let text = "x".repeat(bytes);
    let mut times = Vec::new();
    for _ in 0..RUNS {
        let mut doc = Document::new(&text, FALLBACK);
        doc.set_selection(Selection::cursor(Pos::new(0, bytes / 2)), false);
        let t = Instant::now();
        for k in 0..KEYS {
            type_char(&mut doc, "y", k as u64 * 100);
        }
        times.push(t.elapsed() / u32::try_from(KEYS).unwrap_or(1));
    }
    println!(
        "{:<34} per key {:>10}",
        format!("one line of {} MB", bytes / 1_000_000),
        us(median(times))
    );
}

fn numbered(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("    let value_{i} = compute(\"line {i}\", äöü, 🙂);"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn main() {
    for lines in [10_000, 100_000, 400_000] {
        let text = numbered(lines);
        measure(&format!("{lines} lines, {} KB", text.len() / 1000), &text);
    }
    measure_long_line(5_000_000);
    for path in std::env::args().skip(1) {
        match std::fs::read_to_string(&path) {
            Ok(text) => measure(&path, &text),
            Err(e) => eprintln!("{path}: {e}"),
        }
    }
}
