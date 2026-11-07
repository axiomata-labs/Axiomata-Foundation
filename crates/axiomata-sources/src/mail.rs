//! The owner's mail as Mail.app holds it (Lirvel `docs/plans/companion.md` §8j S2d, Q8/Q20): the inboxes of all
//! accounts, read through Apple events — JavaScript for Automation, measured 2026-11-07 at about 2 s for two weeks of
//! headers and 0.3 s for one message — never Mail's own files (that would need Full Disk Access, C22). The script is a
//! constant; what varies goes in as its arguments, never into its text, and comes back as JSON, so a message cannot
//! forge another line. Accounts and senders the owner closed are dropped here, before anyone sees them.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::calendar::{Day, Moment, invisible};

/// The most days one look goes back.
pub const MAX_DAYS: u32 = 60;
/// The most messages one look returns, the newest first; [`Inbox::cut`] says when there were more.
pub const MAX_MAILS: usize = 2000;
/// The most characters of one message's text.
pub const MAX_TEXT_CHARS: usize = 100_000;
/// The most characters of a sender or a subject.
const MAX_LINE_CHARS: usize = 200;
/// The most messages whose text one call reads.
pub const MAX_TEXTS: usize = 20;
/// How long Mail may take to answer a look — Mail may have to start first; asking the owner for the right waits
/// longer.
const TIMEOUT: Duration = Duration::from_secs(45);
const ASK_TIMEOUT: Duration = Duration::from_secs(120);
/// The most bytes of Mail's answer read.
const MAX_ANSWER_BYTES: u64 = 16 * 1024 * 1024;

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailError {
    /// The owner did not allow this app to use Mail (System Settings → Privacy → Automation).
    NotAllowed,
    /// Mail did not answer in time.
    TimedOut,
    /// A question that is none.
    Invalid(&'static str),
    /// Off macOS.
    Unsupported,
    /// Mail or osascript failed; the error's number, never its text.
    Failed(String),
}

impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MailError::NotAllowed => write!(f, "Mail may not be read: the Mac did not allow it"),
            MailError::TimedOut => write!(f, "Mail did not answer in time"),
            MailError::Invalid(why) => write!(f, "{why}"),
            MailError::Unsupported => write!(f, "mail is read on macOS only"),
            MailError::Failed(why) => write!(f, "Mail could not be read: {why}"),
        }
    }
}

impl std::error::Error for MailError {}

/// One message's head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mail {
    /// The account's name in Mail ("iCloud").
    pub account: String,
    /// Mail's id of the message, to read its text.
    pub id: i64,
    /// The sender's name, empty when only the address is known.
    pub from_name: String,
    /// The sender's address, lower case.
    pub from_address: String,
    /// The subject on one line.
    pub subject: String,
    /// When it came, on the owner's clock.
    pub received: Moment,
    /// Seconds since 1970, to order by.
    pub at: i64,
    /// Whether the owner has read it.
    pub read: bool,
}

/// What a look at the inboxes found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inbox {
    /// The open messages, the newest first.
    pub mails: Vec<Mail>,
    /// More came than [`MAX_MAILS`]: the oldest were left out.
    pub cut: bool,
    /// Open accounts whose inbox Mail does not show: not looked into.
    pub without_inbox: Vec<String>,
}

/// What the owner closed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Closed<'a> {
    /// Whole accounts, by their name in Mail.
    pub accounts: &'a [String],
    /// Senders: an address, or a domain as `@example.com` — which closes its subdomains too (`@mail.example.com`).
    pub senders: &'a [String],
}

impl Closed<'_> {
    /// Whether `mail` is closed by its account or its sender.
    pub fn closes(&self, mail: &Mail) -> bool {
        self.closes_account(&mail.account) || self.closes_sender(&mail.from_address)
    }

    /// Whether the account `name` is closed; letters compared without their case, as Mail's script compares them.
    pub fn closes_account(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        self.accounts
            .iter()
            .any(|account| account.trim().to_lowercase() == name)
    }

    /// Whether the sender `address` (lower case) is closed.
    pub fn closes_sender(&self, address: &str) -> bool {
        self.senders.iter().any(|sender| {
            let sender = sender.trim().to_lowercase();
            match sender.strip_prefix('@') {
                Some("") => false,
                Some(domain) => {
                    address.ends_with(&format!("@{domain}"))
                        || address.ends_with(&format!(".{domain}"))
                }
                None => !sender.is_empty() && address == sender,
            }
        })
    }
}

/// The script: what it does is chosen by its first argument, the rest are its values.
const SCRIPT: &str = r#"
// Each account's inbox, by the account's name, through Mail's own inbox: whatever the box is called ("INBOX",
// "Posteingang").
function inboxes(mail) {
  const found = {};
  for (const box of mail.inbox.mailboxes()) {
    try { found[box.account().name()] = box; } catch (e) {}
  }
  return found;
}
function run(argv) {
  const mail = Application('Mail');
  const what = argv[0];
  if (what === 'running') {
    return JSON.stringify(mail.running());
  }
  if (what === 'accounts') {
    return JSON.stringify(mail.accounts().map(a => a.name()));
  }
  if (what === 'inbox') {
    const cutoff = new Date(Date.now() - Number(argv[1]) * 86400000);
    const closed = JSON.parse(argv[2]).map(n => n.trim().toLowerCase());
    const out = [], without = [];
    const boxes = inboxes(mail);
    for (const account of mail.accounts()) {
      const name = account.name();
      if (closed.includes(name.trim().toLowerCase())) continue;
      const box = boxes[name];
      if (!box) { without.push(name); continue; }
      const msgs = box.messages.whose({ dateReceived: { _greaterThan: cutoff } });
      const ids = msgs.id(), senders = msgs.sender(), subjects = msgs.subject(), dates = msgs.dateReceived(),
        read = msgs.readStatus();
      for (let i = 0; i < ids.length; i++) {
        const d = dates[i];
        out.push({ account: name, id: ids[i], sender: senders[i] || '', subject: subjects[i] || '',
          at: Math.floor(d.getTime() / 1000), year: d.getFullYear(), month: d.getMonth() + 1, day: d.getDate(),
          hour: d.getHours(), minute: d.getMinutes(), weekday: (d.getDay() + 6) % 7 + 1, read: !!read[i] });
      }
    }
    return JSON.stringify({ mails: out, without: without });
  }
  if (what === 'texts') {
    const box = inboxes(mail)[argv[1]];
    const ids = JSON.parse(argv[2]);
    return JSON.stringify(ids.map(id => {
      if (!box) return null;
      const found = box.messages.whose({ id: id });
      if (found.length === 0) return null;
      const m = found[0];
      // At most a few hundred thousand characters cross: a huge message cannot stall the look.
      return { sender: m.sender() || '', subject: m.subject() || '', content: (m.content() || '').slice(0, 400000) };
    }));
  }
  throw new Error('unknown request');
}
"#;

/// Whether Mail is open: a look starts it, a page that only shows the accounts should not.
///
/// # Errors
///
/// See [`MailError`].
pub fn running() -> Result<bool, MailError> {
    let answer = run(&["running"], TIMEOUT)?;
    serde_json::from_str(&answer).map_err(|e| MailError::Failed(e.to_string()))
}

/// The names of Mail's accounts, as Mail spells them. The first call makes macOS ask the owner whether this app may
/// use Mail; it waits for the answer.
///
/// # Errors
///
/// [`MailError::NotAllowed`] when the owner said no.
pub fn accounts() -> Result<Vec<String>, MailError> {
    let answer = run(&["accounts"], ASK_TIMEOUT)?;
    let names: Vec<String> =
        serde_json::from_str(&answer).map_err(|e| MailError::Failed(e.to_string()))?;
    // Kept as Mail spells them: the name is how a message's account is found again. One that holds what shows nothing
    // or breaks a line is left out.
    Ok(names.into_iter().filter(|name| plain(name)).collect())
}

/// The heads of the messages that came into the open accounts' inboxes in the last `days`, the newest first, at most
/// [`MAX_MAILS`]; the closed ones are not among them.
///
/// # Errors
///
/// See [`MailError`].
pub fn inbox(days: u32, closed: &Closed<'_>) -> Result<Inbox, MailError> {
    if days == 0 || days > MAX_DAYS {
        return Err(MailError::Invalid("look back between one and sixty days"));
    }
    let accounts =
        serde_json::to_string(closed.accounts).map_err(|e| MailError::Failed(e.to_string()))?;
    let answer = run(&["inbox", &days.to_string(), &accounts], TIMEOUT)?;
    let mut inbox = parse_inbox(&answer)?;
    inbox.mails.retain(|mail| !closed.closes(mail));
    inbox.mails.sort_by_key(|mail| std::cmp::Reverse(mail.at));
    inbox.cut = inbox.mails.len() > MAX_MAILS;
    inbox.mails.truncate(MAX_MAILS);
    Ok(inbox)
}

/// One message whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The sender's name, empty when only the address is known.
    pub from_name: String,
    /// The sender's address, lower case.
    pub from_address: String,
    /// The subject on one line.
    pub subject: String,
    /// Its text, at most [`MAX_TEXT_CHARS`].
    pub text: String,
}

/// The messages `ids` in the inbox of `account`, in their order; `None` for one that is not there, or that the owner
/// closed by its account or its sender. At most [`MAX_TEXTS`].
///
/// # Errors
///
/// See [`MailError`].
pub fn messages(
    account: &str,
    ids: &[i64],
    closed: &Closed<'_>,
) -> Result<Vec<Option<Message>>, MailError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > MAX_TEXTS || account.trim().is_empty() {
        return Err(MailError::Invalid("say which messages, at most twenty"));
    }
    if closed.closes_account(account) || !plain(account) {
        return Ok(vec![None; ids.len()]);
    }
    let ids_json = serde_json::to_string(ids).map_err(|e| MailError::Failed(e.to_string()))?;
    let answer = run(&["texts", account, &ids_json], TIMEOUT)?;
    parse_messages(&answer, closed)
}

/// Mail's answer to `texts`, the closed senders taken out.
fn parse_messages(answer: &str, closed: &Closed<'_>) -> Result<Vec<Option<Message>>, MailError> {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(answer).map_err(|e| MailError::Failed(e.to_string()))?;
    Ok(rows
        .iter()
        .map(|row| {
            let content = row["content"].as_str()?;
            let (from_name, from_address) = sender(row["sender"].as_str().unwrap_or_default());
            if closed.closes_sender(&from_address) {
                return None;
            }
            Some(Message {
                from_name,
                from_address,
                subject: line(row["subject"].as_str().unwrap_or_default()),
                text: body(content),
            })
        })
        .collect())
}

/// Mail's answer to `inbox` as heads, cleaned.
fn parse_inbox(answer: &str) -> Result<Inbox, MailError> {
    let answer: serde_json::Value =
        serde_json::from_str(answer).map_err(|e| MailError::Failed(e.to_string()))?;
    let rows = answer["mails"].as_array().map_or(&[][..], Vec::as_slice);
    let number = |row: &serde_json::Value, key: &str| row[key].as_i64().unwrap_or(0);
    let mails = rows
        .iter()
        .filter_map(|row| {
            let (from_name, from_address) = sender(row["sender"].as_str().unwrap_or_default());
            let account = row["account"].as_str().filter(|name| plain(name))?;
            Some(Mail {
                account: account.to_owned(),
                id: row["id"].as_i64()?,
                from_name,
                from_address,
                subject: line(row["subject"].as_str().unwrap_or_default()),
                received: Moment {
                    day: Day {
                        year: number(row, "year") as i32,
                        month: number(row, "month") as u32,
                        day: number(row, "day") as u32,
                    },
                    hour: number(row, "hour") as u32,
                    minute: number(row, "minute") as u32,
                    weekday: number(row, "weekday") as u32,
                },
                at: number(row, "at"),
                read: row["read"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    let without_inbox = answer["without"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|name| name.as_str().map(line))
        .collect();
    Ok(Inbox {
        mails,
        cut: false,
        without_inbox,
    })
}

/// Whether an account's name is one to keep as Mail spells it: not empty, nothing that shows nothing, no line break.
fn plain(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_LINE_CHARS
        && !name.chars().any(|c| c.is_control() || invisible(c))
}

/// "Jonas Weber <jonas@example.com>" as name and address — the address from the last `<…>` wherever it stands
/// ("Name <a@b.c> (comment)"), and before anything is cut, so a long name cannot push it out of reach of the closed
/// senders. A bare address has no name.
fn sender(text: &str) -> (String, String) {
    let text: String = text
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let pair = text
        .rfind('<')
        .and_then(|open| text[open..].find('>').map(|close| (open, open + close)));
    match pair {
        Some((open, close)) => {
            let address = text[open + 1..close].trim().to_lowercase();
            let name = line(text[..open].trim().trim_matches('"'));
            (name, line(&address))
        }
        None => (String::new(), line(&text.to_lowercase())),
    }
}

/// `text` on one line, without what shows nothing, at most [`MAX_LINE_CHARS`].
fn line(text: &str) -> String {
    let line: String = text
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_LINE_CHARS)
        .collect()
}

/// A message's text without what shows nothing, its lines kept, blank runs shortened, at most [`MAX_TEXT_CHARS`].
fn body(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for raw in text.lines() {
        let cleaned: String = raw
            .chars()
            .filter(|c| !invisible(*c))
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let cleaned = cleaned.trim_end();
        if cleaned.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(cleaned);
        out.push('\n');
        if out.len() > MAX_TEXT_CHARS * 4 {
            break;
        }
    }
    out.trim().chars().take(MAX_TEXT_CHARS).collect()
}

/// Runs [`SCRIPT`] with `args`, waiting at most `timeout`; its answer.
#[cfg(target_os = "macos")]
fn run(args: &[&str], timeout: Duration) -> Result<String, MailError> {
    // The script as one argument, its values as the next ones: osascript hands them to `run(argv)` as strings.
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", SCRIPT])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| MailError::Failed(format!("osascript did not start: {err}")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(MailError::Failed("no output".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(MailError::Failed("no output".into()))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_ANSWER_BYTES).read_to_end(&mut out);
        out
    });
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MailError::TimedOut);
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&errors.join().unwrap_or_default()).into_owned();
    if !status.success() {
        // -1743: the owner did not allow Apple events to Mail; -600: Mail is not there.
        if err.contains("-1743") || err.to_lowercase().contains("not authorized") {
            return Err(MailError::NotAllowed);
        }
        // Only the error's number, never its text: it may quote a message.
        let code = err
            .split(|c: char| !(c.is_ascii_digit() || c == '-'))
            .find(|part| {
                part.len() > 1
                    && part.starts_with('-')
                    && part[1..].chars().all(|c| c.is_ascii_digit())
            })
            .unwrap_or("");
        return Err(MailError::Failed(
            format!("osascript failed {code}").trim_end().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

#[cfg(not(target_os = "macos"))]
fn run(_args: &[&str], _timeout: Duration) -> Result<String, MailError> {
    Err(MailError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_are_read_cleaned_and_a_closed_sender_or_account_is_dropped() {
        let answer = r#"{"without":["Arbeit 2"],"mails":[
          {"account":"iCloud","id":7,"sender":"\"Jonas Weber\" <Jonas@Example.com>",
           "subject":"Treffen\u200b am\nFreitag",
           "at":1794000000,"year":2026,"month":11,"day":6,"hour":9,"minute":5,"weekday":5,"read":false},
          {"account":"iCloud","id":8,"sender":"news@shop.example","subject":"Angebot","at":1794000100,
           "year":2026,"month":11,"day":6,"hour":9,"minute":7,"weekday":5,"read":true},
          {"account":"Arbeit","id":9,"sender":"chef@firma.de","subject":"x","at":1,"year":2026,"month":1,"day":1,
           "hour":0,"minute":0,"weekday":4,"read":true},
          {"id":10}
        ]}"#;
        let inbox = parse_inbox(answer).unwrap();
        assert_eq!(inbox.without_inbox, ["Arbeit 2"]);
        let mails = inbox.mails;
        assert_eq!(mails.len(), 3, "a row without an account is none");
        assert_eq!(mails[0].from_name, "Jonas Weber");
        assert_eq!(mails[0].from_address, "jonas@example.com");
        assert_eq!(mails[0].subject, "Treffen am Freitag");
        assert_eq!(mails[0].received.day.to_string(), "2026-11-06");
        assert_eq!(mails[1].from_name, "");
        let senders = vec!["@SHOP.example".to_owned()];
        let accounts = vec!["arbeit".to_owned()];
        let closed = Closed {
            accounts: &accounts,
            senders: &senders,
        };
        let open: Vec<i64> = mails
            .iter()
            .filter(|m| !closed.closes(m))
            .map(|m| m.id)
            .collect();
        assert_eq!(open, [7]);
    }

    #[test]
    fn a_closed_domain_closes_its_subdomains_and_odd_senders_are_still_judged() {
        let senders = vec!["@bank.de".to_owned(), "jonas@example.com".to_owned()];
        let closed = Closed {
            accounts: &[],
            senders: &senders,
        };
        for address in ["x@bank.de", "x@mail.bank.de"] {
            assert!(closed.closes_sender(address), "{address}");
        }
        for address in ["x@evilbank.de", "x@bank.de.example", "jonas@example.co"] {
            assert!(!closed.closes_sender(address), "{address}");
        }
        let long = format!("{} <Jonas@Example.com>", "N".repeat(250));
        let (name, address) = sender(&long);
        assert_eq!(address, "jonas@example.com");
        assert_eq!(name.chars().count(), MAX_LINE_CHARS);
        assert_eq!(
            sender("Jonas <jonas@example.com> (Büro)").1,
            "jonas@example.com"
        );
        assert_eq!(sender("x@y.de").1, "x@y.de");
        let accounts = vec!["BÜRO".to_owned()];
        let closed = Closed {
            accounts: &accounts,
            senders: &[],
        };
        assert!(closed.closes_account("Büro"));
        let answer = r#"[{"sender":"a@b.de","subject":"s"},{"sender":"a@b.de","subject":"s","content":null}]"#;
        assert_eq!(
            parse_messages(answer, &Closed::default()).unwrap(),
            vec![None, None]
        );
        assert!(messages("iCloud", &[1; MAX_TEXTS + 1], &Closed::default()).is_err());
    }

    #[test]
    fn a_text_keeps_its_lines_but_nothing_invisible() {
        assert_eq!(
            body("Hallo\u{202e}du,\r\n\n\n\nwie geht's?\u{0007}"),
            "Hallodu,\n\nwie geht's?"
        );
        assert_eq!(line(&"a".repeat(500)).chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    #[ignore = "reads this Mac's mail; run by hand"]
    fn on_this_mac() {
        let started = Instant::now();
        let mails = inbox(14, &Closed::default()).unwrap().mails;
        eprintln!("{} heads in {:?}", mails.len(), started.elapsed());
        if let Some(first) = mails.first() {
            let started = Instant::now();
            let found = messages(&first.account, &[first.id], &Closed::default()).unwrap();
            let chars = found[0].as_ref().map_or(0, |m| m.text.len());
            eprintln!("one text, {chars} chars, in {:?}", started.elapsed());
        }
    }
}
