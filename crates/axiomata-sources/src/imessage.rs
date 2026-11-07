//! Sending an iMessage (Lirvel `docs/plans/companion.md` §8l S4c, Q16): to one number or address, through Messages'
//! own scripting — a constant script, the values only as its arguments — and over iMessage only: one who cannot be
//! reached there gets nothing, never an SMS in its place. Reading messages is not here (that is stage 5).

use crate::calendar::invisible;

/// The longest message sent.
pub const MAX_TEXT_CHARS: usize = 2000;
/// How long Messages may take.
#[cfg(target_os = "macos")]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageError {
    /// The owner did not allow this app to use Messages.
    NotAllowed,
    /// A number, address or text that may not go out.
    Invalid(&'static str),
    TimedOut,
    /// Messages failed — the recipient may not be on iMessage; only the error's number, never its text.
    Failed(String),
}

impl std::fmt::Display for MessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageError::NotAllowed => {
                write!(f, "Messages may not be used: the Mac did not allow it")
            }
            MessageError::Invalid(why) => write!(f, "{why}"),
            MessageError::TimedOut => write!(f, "Messages did not answer in time"),
            MessageError::Failed(why) => write!(
                f,
                "the message was not sent ({why}); the recipient may not be on iMessage"
            ),
        }
    }
}

impl std::error::Error for MessageError {}

/// The script: the handle and the text are its arguments; only an iMessage account sends.
#[cfg(target_os = "macos")]
const SCRIPT: &str = r#"
on run argv
  set theHandle to item 1 of argv
  set theText to item 2 of argv
  tell application "Messages"
    set theService to 1st account whose service type = iMessage
    set theBuddy to participant theHandle of theService
    send theText to theBuddy
  end tell
  return "sent"
end run
"#;

/// Whether `handle` is one phone number (a digit, `+` or `(` first, then digits, spaces, dashes, brackets) or one mail
/// address — never anything `osascript` could read as an option.
pub fn plain_handle(handle: &str) -> bool {
    let handle = handle.trim();
    if handle.contains('@') {
        return crate::mail::plain_address(handle);
    }
    let digits = handle.chars().filter(char::is_ascii_digit).count();
    (6..=15).contains(&digits)
        && handle.starts_with(|c: char| c.is_ascii_digit() || c == '+' || c == '(')
        && handle.chars().enumerate().all(|(n, c)| {
            c.is_ascii_digit() || matches!(c, ' ' | '-' | '(' | ')') || (n == 0 && c == '+')
        })
}

/// Sends `text` to `handle` over iMessage.
///
/// # Errors
///
/// [`MessageError::Invalid`] for a handle or text that may not go out, else see [`MessageError`].
pub fn send(handle: &str, text: &str) -> Result<(), MessageError> {
    if !plain_handle(handle) {
        return Err(MessageError::Invalid("no plain number or address"));
    }
    if text.trim().is_empty()
        || text.chars().count() > MAX_TEXT_CHARS
        || text
            .chars()
            .any(|c| (c.is_control() && c != '\n') || invisible(c))
    {
        return Err(MessageError::Invalid(
            "the text is empty, too long, or holds hidden characters",
        ));
    }
    run(handle.trim(), text)
}

#[cfg(target_os = "macos")]
fn run(handle: &str, text: &str) -> Result<(), MessageError> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(SCRIPT)
        .arg(handle)
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| MessageError::Failed(err.to_string()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(MessageError::Failed("no output".into()))?;
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stderr).take(16 * 1024).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MessageError::TimedOut);
            }
        }
    };
    let err = String::from_utf8_lossy(&errors.join().unwrap_or_default()).into_owned();
    if status.success() {
        return Ok(());
    }
    if err.contains("-1743") || err.to_lowercase().contains("not authorized") {
        return Err(MessageError::NotAllowed);
    }
    let code = err
        .split(|c: char| !(c.is_ascii_digit() || c == '-'))
        .find(|part| {
            part.len() > 1 && part.starts_with('-') && part[1..].chars().all(|c| c.is_ascii_digit())
        })
        .unwrap_or("");
    Err(MessageError::Failed(
        format!("error {code}").trim_end().to_owned(),
    ))
}

#[cfg(not(target_os = "macos"))]
fn run(_handle: &str, _text: &str) -> Result<(), MessageError> {
    Err(MessageError::Failed("macOS only".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_plain_handle_and_a_plain_text_go_out() {
        for good in [
            "+49 171 1234567",
            "0171-1234567",
            "(030) 123456",
            "jonas@example.com",
        ] {
            assert!(plain_handle(good), "{good}");
        }
        for bad in [
            "",
            "12",
            "+49 171 1234567, +49 172",
            "jonas",
            "1+2345678",
            "-12345678",
            "x@y.de\u{202e}",
        ] {
            assert!(!plain_handle(bad), "{bad}");
        }
        assert!(send("+49 171 1234567", "").is_err());
        assert!(send("+49 171 1234567", "Hallo\u{200b}").is_err());
        assert!(send("jonas", "Hallo").is_err());
    }
}
