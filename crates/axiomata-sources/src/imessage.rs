//! iMessages (§8l S4c, §8m S5b). Sending to one number or address, or into an existing group chat by its id (Lirvel
//! #160), through Messages' own scripting — a constant script, the values only as its arguments. A message to one
//! person goes as the owner's Messages would send it (owner, 2026-11-12: "ob SMS oder iMessage ist mir egal"): over
//! iMessage, or as a text message over the iPhone when the number has only ever had text messages or iMessage cannot
//! deliver it — and what became of it is read back from Messages' store, never assumed. Reading from Messages' own
//! store, `~/Library/Messages/chat.db`, opened read only where it is (macOS lets an app read it only with Full Disk
//! Access, which the owner grants by hand): the text of each message — decoded from Messages' archived form when it
//! has no plain one —, who and when, and only the kind of an attachment, never the attachment. Reactions and the
//! group's own notices are left out, and so is whoever the caller closed.

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
    /// Messages' store cannot be read: the app has no Full Disk Access.
    NoDiskAccess,
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
            MessageError::NoDiskAccess => write!(
                f,
                "the messages cannot be read: allow Full Disk Access in System Settings → Privacy & Security"
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

/// The script for a text message (SMS) over the iPhone: the handle and the text are its arguments.
#[cfg(target_os = "macos")]
const SMS_SCRIPT: &str = r#"
on run argv
  set theHandle to item 1 of argv
  set theText to item 2 of argv
  tell application "Messages"
    set theService to 1st account whose service type = SMS
    set theBuddy to participant theHandle of theService
    send theText to theBuddy
  end tell
  return "sent"
end run
"#;

/// The script for an RCS message over the iPhone: the handle and the text are its arguments. A number whose chat runs
/// over RCS gets it there: sent as SMS instead, it arrives, but Messages marks it red, "Nicht zugestellt", because no
/// report comes back (owner, 2026-11-12) — and that red misleads whoever reads the chat afterwards.
#[cfg(target_os = "macos")]
const RCS_SCRIPT: &str = r#"
on run argv
  set theHandle to item 1 of argv
  set theText to item 2 of argv
  tell application "Messages"
    set theService to 1st account whose service type = RCS
    set theBuddy to participant theHandle of theService
    send theText to theBuddy
  end tell
  return "sent"
end run
"#;

/// The script for a group chat: its id and the text are the arguments; an existing chat only, never a new one.
#[cfg(target_os = "macos")]
const GROUP_SCRIPT: &str = r#"
on run argv
  set theChat to item 1 of argv
  set theText to item 2 of argv
  tell application "Messages"
    send theText to chat id theChat
  end tell
  return "sent"
end run
"#;

/// The longest id of a chat taken.
const MAX_GUID_CHARS: usize = 80;

/// Whether `guid` is the id of an iMessage chat as Messages keeps it (`iMessage;+;chat123…`, newer Macs `any;+;…`):
/// never an SMS one, and nothing `osascript` could read as an option.
pub fn plain_chat_guid(guid: &str) -> bool {
    let mut parts = guid.split(';');
    let (Some(service), Some(kind), Some(id), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    matches!(service, "iMessage" | "any")
        && kind == "+"
        && guid.len() <= MAX_GUID_CHARS
        && id.starts_with("chat")
        && id.len() > 4
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A group chat as Messages keeps it: what a message into it goes by, and who reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// The chat's row, as [`Received::chat`] names it.
    pub chat: i64,
    /// Messages' id of the chat, [`plain_chat_guid`].
    pub guid: String,
    /// Its name; empty when it has none.
    pub name: String,
    /// The others in it, by number or address.
    pub members: Vec<String>,
}

/// The most members of a group told.
const MAX_MEMBERS: usize = 100;

/// The group chat of row `chat`; `None` when there is none, it is no group, or not an iMessage one.
///
/// # Errors
///
/// [`MessageError::NoDiskAccess`] without Full Disk Access, [`MessageError::Failed`] when the store fails.
pub fn group(chat: i64) -> Result<Option<Group>, MessageError> {
    group_from(&open()?, chat)
}

/// [`group`] on the store `db`.
fn group_from(db: &rusqlite::Connection, chat: i64) -> Result<Option<Group>, MessageError> {
    let row = db.query_row(
        "SELECT COALESCE(guid, ''), style, COALESCE(display_name, ''), COALESCE(service_name, '')
         FROM chat WHERE ROWID = ?1",
        [chat],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    );
    let (guid, style, name, service) = match row {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(err) => return Err(store_error(&err)),
    };
    // The id's prefix is no proof: newer Macs write "any;" for SMS and RCS groups too — the chat's service decides.
    if style != GROUP_STYLE || !plain_chat_guid(&guid) || service != "iMessage" {
        return Ok(None);
    }
    let mut statement = db
        .prepare(
            "SELECT COALESCE(h.id, '') FROM chat_handle_join j JOIN handle h ON h.ROWID = j.handle_id
             WHERE j.chat_id = ?1 ORDER BY h.ROWID LIMIT ?2",
        )
        .map_err(|err| store_error(&err))?;
    let members = statement
        .query_map(rusqlite::params![chat, MAX_MEMBERS as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|err| store_error(&err))?
        .filter_map(Result::ok)
        .map(|handle| {
            clean(&handle)
                .chars()
                .take(MAX_HANDLE_CHARS)
                .collect::<String>()
        })
        .filter(|handle| !handle.is_empty())
        .collect();
    Ok(Some(Group {
        chat,
        guid,
        name: clean(&name),
        members,
    }))
}

/// Sends `text` into the existing group chat `guid` over iMessage.
///
/// # Errors
///
/// [`MessageError::Invalid`] for a chat id or text that may not go out, else see [`MessageError`].
pub fn send_to_group(guid: &str, text: &str) -> Result<(), MessageError> {
    if !plain_chat_guid(guid) {
        return Err(MessageError::Invalid("no iMessage group chat"));
    }
    check_text(text)?;
    run_script(GROUP_SCRIPT, guid, text)
}

/// Whether `text` may go out: not empty, not too long, nothing hidden in it.
fn check_text(text: &str) -> Result<(), MessageError> {
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
    Ok(())
}

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

/// How a message to one person went out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// It went as a text message over the iPhone (SMS or RCS), not over iMessage.
    pub sms: bool,
    /// Messages' store says it was sent; `false` when the store could not be read (no Full Disk Access) or Messages
    /// had not answered yet — then it was handed over, nothing more is known.
    pub confirmed: bool,
}

/// How long Messages is watched for what became of a message.
const WATCH: std::time::Duration = std::time::Duration::from_secs(12);
/// How often the store is looked at meanwhile.
const WATCH_STEP: std::time::Duration = std::time::Duration::from_millis(300);

/// What Messages' store says of a message sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Not in the store yet, or not sent yet.
    Waiting,
    Sent,
    /// Messages gave up on it (the red "Nicht zugestellt"), with its error number.
    Failed(i64),
}

/// Sends `text` to `handle` as the owner's Messages would: over iMessage, as a text message over the iPhone when the
/// number only ever had text messages or iMessage cannot deliver it — over RCS first when the number has had RCS,
/// as an SMS when RCS cannot take it.
///
/// # Errors
///
/// [`MessageError::Invalid`] for a handle or text that may not go out; [`MessageError::Failed`] when neither way
/// got it out; else see [`MessageError`].
pub fn send(handle: &str, text: &str) -> Result<Delivery, MessageError> {
    if !plain_handle(handle) {
        return Err(MessageError::Invalid("no plain number or address"));
    }
    check_text(text)?;
    let handle = handle.trim();
    // A number that only ever had text messages gets one: iMessage would fail on it, red, after a while.
    let known = services(handle).ok();
    let only_text = !handle.contains('@')
        && known.is_some_and(|(imessage, sms, rcs)| (sms || rcs) && !imessage);
    let rcs_first = known.is_some_and(|(_, _, rcs)| rcs);
    if !only_text {
        let before = last_row();
        match run_script_of(Way::IMessage, handle, text) {
            Ok(()) => match watch(handle, text, before) {
                Fate::Sent => {
                    return Ok(Delivery {
                        sms: false,
                        confirmed: true,
                    });
                }
                Fate::Waiting => {
                    return Ok(Delivery {
                        sms: false,
                        confirmed: false,
                    });
                }
                Fate::Failed(_) => {}
            },
            // Stopped waiting is not stopped sending: a text message now could make it two (reviews 2026-11-12).
            Err(MessageError::TimedOut) => return Err(MessageError::TimedOut),
            // Not allowed is not allowed for a text message either.
            Err(MessageError::NotAllowed) => return Err(MessageError::NotAllowed),
            Err(_) => {}
        }
        // Messages may have sent it as a text message by itself meanwhile: then not a second time.
        if let Ok(Fate::Sent) = fate(handle, text, before, true) {
            return Ok(Delivery {
                sms: true,
                confirmed: true,
            });
        }
    }
    // An address has no text message.
    if handle.contains('@') {
        return Err(MessageError::Failed(
            "iMessage could not deliver it, and an address takes no text message".into(),
        ));
    }
    if rcs_first {
        let before = last_row();
        let ran = run_script_of(Way::Rcs, handle, text);
        match ran {
            Err(MessageError::TimedOut) => return Err(MessageError::TimedOut),
            Err(MessageError::NotAllowed) => return Err(MessageError::NotAllowed),
            _ => {}
        }
        // Even when the script failed, Messages may have taken it: watched before anything goes again.
        match watch(handle, text, before) {
            Fate::Sent => {
                return Ok(Delivery {
                    sms: true,
                    confirmed: true,
                });
            }
            // Red over RCS is no proof it did not arrive (the very case above): no second copy as SMS.
            Fate::Failed(code) => {
                return Err(MessageError::Failed(format!(
                    "RCS reports it not delivered (error {code}); not sent again as SMS, so no copy goes twice"
                )));
            }
            Fate::Waiting if ran.is_ok() => {
                return Ok(Delivery {
                    sms: true,
                    confirmed: false,
                });
            }
            // No RCS account on this Mac, and nothing of it in the store: as an SMS.
            Fate::Waiting => {}
        }
    }
    let before = last_row();
    run_script_of(Way::Sms, handle, text)?;
    match watch(handle, text, before) {
        Fate::Sent => Ok(Delivery {
            sms: true,
            confirmed: true,
        }),
        Fate::Waiting => Ok(Delivery {
            sms: true,
            confirmed: false,
        }),
        Fate::Failed(code) => Err(MessageError::Failed(format!(
            "not delivered, neither over iMessage nor as a text message (error {code}) — is the iPhone near and \
             text message forwarding on?"
        ))),
    }
}

/// The ways a message to one person goes.
#[derive(Clone, Copy)]
enum Way {
    IMessage,
    Sms,
    Rcs,
}

#[cfg(target_os = "macos")]
fn run_script_of(way: Way, handle: &str, text: &str) -> Result<(), MessageError> {
    match way {
        Way::IMessage => run_script(SCRIPT, handle, text),
        Way::Sms => run_script(SMS_SCRIPT, handle, text),
        Way::Rcs => run_script(RCS_SCRIPT, handle, text),
    }
}

#[cfg(not(target_os = "macos"))]
fn run_script_of(_way: Way, _handle: &str, _text: &str) -> Result<(), MessageError> {
    Err(MessageError::Failed("macOS only".into()))
}

/// The newest row of Messages' store before a send: only a message after it can be the one sent (reviews
/// 2026-11-12: the same text sent twice must not be confirmed by the first). `i64::MAX` when the store cannot be read —
/// then nothing is confirmed.
fn last_row() -> i64 {
    open()
        .and_then(|db| {
            db.query_row("SELECT COALESCE(MAX(ROWID), 0) FROM message", [], |row| {
                row.get(0)
            })
            .map_err(|err| store_error(&err))
        })
        .unwrap_or(i64::MAX)
}

/// Whether `handle` has had iMessage, SMS and RCS messages in Messages' store.
fn services(handle: &str) -> Result<(bool, bool, bool), MessageError> {
    let db = open()?;
    let mut statement = db
        .prepare("SELECT id, service FROM handle")
        .map_err(|err| store_error(&err))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| store_error(&err))?;
    let (mut imessage, mut sms, mut rcs) = (false, false, false);
    for (id, service) in rows.flatten() {
        if same_handle(&id, handle) {
            imessage |= service == "iMessage";
            sms |= service == "SMS";
            rcs |= service == "RCS";
        }
    }
    Ok((imessage, sms, rcs))
}

/// Watches Messages' store for what became of `text`, sent to `handle` in a row after `before`, for at most
/// [`WATCH`].
fn watch(handle: &str, text: &str, before: i64) -> Fate {
    let until = std::time::Instant::now() + WATCH;
    loop {
        match fate(handle, text, before, false) {
            Ok(Fate::Waiting) => {}
            Ok(fate) => return fate,
            // Without Full Disk Access nothing can be read back: it was handed over, nothing more is known.
            Err(_) => return Fate::Waiting,
        }
        if std::time::Instant::now() >= until {
            return Fate::Waiting;
        }
        std::thread::sleep(WATCH_STEP);
    }
}

/// The newest message of the owner's to `handle` after row `before` holding `text`: sent, failed, or not yet. With
/// `any_sent`, whether any such message was sent, however the newest fared.
fn fate(handle: &str, text: &str, before: i64, any_sent: bool) -> Result<Fate, MessageError> {
    let db = open()?;
    let mut statement = db
        .prepare(
            "SELECT COALESCE(h.id, ''), m.text, m.attributedBody, m.is_sent, m.error
             FROM message m LEFT JOIN handle h ON h.ROWID = m.handle_id
             WHERE m.is_from_me = 1 AND m.ROWID > ?1
             ORDER BY m.ROWID DESC LIMIT 20",
        )
        .map_err(|err| store_error(&err))?;
    let rows = statement
        .query_map([before], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|err| store_error(&err))?;
    let mut newest = None;
    for (id, plain, archived, sent, error) in rows.flatten() {
        let body = plain.or_else(|| archived.as_deref().and_then(body_text));
        if !same_handle(&id, handle) || body.as_deref().map(str::trim) != Some(text.trim()) {
            continue;
        }
        let fate = classify(sent, error);
        if !any_sent {
            return Ok(fate);
        }
        if fate == Fate::Sent {
            return Ok(Fate::Sent);
        }
        newest.get_or_insert(fate);
    }
    Ok(newest.unwrap_or(Fate::Waiting))
}

/// What a message's `is_sent` and `error` say.
fn classify(sent: i64, error: i64) -> Fate {
    if error != 0 {
        Fate::Failed(error)
    } else if sent != 0 {
        Fate::Sent
    } else {
        Fate::Waiting
    }
}

/// Runs `script` with its two arguments, waiting at most [`TIMEOUT`].
#[cfg(target_os = "macos")]
fn run_script(script: &str, first: &str, text: &str) -> Result<(), MessageError> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .arg(first)
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
fn run_script(_script: &str, _first: &str, _text: &str) -> Result<(), MessageError> {
    Err(MessageError::Failed("macOS only".into()))
}

/// The most messages one read returns.
pub const MAX_READ: usize = 50;
/// The most messages of the time asked for one read looks through.
const MAX_SCANNED: i64 = 5000;
/// Seconds from 1970 to 2001, where Messages' clock starts.
const APPLE_EPOCH: i64 = 978_307_200;
/// The longest text kept of a message.
const MAX_READ_CHARS: usize = 2000;
/// How Messages marks a group chat (`chat.style`).
const GROUP_STYLE: i64 = 43;
/// The longest number or address kept.
const MAX_HANDLE_CHARS: usize = 64;
/// The digits a number needs to be compared by its national part only.
const MIN_TAIL_DIGITS: usize = 9;

/// A message as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub id: i64,
    /// The chat it is in.
    pub chat: i64,
    /// The group's name, for a message in a group chat (empty when the group has none).
    pub group: Option<String>,
    /// In a chat of two the other one's number or address (also for the owner's own messages); in a group the
    /// sender's (empty for the owner's own).
    pub handle: String,
    pub from_me: bool,
    /// Seconds since 1970.
    pub at: i64,
    /// Its text, on as many lines as it has; empty when it is only an attachment.
    pub text: String,
    /// The kind of its attachment, when it has one: "photo", "video", "voice", "file".
    pub attachment: Option<&'static str>,
    /// It came over iMessage, whose sender is the account's: an SMS forwarded to the Mac may carry any number.
    pub imessage: bool,
}

/// What a read found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The messages, the newest first.
    pub messages: Vec<Received>,
    /// Only the newest messages of the time asked were looked through: older ones may fit too.
    pub capped: bool,
}

/// What to read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Only these numbers and addresses (one person's); empty for every one.
    pub handles: Vec<String>,
    /// Only what came since then, seconds since 1970.
    pub since: i64,
    /// Only messages whose text holds this, ignoring case.
    pub about: Option<String>,
    /// At most this many, the newest; at most [`MAX_READ`].
    pub limit: usize,
}

/// Where Messages keeps its messages.
pub fn store_path() -> Option<std::path::PathBuf> {
    std::env::home_dir().map(|home| home.join("Library/Messages/chat.db"))
}

/// Whether this app may read Messages' store: it has Full Disk Access.
pub fn readable() -> bool {
    open().and_then(|db| query_one(&db)).is_ok()
}

fn query_one(db: &rusqlite::Connection) -> Result<(), MessageError> {
    db.query_row("SELECT 1 FROM message LIMIT 1", [], |_| Ok(()))
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(()),
            err => Err(store_error(&err)),
        })
}

/// Messages' store, read only where it is: its journal (WAL) is read too, nothing is written.
fn open() -> Result<rusqlite::Connection, MessageError> {
    use rusqlite::OpenFlags;
    let path = store_path().ok_or(MessageError::NoDiskAccess)?;
    // Whether the app may look at all: without Full Disk Access even the file's size is refused.
    if let Err(err) = std::fs::File::open(&path) {
        return Err(match err.kind() {
            std::io::ErrorKind::PermissionDenied => MessageError::NoDiskAccess,
            _ => MessageError::Failed("Messages keeps no messages on this Mac".into()),
        });
    }
    let db = rusqlite::Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| store_error(&err))?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|err| store_error(&err))?;
    // Read only twice over: no statement of this connection may write.
    db.pragma_update(None, "query_only", true)
        .map_err(|err| store_error(&err))?;
    Ok(db)
}

/// A failure of the store as the caller is told it: no access, or only that it failed — the file itself could be
/// opened before, so a failure here is no missing Full Disk Access unless the system says so.
fn store_error(err: &rusqlite::Error) -> MessageError {
    let text = err.to_string().to_lowercase();
    if text.contains("authoriz") || text.contains("not permitted") {
        MessageError::NoDiskAccess
    } else {
        MessageError::Failed("Messages' store could not be read".into())
    }
}

/// Reads what `query` asks for, the newest first; never a message of a `closed` number or address.
///
/// # Errors
///
/// [`MessageError::NoDiskAccess`] without Full Disk Access, [`MessageError::Failed`] when the store fails.
pub fn read(query: &Query, closed: &[String]) -> Result<Found, MessageError> {
    read_from(&open()?, query, closed)
}

/// [`read`] on the store `db`.
fn read_from(
    db: &rusqlite::Connection,
    query: &Query,
    closed: &[String],
) -> Result<Found, MessageError> {
    let since = (query.since - APPLE_EPOCH)
        .max(0)
        .saturating_mul(1_000_000_000);
    let mut rows = db
        .prepare(
            "SELECT m.ROWID, m.text, m.attributedBody, m.date, m.is_from_me, COALESCE(h.id, ''), c.ROWID, c.style,
                    COALESCE(c.display_name, ''), COALESCE(c.chat_identifier, ''),
                    (SELECT a.mime_type FROM message_attachment_join j JOIN attachment a ON a.ROWID = j.attachment_id
                     WHERE j.message_id = m.ROWID LIMIT 1),
                    m.cache_has_attachments, COALESCE(m.service, '')
             FROM message m
             JOIN chat_message_join cm ON cm.message_id = m.ROWID
             JOIN chat c ON c.ROWID = cm.chat_id
             LEFT JOIN handle h ON h.ROWID = m.handle_id
             WHERE m.date >= ?1 AND m.associated_message_type = 0 AND m.item_type = 0
             ORDER BY m.date DESC LIMIT ?2",
        )
        .map_err(|err| store_error(&err))?;
    let limit = query.limit.clamp(1, MAX_READ);
    let about = query.about.as_ref().map(|about| about.to_lowercase());
    let mut found = Vec::new();
    let mut seen: i64 = 0;
    let mut scanned = rows
        .query(rusqlite::params![since, MAX_SCANNED])
        .map_err(|err| store_error(&err))?;
    while let Some(row) = scanned.next().map_err(|err| store_error(&err))? {
        seen += 1;
        let get = |n: usize| -> Option<String> { row.get::<_, Option<String>>(n).ok().flatten() };
        let style: i64 = row.get(7).unwrap_or(0);
        let group = style == GROUP_STYLE;
        let sender = get(5).unwrap_or_default();
        let handle = if group || !sender.is_empty() {
            sender
        } else {
            get(9).unwrap_or_default()
        };
        let handle: String = clean(&handle).chars().take(MAX_HANDLE_CHARS).collect();
        if !handle.is_empty() && closed.iter().any(|closed| same_handle(closed, &handle)) {
            continue;
        }
        if !query.handles.is_empty()
            && !query
                .handles
                .iter()
                .any(|wanted| same_handle(wanted, &handle))
        {
            continue;
        }
        let text = get(1)
            .filter(|text| !text.trim().is_empty())
            .or_else(|| {
                row.get::<_, Option<Vec<u8>>>(2)
                    .ok()
                    .flatten()
                    .and_then(|blob| body_text(&blob))
            })
            .map(|text| clean(&text))
            .unwrap_or_default();
        let has_attachment = row.get::<_, Option<i64>>(11).ok().flatten().unwrap_or(0) != 0;
        let attachment = has_attachment.then(|| kind_of(get(10).as_deref().unwrap_or("")));
        if text.is_empty() && attachment.is_none() {
            continue;
        }
        if let Some(about) = &about
            && !text.to_lowercase().contains(about.as_str())
        {
            continue;
        }
        let date: i64 = row.get(3).unwrap_or(0);
        // Older stores count seconds, newer ones nanoseconds.
        let seconds = if date > 1_000_000_000_000 {
            date / 1_000_000_000
        } else {
            date
        };
        found.push(Received {
            id: row.get(0).unwrap_or(0),
            chat: row.get(6).unwrap_or(0),
            group: group.then(|| clean(&get(8).unwrap_or_default())),
            handle,
            from_me: row.get::<_, i64>(4).unwrap_or(0) != 0,
            at: seconds + APPLE_EPOCH,
            text,
            attachment,
            imessage: get(12).is_some_and(|service| service.eq_ignore_ascii_case("iMessage")),
        });
        if found.len() >= limit {
            break;
        }
    }
    Ok(Found {
        capped: found.len() < limit && seen >= MAX_SCANNED,
        messages: found,
    })
}

/// Whether two numbers or addresses are the same one's: an address ignoring case; two numbers with their country
/// code all their digits; a number written without one ("0171 1234567") by its last nine digits against the other
/// ("+491711234567") — never a short one.
pub fn same_handle(a: &str, b: &str) -> bool {
    if a.contains('@') || b.contains('@') {
        return a.trim().eq_ignore_ascii_case(b.trim());
    }
    // Digits, and whether they begin with the country ("+49", "0049").
    let number = |text: &str| -> (String, bool) {
        let text = text.trim();
        let digits: String = text.chars().filter(char::is_ascii_digit).collect();
        if text.starts_with('+') {
            (digits, true)
        } else if let Some(rest) = digits.strip_prefix("00") {
            (rest.to_owned(), true)
        } else {
            (digits, false)
        }
    };
    let ((a, a_country), (b, b_country)) = (number(a), number(b));
    if a.len() < 7 || b.len() < 7 {
        return false;
    }
    if a_country && b_country {
        return a == b;
    }
    if a.len() < MIN_TAIL_DIGITS || b.len() < MIN_TAIL_DIGITS {
        return a == b;
    }
    a[a.len() - MIN_TAIL_DIGITS..] == b[b.len() - MIN_TAIL_DIGITS..]
}

/// What kind of attachment a MIME type is.
fn kind_of(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "photo"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") {
        "voice"
    } else {
        "file"
    }
}

/// A message's text without what shows nothing and without the placeholder of an inline attachment, cut.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|&c| !invisible(c) && c != '\u{fffc}' && (!c.is_control() || c == '\n'))
        .take(MAX_READ_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The text of a message in Messages' archived form (`attributedBody`, an `NSAttributedString` in Apple's
/// typedstream): the string that follows the class name `NSString` — after the type mark `+` its length (one byte,
/// or `0x81` and two bytes, or `0x82` and four, little endian), then its UTF-8. `None` when it holds none.
pub fn body_text(blob: &[u8]) -> Option<String> {
    const CLASS: &[u8] = b"NSString";
    let at = blob
        .windows(CLASS.len())
        .position(|window| window == CLASS)?
        + CLASS.len();
    // The `+` that opens the string comes a few bytes after the class name.
    let plus = blob
        .get(at..(at + 16).min(blob.len()))?
        .iter()
        .position(|&b| b == b'+')?
        + at
        + 1;
    let (length, start) = match *blob.get(plus)? {
        0x81 => (
            usize::from(u16::from_le_bytes([
                *blob.get(plus + 1)?,
                *blob.get(plus + 2)?,
            ])),
            plus + 3,
        ),
        0x82 => {
            let bytes: [u8; 4] = blob.get(plus + 1..plus + 5)?.try_into().ok()?;
            (u32::from_le_bytes(bytes) as usize, plus + 5)
        }
        // Any other mark above 0x7f is no length.
        0x80..=0xff => return None,
        short => (usize::from(short), plus + 1),
    };
    let text = blob.get(start..start.checked_add(length)?)?;
    String::from_utf8(text.to_vec())
        .ok()
        .filter(|text| !text.is_empty())
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

    /// An attributed body as Messages archives it: the class name, a few type bytes, `+`, the length, the text.
    fn archived(text: &str) -> Vec<u8> {
        let mut blob =
            b"\x04\x0bstreamtyped\x81\xe8\x03\x84\x01@\x84\x84\x84\x12NSAttributedString\x00"
                .to_vec();
        blob.extend_from_slice(
            b"\x84\x84\x08NSObject\x00\x85\x92\x84\x84\x84\x08NSString\x01\x94\x84\x01+",
        );
        let bytes = text.as_bytes();
        match bytes.len() {
            0..=0x7f => blob.push(bytes.len() as u8),
            len if len <= 0xffff => {
                blob.push(0x81);
                blob.extend_from_slice(&(len as u16).to_le_bytes());
            }
            len => {
                blob.push(0x82);
                blob.extend_from_slice(&(len as u32).to_le_bytes());
            }
        }
        blob.extend_from_slice(bytes);
        blob.extend_from_slice(b"\x86\x84\x02iI\x01");
        blob
    }

    #[test]
    fn the_text_of_an_archived_body_is_read_whatever_its_length() {
        assert_eq!(
            body_text(&archived("Hallo 👋")).as_deref(),
            Some("Hallo 👋")
        );
        let long = "x".repeat(300);
        assert_eq!(body_text(&archived(&long)), Some(long));
        let longer = "y".repeat(70_000);
        assert_eq!(body_text(&archived(&longer)), Some(longer));
        assert_eq!(body_text(b"no string here"), None);
        // A mark that is no length, and a class name at the very end.
        let mut odd = archived("x");
        let at = odd.iter().position(|&b| b == b'+').unwrap() + 1;
        odd[at] = 0x85;
        assert_eq!(body_text(&odd), None);
        assert_eq!(body_text(b"NSString\x01+\x02hi"), Some("hi".into()));
        // A length past the end is no text.
        let mut cut = archived("Hallo");
        cut.truncate(cut.len() - 9);
        assert_eq!(body_text(&cut), None);
        assert_eq!(clean("Foto \u{fffc} da\u{200b}"), "Foto  da");
    }

    #[test]
    fn a_number_is_the_same_one_however_it_is_written() {
        assert!(same_handle("0171 1234567", "+491711234567"));
        assert!(same_handle("0049 171 1234567", "+491711234567"));
        assert!(same_handle("Jonas@Example.com", "jonas@example.com"));
        assert!(!same_handle("+491711234567", "+491711234568"));
        // Another country is another one, and a short number is only itself.
        assert!(!same_handle("+49 171 1234567", "+43 171 1234567"));
        assert!(!same_handle("1234567", "+491711234567"));
        assert!(same_handle("1234567", "123 4567"));
        assert!(!same_handle("123", "0123"));
        assert!(!same_handle("jonas@example.com", "+491711234567"));
    }

    #[test]
    fn only_an_imessage_group_chats_id_goes_to_messages() {
        assert!(plain_chat_guid("iMessage;+;chat123456789"));
        assert!(plain_chat_guid("any;+;chat0A1b-2"));
        // An SMS group, a chat of two, an option, a script.
        assert!(!plain_chat_guid("SMS;+;chat123"));
        assert!(!plain_chat_guid("iMessage;-;+491711234567"));
        assert!(!plain_chat_guid("-e;+;chat1"));
        assert!(!plain_chat_guid("iMessage;+;chat1\" & quit"));
        assert!(!plain_chat_guid("iMessage;+;chat"));
        assert!(!plain_chat_guid("iMessage;+;chat1;x"));
        assert_eq!(
            send_to_group("SMS;+;chat1", "Hallo"),
            Err(MessageError::Invalid("no iMessage group chat"))
        );
        assert!(matches!(
            send_to_group("iMessage;+;chat1", "  "),
            Err(MessageError::Invalid(_))
        ));
    }

    #[test]
    fn a_group_is_read_with_its_id_name_and_members_never_an_sms_one() {
        let db = store();
        let family = group_from(&db, 2).unwrap().unwrap();
        assert_eq!(family.guid, "iMessage;+;chat123");
        assert_eq!(family.name, "Familie");
        assert_eq!(family.members, ["+491711234567", "+491607654321"]);
        // A chat of two is no group; an SMS group is none she writes into; a gone one is none.
        assert_eq!(group_from(&db, 1).unwrap(), None);
        assert_eq!(group_from(&db, 3).unwrap(), None);
        // An "any;" id of an SMS group is none either: the chat's service decides.
        assert_eq!(group_from(&db, 4).unwrap(), None);
        assert_eq!(group_from(&db, 99).unwrap(), None);
    }

    /// A store with Messages' tables as far as `read` uses them.
    fn store() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT);
             CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, style INTEGER, display_name TEXT, chat_identifier TEXT,
                 guid TEXT, service_name TEXT);
             CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
             CREATE TABLE message (ROWID INTEGER PRIMARY KEY, text TEXT, attributedBody BLOB, date INTEGER,
                 is_from_me INTEGER, handle_id INTEGER, associated_message_type INTEGER, item_type INTEGER,
                 cache_has_attachments INTEGER, service TEXT);
             CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
             CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY, mime_type TEXT);
             CREATE TABLE message_attachment_join (message_id INTEGER, attachment_id INTEGER);
             INSERT INTO handle VALUES (1, '+491711234567'), (2, '+491607654321');
             INSERT INTO chat VALUES (1, 45, NULL, '+491711234567', 'iMessage;-;+491711234567', 'iMessage'),
                 (2, 43, 'Familie', 'chat123', 'iMessage;+;chat123', 'iMessage'),
                 (3, 43, 'Verein', 'chat9', 'SMS;+;chat9', 'SMS'),
                 (4, 43, 'Nachbarn', 'chat4', 'any;+;chat4', 'SMS');
             INSERT INTO chat_handle_join VALUES (2, 1), (2, 2), (3, 1), (4, 2);
             INSERT INTO attachment VALUES (1, 'image/jpeg');",
        )
        .unwrap();
        let day = 815_000_000_i64 * 1_000_000_000;
        let add = |id: i64,
                   chat: i64,
                   handle: i64,
                   from_me: bool,
                   text: Option<&str>,
                   blob: Option<Vec<u8>>| {
            db.execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, 'iMessage')",
                rusqlite::params![id, text, blob, day + id * 60_000_000_000, from_me, handle],
            )
            .unwrap();
            db.execute("INSERT INTO chat_message_join VALUES (?1, ?2)", [chat, id])
                .unwrap();
        };
        add(1, 1, 1, false, Some("Kommst du morgen?"), None);
        add(2, 1, 0, true, None, Some(archived("Ja, gern!")));
        add(3, 2, 2, false, Some("Frühstück um 9?"), None);
        add(4, 2, 0, true, Some("Bin dabei"), None);
        add(5, 1, 1, false, None, None);
        db.execute_batch(
            "UPDATE message SET cache_has_attachments = 1 WHERE ROWID = 5;
             INSERT INTO message_attachment_join VALUES (5, 1);
             INSERT INTO message VALUES (6, 'Gefällt mir', NULL, 0, 0, 1, 2000, 0, 0, 'iMessage');
             INSERT INTO chat_message_join VALUES (1, 6);
             UPDATE message SET service = 'SMS' WHERE ROWID = 3;",
        )
        .unwrap();
        db
    }

    #[test]
    fn the_store_is_read_newest_first_without_reactions_and_closed_ones() {
        let db = store();
        let all = Query {
            limit: MAX_READ,
            ..Query::default()
        };
        let found = read_from(&db, &all, &[]).unwrap();
        let ids: Vec<i64> = found.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, [5, 4, 3, 2, 1], "reactions are left out");
        let by_id = |id: i64| found.messages.iter().find(|m| m.id == id).unwrap().clone();
        // The owner's own in a chat of two names the other one; in a group, nobody.
        assert_eq!(by_id(2).handle, "+491711234567");
        assert_eq!(by_id(2).text, "Ja, gern!");
        assert!(by_id(2).from_me);
        assert_eq!(by_id(4).handle, "");
        assert_eq!(by_id(3).group.as_deref(), Some("Familie"));
        assert!(!by_id(3).imessage, "an SMS is no iMessage");
        assert_eq!(by_id(5).attachment, Some("photo"));
        assert_eq!(by_id(1).at, 815_000_060 + APPLE_EPOCH);
        // One person's, a word, and never a closed one.
        let jonas = Query {
            handles: vec!["0171 1234567".into()],
            ..all.clone()
        };
        assert_eq!(read_from(&db, &jonas, &[]).unwrap().messages.len(), 3);
        let word = Query {
            about: Some("FRÜHSTÜCK".into()),
            ..all.clone()
        };
        assert_eq!(read_from(&db, &word, &[]).unwrap().messages[0].id, 3);
        let closed = read_from(&db, &all, &["+49 171 1234567".into()]).unwrap();
        assert!(closed.messages.iter().all(|m| m.chat == 2));
        assert!(!closed.capped);
    }

    #[test]
    #[ignore = "reads this Mac's messages; run by hand with Full Disk Access — prints only counts"]
    fn on_this_mac() {
        let query = Query {
            since: 0,
            limit: MAX_READ,
            ..Query::default()
        };
        let found = read(&query, &[]).unwrap().messages;
        let decoded = found.iter().filter(|m| !m.text.is_empty()).count();
        let over_imessage = found.iter().filter(|m| m.imessage).count();
        eprintln!(
            "{} messages read, {decoded} with text, {over_imessage} over iMessage",
            found.len()
        );
    }

    #[test]
    fn what_messages_store_says_of_a_message_sent() {
        assert_eq!(classify(0, 0), Fate::Waiting);
        assert_eq!(classify(1, 0), Fate::Sent);
        assert_eq!(classify(0, 22), Fate::Failed(22), "the red not delivered");
        assert_eq!(classify(1, 4), Fate::Failed(4), "an error outweighs sent");
    }
}
