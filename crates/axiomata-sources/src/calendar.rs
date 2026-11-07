//! The owner's calendars and reminders, read through EventKit (Lirvel `docs/plans/companion.md` §8j S2b, Q15/Q16).
//! Read only. Times come out in the Mac's own time zone as the owner reads them on a clock, and a day goes in as a
//! date — the system does the time-zone arithmetic, never the caller. A calendar or list the owner closed is never
//! asked for, and access is only asked for when the owner switches a source on ([`ask`]), never in passing.

use std::fmt;

/// The longest span of days one question of events covers (Q15).
pub const MAX_DAYS: u32 = 31;
/// How many days back finished reminders go (Q16).
pub const MAX_DONE_DAYS: u32 = 7;
/// The most events or reminders one answer holds; more is a question asked too wide.
pub const MAX_ITEMS: usize = 200;
/// The longest text kept of a title, a location or a name.
const MAX_TEXT_CHARS: usize = 200;

/// Events or reminders: EventKit asks for each apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Events,
    Reminders,
}

/// Whether the owner let this app read a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Granted,
    /// Nobody asked yet: [`ask`] shows the system's dialog.
    NotAsked,
    /// Refused, or limited (write-only) — changed only in System Settings.
    Denied,
}

/// A date on the owner's calendar.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Day {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A moment on the owner's clock.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Moment {
    pub day: Day,
    pub hour: u32,
    pub minute: u32,
    /// 1 Monday … 7 Sunday.
    pub weekday: u32,
}

/// A calendar or a list of reminders, for the owner to close one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    /// EventKit's identifier: what a closed one is kept as.
    pub id: String,
    pub title: String,
    /// The account it lives in ("iCloud", "Google", "Auf meinem Mac").
    pub account: String,
    /// It takes new entries and changes (not a subscribed or a read-only shared one).
    pub writable: bool,
}

/// One occurrence of an event (a repeating event is one per occurrence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// EventKit's identifier of the event (the same for every occurrence of a repeating one).
    pub id: String,
    /// Whether it may be moved or renamed without touching anyone else: a writable calendar, no attendees, no
    /// repetition (§8k Q5).
    pub changeable: bool,
    /// It repeats: a delete takes this occurrence or the whole series.
    pub repeats: bool,
    /// It has guests: deleting it would send them a cancellation — never done here.
    pub guests: bool,
    /// Its calendar takes changes.
    pub writable: bool,
    pub title: String,
    pub start: Moment,
    pub end: Moment,
    pub all_day: bool,
    pub location: Option<String>,
    pub calendar: String,
}

/// When a reminder is due: a day, with a time when it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Due {
    pub day: Day,
    /// 1 Monday … 7 Sunday.
    pub weekday: u32,
    pub time: Option<(u32, u32)>,
}

/// A reminder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    /// EventKit's identifier of the reminder.
    pub id: String,
    pub title: String,
    pub due: Option<Due>,
    pub list: String,
    /// When it was ticked off; `None` while open.
    pub done: Option<Moment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalendarError {
    /// The owner has not let the app read this kind (or took it back in System Settings).
    AccessDenied(Kind),
    /// A question the caller got wrong.
    Invalid(&'static str),
    /// EventKit did not answer in time.
    TimedOut,
    /// Not on macOS.
    Unsupported,
    /// The event, reminder, calendar or list is not there (any more).
    NotFound,
    /// The calendar or list takes no changes.
    ReadOnly,
    /// EventKit refused to save; its own description.
    Refused(String),
}

impl fmt::Display for CalendarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied(Kind::Events) => write!(
                f,
                "no access to the calendars — allow it in System Settings → Privacy & Security → Calendars"
            ),
            Self::AccessDenied(Kind::Reminders) => write!(
                f,
                "no access to the reminders — allow it in System Settings → Privacy & Security → Reminders"
            ),
            Self::Invalid(why) => write!(f, "{why}"),
            Self::TimedOut => write!(f, "EventKit did not answer in time"),
            Self::Unsupported => write!(f, "calendars are read on macOS only"),
            Self::NotFound => write!(f, "it is not there (any more)"),
            Self::ReadOnly => write!(f, "that calendar or list takes no changes"),
            Self::Refused(why) => write!(f, "the calendar refused it: {why}"),
        }
    }
}

impl std::error::Error for CalendarError {}

/// Whether this app may read `kind`.
pub fn status(kind: Kind) -> Status {
    imp::status(kind)
}

/// Asks the owner for access to `kind` when nobody has yet — the system's dialog; blocks until the owner answered (up
/// to two minutes), so call it off the UI thread. `Ok` once it is granted.
///
/// # Errors
///
/// [`CalendarError::AccessDenied`] when the owner says no, or said so before.
pub fn ask(kind: Kind) -> Result<(), CalendarError> {
    imp::ask(kind)
}

/// The calendars (or lists) there are, the closed ones too: for the owner's choice.
///
/// # Errors
///
/// [`CalendarError::AccessDenied`] without access.
pub fn collections(kind: Kind) -> Result<Vec<Collection>, CalendarError> {
    imp::collections(kind)
}

/// The events of `days` days from `from` on, in every calendar but the `closed` ones, earliest first.
///
/// # Errors
///
/// [`CalendarError::Invalid`] for no day or more than [`MAX_DAYS`]; [`CalendarError::AccessDenied`] without access.
pub fn events(from: Day, days: u32, closed: &[String]) -> Result<Vec<Event>, CalendarError> {
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(CalendarError::Invalid("between 1 and 31 days"));
    }
    if !valid(from) {
        return Err(CalendarError::Invalid("no such day"));
    }
    let mut events = imp::events(from, days, closed)?;
    events.sort_by_key(|event| (event.start.day, !event.all_day, event.start));
    events.truncate(MAX_ITEMS);
    Ok(events)
}

/// The open reminders in every list but the `closed` ones: due ones first, by due date, then the rest.
///
/// # Errors
///
/// [`CalendarError::AccessDenied`] without access.
pub fn open_reminders(closed: &[String]) -> Result<Vec<Reminder>, CalendarError> {
    let mut reminders = imp::reminders(None, closed)?;
    reminders.sort_by_key(|reminder| {
        (
            reminder.due.is_none(),
            reminder.due.map(|due| (due.day, due.time)),
        )
    });
    reminders.truncate(MAX_ITEMS);
    Ok(reminders)
}

/// The reminders ticked off in the last `days` days, in every list but the `closed` ones, latest first.
///
/// # Errors
///
/// [`CalendarError::Invalid`] for no day or more than [`MAX_DONE_DAYS`]; [`CalendarError::AccessDenied`] without
/// access.
pub fn done_reminders(days: u32, closed: &[String]) -> Result<Vec<Reminder>, CalendarError> {
    if !(1..=MAX_DONE_DAYS).contains(&days) {
        return Err(CalendarError::Invalid("between 1 and 7 days"));
    }
    let mut reminders = imp::reminders(Some(days), closed)?;
    reminders.sort_by_key(|reminder| std::cmp::Reverse(reminder.done));
    reminders.truncate(MAX_ITEMS);
    Ok(reminders)
}

/// Now on the owner's clock; `None` off macOS.
pub fn now() -> Option<Moment> {
    imp::now()
}

/// Whether `day` is a date at all (the calendar's own check comes on top).
fn valid(day: Day) -> bool {
    (1900..=2200).contains(&day.year)
        && (1..=12).contains(&day.month)
        && (1..=31).contains(&day.day)
}

/// `text` on one line, without control characters and what shows nothing, at most [`MAX_TEXT_CHARS`] — as every
/// title here is told; a name to compare with one of them is cleaned the same way.
pub fn clean(text: &str) -> String {
    let line: String = text
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars().take(MAX_TEXT_CHARS).collect()
}

/// Characters that show nothing but can hide words from a check or carry text a reader never sees: zero-width
/// spaces and joiners, direction marks and overrides, the soft hyphen, the byte-order mark, tag characters and the
/// private-use planes.
pub fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{E000}'..='\u{F8FF}'
            | '\u{E0000}'..='\u{E0FFF}'
            | '\u{F0000}'..='\u{10FFFF}'
    )
}

/// EventKit's weekday (1 Sunday … 7 Saturday) as 1 Monday … 7 Sunday.
fn weekday(eventkit: i64) -> u32 {
    match eventkit {
        1 => 7,
        2..=7 => (eventkit - 1) as u32,
        _ => 0,
    }
}

mod write;

pub use write::{
    Created, Frequency, MAX_INTERVAL, NewEvent, NewReminder, Repeat, RepeatEnd, Snapshot,
    complete_reminder, create_event, create_reminder, default_collection, event_snapshot,
    move_event, reminder_done, remove_event, remove_reminder,
};
pub use write::{
    DeletedEvent, DeletedReminder, Scope, delete_event, delete_reminder, restore_event,
    restore_reminder,
};

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_event_kit::{
        EKAuthorizationStatus, EKCalendar, EKEntityType, EKEventStore, EKReminder,
    };
    use objc2_foundation::{
        NSArray, NSCalendar, NSCalendarIdentifierGregorian, NSCalendarOptions, NSCalendarUnit,
        NSDate, NSDateComponentUndefined, NSDateComponents, NSError,
    };

    use super::{
        CalendarError, Collection, Day, Due, Event, Kind, Moment, Reminder, Status, clean, weekday,
    };

    /// How long the owner gets to answer the system's dialog.
    const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
    /// How long EventKit gets to fetch reminders.
    const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

    fn entity(kind: Kind) -> EKEntityType {
        match kind {
            Kind::Events => EKEntityType::Event,
            Kind::Reminders => EKEntityType::Reminder,
        }
    }

    pub(super) fn status(kind: Kind) -> Status {
        // SAFETY: a class method without arguments that hold state.
        match unsafe { EKEventStore::authorizationStatusForEntityType(entity(kind)) } {
            EKAuthorizationStatus::FullAccess => Status::Granted,
            EKAuthorizationStatus::NotDetermined => Status::NotAsked,
            _ => Status::Denied,
        }
    }

    pub(super) fn ask(kind: Kind) -> Result<(), CalendarError> {
        match status(kind) {
            Status::Granted => return Ok(()),
            Status::Denied => return Err(CalendarError::AccessDenied(kind)),
            Status::NotAsked => {}
        }
        let (tx, rx) = mpsc::channel();
        let answer = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            // The receiver may be gone after a timeout; nobody is left to tell.
            let _ = tx.send(granted.as_bool());
        });
        // The store lives until the answer came: dropped earlier, the dialog's answer may never arrive.
        // SAFETY: a plain constructor.
        let store = unsafe { EKEventStore::new() };
        // SAFETY: the store and the block live across the call; EventKit copies the block.
        unsafe {
            match kind {
                Kind::Events => {
                    store.requestFullAccessToEventsWithCompletion(&*answer as *const _ as *mut _);
                }
                Kind::Reminders => {
                    store
                        .requestFullAccessToRemindersWithCompletion(&*answer as *const _ as *mut _);
                }
            }
        }
        let granted = rx.recv_timeout(ANSWER_TIMEOUT);
        drop(store);
        match granted {
            Ok(true) => Ok(()),
            _ => Err(CalendarError::AccessDenied(kind)),
        }
    }

    /// The Gregorian calendar in the Mac's time zone: the dates going in and out are Gregorian whatever calendar the
    /// owner's Mac shows (a Buddhist year would be read as a date two thousand years ahead).
    pub(super) fn gregorian() -> Retained<NSCalendar> {
        // SAFETY: a static string of Foundation's.
        let gregorian = unsafe { NSCalendarIdentifierGregorian };
        NSCalendar::calendarWithIdentifier(gregorian).unwrap_or_else(NSCalendar::currentCalendar)
    }

    /// A store this app may read `kind` from.
    pub(super) fn store(kind: Kind) -> Result<Retained<EKEventStore>, CalendarError> {
        if status(kind) != Status::Granted {
            return Err(CalendarError::AccessDenied(kind));
        }
        // SAFETY: a plain constructor.
        Ok(unsafe { EKEventStore::new() })
    }

    pub(super) fn collections(kind: Kind) -> Result<Vec<Collection>, CalendarError> {
        let store = store(kind)?;
        // SAFETY: reading properties of objects this thread holds.
        unsafe {
            Ok(store
                .calendarsForEntityType(entity(kind))
                .iter()
                .map(|calendar| Collection {
                    id: calendar.calendarIdentifier().to_string(),
                    title: clean(&calendar.title().to_string()),
                    account: calendar
                        .source()
                        .map(|source| clean(&source.title().to_string()))
                        .unwrap_or_default(),
                    writable: calendar.allowsContentModifications(),
                })
                .collect())
        }
    }

    /// Every calendar of `kind` but the `closed` ones. Never `nil`: to EventKit that would mean all of them.
    fn open_calendars(
        store: &EKEventStore,
        kind: Kind,
        closed: &[String],
    ) -> Retained<NSArray<EKCalendar>> {
        // SAFETY: reading properties of objects this thread holds.
        unsafe {
            let open: Vec<Retained<EKCalendar>> = store
                .calendarsForEntityType(entity(kind))
                .iter()
                .filter(|calendar| !closed.contains(&calendar.calendarIdentifier().to_string()))
                .collect();
            NSArray::from_retained_slice(&open)
        }
    }

    /// The start of `day` on the owner's clock.
    pub(super) fn start_of(
        calendar: &NSCalendar,
        day: super::Day,
    ) -> Result<Retained<NSDate>, CalendarError> {
        let parts = NSDateComponents::new();
        parts.setYear(day.year as isize);
        parts.setMonth(day.month as isize);
        parts.setDay(day.day as isize);
        let date = calendar
            .dateFromComponents(&parts)
            .ok_or(CalendarError::Invalid("no such day"))?;
        // A day that does not exist (31 February) rolls over: it is refused, not moved.
        let back = moment(calendar, &date).day;
        if back != day {
            return Err(CalendarError::Invalid("no such day"));
        }
        Ok(date)
    }

    pub(super) fn days_later(
        calendar: &NSCalendar,
        date: &NSDate,
        days: i64,
    ) -> Option<Retained<NSDate>> {
        calendar.dateByAddingUnit_value_toDate_options(
            NSCalendarUnit::Day,
            days as isize,
            date,
            NSCalendarOptions::empty(),
        )
    }

    pub(super) fn moment(calendar: &NSCalendar, date: &NSDate) -> Moment {
        let units = NSCalendarUnit::Year
            | NSCalendarUnit::Month
            | NSCalendarUnit::Day
            | NSCalendarUnit::Hour
            | NSCalendarUnit::Minute
            | NSCalendarUnit::Weekday;
        let parts = calendar.components_fromDate(units, date);
        Moment {
            day: Day {
                year: parts.year() as i32,
                month: parts.month() as u32,
                day: parts.day() as u32,
            },
            hour: parts.hour() as u32,
            minute: parts.minute() as u32,
            weekday: weekday(parts.weekday() as i64),
        }
    }

    pub(super) fn now() -> Option<Moment> {
        Some(moment(&gregorian(), &NSDate::now()))
    }

    pub(super) fn events(
        from: Day,
        days: u32,
        closed: &[String],
    ) -> Result<Vec<Event>, CalendarError> {
        let store = store(Kind::Events)?;
        let calendar = gregorian();
        let start = start_of(&calendar, from)?;
        let end = days_later(&calendar, &start, i64::from(days))
            .ok_or(CalendarError::Invalid("no such day"))?;
        // SAFETY: the predicate comes from the same store; everything lives on this thread.
        unsafe {
            let calendars = open_calendars(&store, Kind::Events, closed);
            if calendars.is_empty() {
                return Ok(Vec::new());
            }
            let predicate = store.predicateForEventsWithStartDate_endDate_calendars(
                &start,
                &end,
                Some(&calendars),
            );
            Ok(store
                .eventsMatchingPredicate(&predicate)
                .iter()
                .map(|event| {
                    let location = event
                        .location()
                        .map(|location| clean(&location.to_string()))
                        .filter(|location| !location.is_empty());
                    let calendar_item = event.calendar();
                    let writable = calendar_item
                        .as_ref()
                        .is_some_and(|calendar| calendar.allowsContentModifications());
                    Event {
                        id: event
                            .eventIdentifier()
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                        changeable: writable
                            && !event.hasAttendees()
                            && !event.hasRecurrenceRules(),
                        repeats: event.hasRecurrenceRules() || event.isDetached(),
                        guests: event.hasAttendees(),
                        writable,
                        title: clean(&event.title().to_string()),
                        start: moment(&calendar, &event.startDate()),
                        end: moment(&calendar, &event.endDate()),
                        all_day: event.isAllDay(),
                        location,
                        calendar: calendar_item
                            .map(|calendar| clean(&calendar.title().to_string()))
                            .unwrap_or_default(),
                    }
                })
                .collect())
        }
    }

    /// Open reminders with `done_days` `None`, else the ones ticked off in the last `done_days` days.
    pub(super) fn reminders(
        done_days: Option<u32>,
        closed: &[String],
    ) -> Result<Vec<Reminder>, CalendarError> {
        let store = store(Kind::Reminders)?;
        let calendar = gregorian();
        // SAFETY: the predicate comes from the same store; the block owns only a channel sender, and EventKit copies
        // it; the reminders it hands over are read on this thread after the fetch is done.
        unsafe {
            let lists = open_calendars(&store, Kind::Reminders, closed);
            if lists.is_empty() {
                return Ok(Vec::new());
            }
            let predicate = match done_days {
                None => store.predicateForIncompleteRemindersWithDueDateStarting_ending_calendars(
                    None,
                    None,
                    Some(&lists),
                ),
                Some(days) => {
                    let today = calendar.startOfDayForDate(&NSDate::now());
                    let since = days_later(&calendar, &today, -i64::from(days - 1))
                        .ok_or(CalendarError::Invalid("no such day"))?;
                    store.predicateForCompletedRemindersWithCompletionDateStarting_ending_calendars(
                        Some(&since),
                        Some(&NSDate::now()),
                        Some(&lists),
                    )
                }
            };
            // EventKit's objects stay on the queue that handed them over: the block copies what is needed into plain
            // values, and only those cross to this thread.
            let (tx, rx) = mpsc::channel::<Vec<Reminder>>();
            let done = RcBlock::new(move |found: *mut NSArray<EKReminder>| {
                let calendar = gregorian();
                let found = Retained::retain(found);
                let reminders = found
                    .map(|found| {
                        found
                            .iter()
                            .map(|reminder| plain(&calendar, &reminder))
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = tx.send(reminders);
            });
            let request = store.fetchRemindersMatchingPredicate_completion(&predicate, &done);
            match rx.recv_timeout(FETCH_TIMEOUT) {
                Ok(reminders) => Ok(reminders),
                Err(_) => {
                    store.cancelFetchRequest(&request);
                    Err(CalendarError::TimedOut)
                }
            }
        }
    }

    /// A reminder as plain values.
    pub(super) fn plain(calendar: &NSCalendar, reminder: &EKReminder) -> Reminder {
        // SAFETY: reading properties of an object the caller holds, on the queue that handed it over.
        unsafe {
            Reminder {
                id: reminder.calendarItemIdentifier().to_string(),
                title: clean(&reminder.title().to_string()),
                due: reminder
                    .dueDateComponents()
                    .and_then(|parts| due(calendar, &parts)),
                list: reminder
                    .calendar()
                    .map(|list| clean(&list.title().to_string()))
                    .unwrap_or_default(),
                done: reminder
                    .completionDate()
                    .filter(|_| reminder.isCompleted())
                    .map(|date| moment(calendar, &date)),
            }
        }
    }

    /// A reminder's due date: its day, with a time when it has one.
    fn due(calendar: &NSCalendar, parts: &NSDateComponents) -> Option<Due> {
        let defined = |value: isize| (value != NSDateComponentUndefined).then_some(value);
        let day = Day {
            year: defined(parts.year())? as i32,
            month: defined(parts.month())? as u32,
            day: defined(parts.day())? as u32,
        };
        let time = defined(parts.hour())
            .map(|hour| (hour as u32, defined(parts.minute()).unwrap_or(0) as u32));
        // The weekday from the calendar, not the components: they rarely carry one.
        let weekday = start_of(calendar, day)
            .ok()
            .map(|date| moment(calendar, &date).weekday)?;
        Some(Due { day, weekday, time })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{CalendarError, Collection, Day, Event, Kind, Reminder, Status};

    pub(super) fn status(_kind: Kind) -> Status {
        Status::Denied
    }
    pub(super) fn now() -> Option<super::Moment> {
        None
    }
    pub(super) fn ask(_kind: Kind) -> Result<(), CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn collections(_kind: Kind) -> Result<Vec<Collection>, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn events(
        _from: Day,
        _days: u32,
        _closed: &[String],
    ) -> Result<Vec<Event>, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn reminders(
        _done_days: Option<u32>,
        _closed: &[String],
    ) -> Result<Vec<Reminder>, CalendarError> {
        Err(CalendarError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Day = Day {
        year: 2026,
        month: 11,
        day: 8,
    };

    #[test]
    fn a_question_too_wide_or_of_no_day_is_refused_before_eventkit() {
        assert_eq!(
            events(DAY, 0, &[]),
            Err(CalendarError::Invalid("between 1 and 31 days"))
        );
        assert_eq!(
            events(DAY, MAX_DAYS + 1, &[]),
            Err(CalendarError::Invalid("between 1 and 31 days"))
        );
        assert_eq!(
            events(Day { month: 13, ..DAY }, 1, &[]),
            Err(CalendarError::Invalid("no such day"))
        );
        assert_eq!(
            done_reminders(0, &[]),
            Err(CalendarError::Invalid("between 1 and 7 days"))
        );
        assert_eq!(
            done_reminders(8, &[]),
            Err(CalendarError::Invalid("between 1 and 7 days"))
        );
    }

    #[test]
    fn texts_come_out_on_one_line_and_short() {
        assert_eq!(clean("Zahnarzt\n\tDr. Weber "), "Zahnarzt Dr. Weber");
        assert_eq!(clean(&"x".repeat(500)).chars().count(), MAX_TEXT_CHARS);
        assert_eq!(clean("pa\u{200B}ss\u{202E}wort\u{E0041}"), "passwort");
    }

    #[test]
    fn the_week_starts_on_monday() {
        assert_eq!(weekday(1), 7);
        assert_eq!(weekday(2), 1);
        assert_eq!(weekday(7), 6);
        assert_eq!(DAY.to_string(), "2026-11-08");
    }

    /// Reads this Mac's calendars and reminders: run by hand once access was given
    /// (`cargo test -p axiomata-sources calendar -- --ignored --nocapture`).
    #[test]
    #[ignore = "reads the owner's calendars"]
    fn what_this_mac_has_this_week() {
        println!("events: {:?}", status(Kind::Events));
        println!("reminders: {:?}", status(Kind::Reminders));
        for event in events(DAY, 7, &[]).unwrap_or_default().iter().take(5) {
            println!("{event:?}");
        }
        for reminder in open_reminders(&[]).unwrap_or_default().iter().take(5) {
            println!("{reminder:?}");
        }
    }
}
