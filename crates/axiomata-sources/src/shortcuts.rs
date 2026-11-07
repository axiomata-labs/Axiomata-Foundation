//! The owner's shortcuts (§8k Q8): their names, and running one — through Apple's
//! own `shortcuts` command, the name as one argument, an input as a file of its own, never through a shell. What a
//! shortcut does is the owner's: a caller runs only those the owner allowed, each after a yes.

use std::io::{Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long listing may take.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a shortcut may run.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest input handed to a shortcut.
pub const MAX_INPUT_CHARS: usize = 2000;
/// The most of a shortcut's output read.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;

/// Numbers the input files of this process: `create_new` refuses one that is there, never writes through it.
static NEXT_INPUT: AtomicU64 = AtomicU64::new(0);

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutError {
    Invalid(&'static str),
    TimedOut,
    /// It ran and failed; only that, never its message (it may quote what it worked on).
    Failed,
}

impl std::fmt::Display for ShortcutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShortcutError::Invalid(why) => write!(f, "{why}"),
            ShortcutError::TimedOut => write!(f, "the shortcut did not finish in time"),
            ShortcutError::Failed => write!(f, "the shortcut failed"),
        }
    }
}

impl std::error::Error for ShortcutError {}

/// The names of the owner's shortcuts.
///
/// # Errors
///
/// When `shortcuts` does not answer.
pub fn list() -> Result<Vec<String>, ShortcutError> {
    let out = run(Command::new("/usr/bin/shortcuts").arg("list"), LIST_TIMEOUT)?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(str::trim)
        .filter(|name| plain(name))
        .map(str::to_owned)
        .collect())
}

/// Runs the shortcut `name`, with `input` as text when given; its output as text, cut.
///
/// # Errors
///
/// [`ShortcutError::Invalid`] for a name that is none or an input too long, else see [`ShortcutError`].
pub fn run_shortcut(name: &str, input: Option<&str>) -> Result<String, ShortcutError> {
    if !plain(name) || name.starts_with('-') {
        return Err(ShortcutError::Invalid("no such shortcut"));
    }
    let mut command = Command::new("/usr/bin/shortcuts");
    command.arg("run").arg(name);
    // The input as a file only this account reads, gone afterwards.
    let file = match input.map(str::trim).filter(|input| !input.is_empty()) {
        Some(input) => {
            if input.chars().count() > MAX_INPUT_CHARS {
                return Err(ShortcutError::Invalid("the input is too long"));
            }
            // What the owner heard is what the shortcut gets: nothing hidden in it.
            if input
                .chars()
                .any(|c| (c.is_control() && c != '\n') || crate::calendar::invisible(c))
            {
                return Err(ShortcutError::Invalid("the input holds hidden characters"));
            }
            let path = std::env::temp_dir().join(format!(
                "shortcut-input-{}-{}.txt",
                std::process::id(),
                NEXT_INPUT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|_| ShortcutError::Failed)?;
            if file.write_all(input.as_bytes()).is_err() {
                let _ = std::fs::remove_file(&path);
                return Err(ShortcutError::Failed);
            }
            command.arg("--input-path").arg(&path);
            Some(path)
        }
        None => None,
    };
    let ran = run(&mut command, RUN_TIMEOUT);
    if let Some(path) = file {
        let _ = std::fs::remove_file(path);
    }
    let out = ran?;
    Ok(String::from_utf8_lossy(&out).chars().take(2000).collect())
}

/// Whether `name` is a shortcut's name to take: one line, nothing hidden, not too long.
fn plain(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 200
        && !name
            .chars()
            .any(|c| c.is_control() || crate::calendar::invisible(c))
}

fn run(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, ShortcutError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ShortcutError::Failed)?;
    let mut stdout = child.stdout.take().ok_or(ShortcutError::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT_BYTES).read_to_end(&mut out);
        // The rest is read and dropped: a shortcut that says much must not fail on a closed pipe.
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ShortcutError::TimedOut);
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(ShortcutError::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_is_none_runs_nothing() {
        for name in ["", "-h", "a\nb", "x\u{202e}", &"n".repeat(201)] {
            assert_eq!(
                run_shortcut(name, None),
                Err(ShortcutError::Invalid("no such shortcut")),
                "{name:?}"
            );
        }
        assert_eq!(
            run_shortcut("Wetter", Some("Licht\u{e0041}an")),
            Err(ShortcutError::Invalid("the input holds hidden characters"))
        );
        assert_eq!(
            run_shortcut("Wetter", Some(&"x".repeat(MAX_INPUT_CHARS + 1))),
            Err(ShortcutError::Invalid("the input is too long"))
        );
    }

    #[test]
    #[ignore = "lists this Mac's shortcuts; run by hand"]
    fn on_this_mac() {
        let started = Instant::now();
        let names = list().unwrap();
        eprintln!("{} shortcuts in {:?}", names.len(), started.elapsed());
    }
}
