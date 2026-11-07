//! Reversible writes to the calendars and reminders (§8k Q5): create an event (also a
//! repeating one) or a reminder, tick a reminder off or open it again, move or rename an event that touches nobody
//! else — and take back what was created, as long as it is unchanged. Nothing here deletes what the owner made, and
//! nothing writes into a calendar or list the owner closed. The caller has its own "yes"; this only does.

use super::{CalendarError, Collection, Day, Kind, Moment};

/// The largest repeat interval taken ("every 99 weeks"); beyond it the input is a typo, not a plan.
pub const MAX_INTERVAL: u32 = 99;
/// The most occurrences a series may be given.
const MAX_COUNT: u32 = 999;
/// The longest event: 14 days (§8k Q20).
const MAX_MINUTES: u32 = 14 * 24 * 60;
/// The earliest a hint may come before its time: one week.
const MAX_ALERT_MINUTES: u32 = 7 * 24 * 60;
/// The longest title and location, in characters: they go into calendars that sync to every device.
const MAX_TEXT_CHARS: usize = 256;

/// How often an event repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// When a repetition ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RepeatEnd {
    Never,
    /// After this many occurrences.
    Count(u32),
    /// With the last occurrence on or before this day.
    Until(Day),
}

/// How an event repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Repeat {
    pub frequency: Frequency,
    /// Every `interval` days/weeks/months/years, `1..=MAX_INTERVAL`.
    pub interval: u32,
    pub end: RepeatEnd,
}

/// An event to create.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewEvent {
    pub title: String,
    /// The calendar's identifier; `None` for the Mac's default calendar for new events.
    pub calendar: Option<String>,
    pub day: Day,
    /// Hour and minute on the owner's clock; `None` for an all-day event.
    pub time: Option<(u32, u32)>,
    /// How long it lasts (ignored all day, where it covers `minutes` rounded up to whole days, at least one).
    pub minutes: u32,
    pub location: Option<String>,
    /// A hint this many minutes before.
    pub alert_minutes: Option<u32>,
    pub repeat: Option<Repeat>,
}

/// A reminder to create.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewReminder {
    pub title: String,
    /// The list's identifier; `None` for the Mac's default list.
    pub list: Option<String>,
    /// When it is due: a day, with a time when it has one.
    pub due: Option<(Day, Option<(u32, u32)>)>,
}

/// What was created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    /// EventKit's identifier: what takes it back.
    pub id: String,
    /// The calendar's or list's name.
    pub collection: String,
}

/// An event as it stands, to tell whether the owner changed it since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub title: String,
    pub start: Moment,
    pub end: Moment,
    pub all_day: bool,
    pub location: Option<String>,
    /// It has guests: taking it back would send them a cancellation.
    pub attendees: bool,
    /// It repeats.
    pub repeats: bool,
    /// It has notes.
    pub notes: bool,
}

impl NewEvent {
    /// Checks what EventKit would otherwise reject — or raise an exception for, which would end the process.
    fn check(&self) -> Result<(), CalendarError> {
        text(&self.title)?;
        if let Some(location) = &self.location {
            plain(location)?;
        }
        day(self.day)?;
        if let Some(time) = self.time {
            clock(time)?;
        }
        if self.minutes == 0 || self.minutes > MAX_MINUTES {
            return Err(CalendarError::Invalid(
                "an event lasts between a minute and 14 days",
            ));
        }
        if self
            .alert_minutes
            .is_some_and(|alert| alert > MAX_ALERT_MINUTES)
        {
            return Err(CalendarError::Invalid("a hint comes at most a week before"));
        }
        if let Some(repeat) = self.repeat {
            if !(1..=MAX_INTERVAL).contains(&repeat.interval) {
                return Err(CalendarError::Invalid("the interval lies between 1 and 99"));
            }
            match repeat.end {
                RepeatEnd::Count(count) if !(1..=MAX_COUNT).contains(&count) => {
                    return Err(CalendarError::Invalid("between 1 and 999 occurrences"));
                }
                RepeatEnd::Until(until) => {
                    day(until)?;
                    if until < self.day {
                        return Err(CalendarError::Invalid(
                            "the repetition ends before it starts",
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl NewReminder {
    fn check(&self) -> Result<(), CalendarError> {
        text(&self.title)?;
        if let Some((due, time)) = self.due {
            day(due)?;
            if let Some(time) = time {
                clock(time)?;
            }
        }
        Ok(())
    }
}

fn text(title: &str) -> Result<(), CalendarError> {
    if title.trim().is_empty() {
        return Err(CalendarError::Invalid("the title is empty"));
    }
    plain(title)
}

fn plain(text: &str) -> Result<(), CalendarError> {
    if text.chars().count() > MAX_TEXT_CHARS
        || text.chars().any(|c| c.is_control() || super::invisible(c))
    {
        return Err(CalendarError::Invalid(
            "a text is too long or holds hidden characters",
        ));
    }
    Ok(())
}

fn day(day: Day) -> Result<(), CalendarError> {
    if super::valid(day) {
        Ok(())
    } else {
        Err(CalendarError::Invalid("no such day"))
    }
}

fn clock((hour, minute): (u32, u32)) -> Result<(), CalendarError> {
    if hour < 24 && minute < 60 {
        Ok(())
    } else {
        Err(CalendarError::Invalid("no such time"))
    }
}

/// Creates `event` in its calendar — never one of the `closed` — and returns what takes it back.
///
/// # Errors
///
/// [`CalendarError::Invalid`] for a bad field, [`CalendarError::ReadOnly`] for a closed or read-only calendar,
/// [`CalendarError::AccessDenied`] without access, [`CalendarError::Refused`] when EventKit says no.
pub fn create_event(event: &NewEvent, closed: &[String]) -> Result<Created, CalendarError> {
    event.check()?;
    imp::create_event(event, closed)
}

/// Creates `reminder` in its list — never one of the `closed`.
///
/// # Errors
///
/// As [`create_event`].
pub fn create_reminder(
    reminder: &NewReminder,
    closed: &[String],
) -> Result<Created, CalendarError> {
    reminder.check()?;
    imp::create_reminder(reminder, closed)
}

/// Ticks reminder `id` off (`done`) or opens it again; whether it was done before.
///
/// # Errors
///
/// [`CalendarError::NotFound`], [`CalendarError::ReadOnly`] (also for a closed list), [`CalendarError::Refused`].
pub fn complete_reminder(id: &str, done: bool, closed: &[String]) -> Result<bool, CalendarError> {
    imp::complete_reminder(id, done, closed)
}

/// Whether reminder `id` is ticked off.
///
/// # Errors
///
/// [`CalendarError::NotFound`].
pub fn reminder_done(id: &str) -> Result<bool, CalendarError> {
    imp::reminder_done(id)
}

/// Moves event `id` to `day` at `time` (`None`: all day) for `minutes` (`None`: as long as it was), and renames it to
/// `title` when given; only an event that touches nobody else (no attendees, no repetition) in an open, writable
/// calendar. Returns how it stood before.
///
/// # Errors
///
/// [`CalendarError::NotFound`], [`CalendarError::ReadOnly`] for one that may not be changed,
/// [`CalendarError::Invalid`],
/// [`CalendarError::Refused`].
pub fn move_event(
    id: &str,
    day_to: Day,
    time: Option<(u32, u32)>,
    minutes: Option<u32>,
    title: Option<&str>,
    closed: &[String],
) -> Result<Snapshot, CalendarError> {
    day(day_to)?;
    if let Some(time) = time {
        clock(time)?;
    }
    if minutes.is_some_and(|minutes| minutes == 0 || minutes > MAX_MINUTES) {
        return Err(CalendarError::Invalid(
            "an event lasts between a minute and 14 days",
        ));
    }
    if let Some(title) = title {
        text(title)?;
    }
    imp::move_event(id, day_to, time, minutes, title, closed)
}

/// How event `id` stands now.
///
/// # Errors
///
/// [`CalendarError::NotFound`].
pub fn event_snapshot(id: &str) -> Result<Snapshot, CalendarError> {
    imp::event_snapshot(id)
}

/// The calendar (or list) new events (reminders) go to on this Mac, when there is one.
///
/// # Errors
///
/// [`CalendarError::AccessDenied`] without access.
pub fn default_collection(kind: Kind) -> Result<Option<Collection>, CalendarError> {
    imp::default_collection(kind)
}

/// Takes event `id` back — the whole series of a repeating one. Only for what the caller created itself.
///
/// # Errors
///
/// [`CalendarError::NotFound`], [`CalendarError::Refused`].
pub fn remove_event(id: &str) -> Result<(), CalendarError> {
    imp::remove_event(id)
}

/// Takes reminder `id` back. Only for what the caller created itself.
///
/// # Errors
///
/// [`CalendarError::NotFound`], [`CalendarError::Refused`].
pub fn remove_reminder(id: &str) -> Result<(), CalendarError> {
    imp::remove_reminder(id)
}

/// A deleted event, kept to bring it back: as an event of its own when one occurrence of a series was deleted, as the
/// series when the whole was. Guests and invitations do not come back.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeletedEvent {
    pub event: NewEvent,
    pub notes: Option<String>,
}

/// A deleted reminder, kept to bring it back.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeletedReminder {
    pub reminder: NewReminder,
    pub notes: Option<String>,
    pub done: bool,
}

/// What of an event a delete takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The one event, or the one occurrence of a series, that starts at this moment.
    One(Moment),
    /// The whole series.
    Series,
}

/// Deletes what `scope` names of event `id` — only while it is still called `title` (and, for one, still starts then)
/// — and returns what brings it back. Refused, before anything is deleted: an event with guests (they would get a
/// cancellation), one in a closed or read-only calendar, a series whose rule is more than a plain "every N
/// days/weeks/months/years", one with more than one hint or a hint at a fixed time, and one that could not be
/// brought back as it was. A whole series comes back by its rule: an occurrence the owner moved or removed by hand
/// comes back where the rule puts it.
///
/// # Errors
///
/// [`CalendarError::ReadOnly`] for one that may not be deleted, [`CalendarError::Invalid`] for one that would not come
/// back, [`CalendarError::NotFound`] (also when it changed since), [`CalendarError::Refused`].
pub fn delete_event(
    id: &str,
    scope: Scope,
    title: &str,
    closed: &[String],
) -> Result<DeletedEvent, CalendarError> {
    imp::delete_event(id, scope, title, closed)
}

/// Deletes reminder `id` — only while it is still called `title`, and never a repeating one (its repetition would not
/// come back) — and returns what brings it back.
///
/// # Errors
///
/// As [`delete_event`].
pub fn delete_reminder(
    id: &str,
    title: &str,
    closed: &[String],
) -> Result<DeletedReminder, CalendarError> {
    imp::delete_reminder(id, title, closed)
}

/// Brings a deleted event back, as a new one.
///
/// # Errors
///
/// As [`create_event`].
pub fn restore_event(kept: &DeletedEvent, closed: &[String]) -> Result<Created, CalendarError> {
    kept.event.check()?;
    imp::restore_event(kept, closed)
}

/// Brings a deleted reminder back, as a new one.
///
/// # Errors
///
/// As [`create_reminder`].
pub fn restore_reminder(
    kept: &DeletedReminder,
    closed: &[String],
) -> Result<Created, CalendarError> {
    kept.reminder.check()?;
    imp::restore_reminder(kept, closed)
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::AllocAnyThread as _;
    use objc2::rc::Retained;
    use objc2_event_kit::{
        EKAlarm, EKCalendar, EKEntityType, EKEvent, EKEventStore, EKRecurrenceEnd,
        EKRecurrenceFrequency, EKRecurrenceRule, EKReminder, EKSpan,
    };
    use objc2_foundation::{NSCalendar, NSDate, NSDateComponents, NSString};

    use super::super::imp::{days_later, gregorian, moment, start_of, store};
    use super::super::{CalendarError, Day, Kind};
    use super::{Created, Frequency, NewEvent, NewReminder, RepeatEnd, Snapshot};

    /// `day` at `time` on the owner's clock; its start for none.
    fn at(
        calendar: &NSCalendar,
        day: Day,
        time: Option<(u32, u32)>,
    ) -> Result<Retained<NSDate>, CalendarError> {
        let start = start_of(calendar, day)?;
        let Some((hour, minute)) = time else {
            return Ok(start);
        };
        let parts = NSDateComponents::new();
        parts.setYear(day.year as isize);
        parts.setMonth(day.month as isize);
        parts.setDay(day.day as isize);
        parts.setHour(hour as isize);
        parts.setMinute(minute as isize);
        calendar
            .dateFromComponents(&parts)
            .ok_or(CalendarError::Invalid("no such time"))
    }

    fn minutes_later(date: &NSDate, minutes: u32) -> Retained<NSDate> {
        date.dateByAddingTimeInterval(f64::from(minutes) * 60.0)
    }

    /// The calendar or list `id` (the default for new ones without), when it is open and writable.
    fn target(
        store: &EKEventStore,
        kind: Kind,
        id: Option<&str>,
        closed: &[String],
    ) -> Result<Retained<EKCalendar>, CalendarError> {
        // SAFETY: reading properties of objects this thread holds.
        unsafe {
            let calendar = match id {
                Some(id) => store
                    .calendarWithIdentifier(&NSString::from_str(id))
                    .ok_or(CalendarError::NotFound)?,
                None => match kind {
                    Kind::Events => store.defaultCalendarForNewEvents(),
                    Kind::Reminders => store.defaultCalendarForNewReminders(),
                }
                .ok_or(CalendarError::NotFound)?,
            };
            let entity = match kind {
                Kind::Events => EKEntityType::Event,
                Kind::Reminders => EKEntityType::Reminder,
            };
            let right_kind = store
                .calendarsForEntityType(entity)
                .iter()
                .any(|other| other.calendarIdentifier() == calendar.calendarIdentifier());
            if !right_kind {
                return Err(CalendarError::NotFound);
            }
            if !calendar.allowsContentModifications()
                || closed.contains(&calendar.calendarIdentifier().to_string())
            {
                return Err(CalendarError::ReadOnly);
            }
            Ok(calendar)
        }
    }

    fn created(
        id: Option<Retained<NSString>>,
        calendar: &EKCalendar,
    ) -> Result<Created, CalendarError> {
        let id = id
            .map(|id| id.to_string())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| CalendarError::Refused("it has no identifier".into()))?;
        // SAFETY: reading a property of an object this thread holds.
        let collection = super::super::clean(&unsafe { calendar.title() }.to_string());
        Ok(Created { id, collection })
    }

    pub(super) fn create_event(
        event: &NewEvent,
        closed: &[String],
    ) -> Result<Created, CalendarError> {
        create_event_with(event, None, closed)
    }

    /// [`create_event`] with the notes it had: what brings a deleted event back.
    fn create_event_with(
        event: &NewEvent,
        notes: Option<&str>,
        closed: &[String],
    ) -> Result<Created, CalendarError> {
        let store = store(Kind::Events)?;
        let calendar = gregorian();
        let target = target(&store, Kind::Events, event.calendar.as_deref(), closed)?;
        let start = at(&calendar, event.day, event.time)?;
        let end = match event.time {
            Some(_) => minutes_later(&start, event.minutes),
            // All day: from the day's start to the start of the day after the last one.
            None => days_later(
                &calendar,
                &start,
                i64::from(event.minutes.div_ceil(24 * 60).max(1)),
            )
            .ok_or(CalendarError::Invalid("no such day"))?,
        };
        // SAFETY: plain EventKit calls on objects created and used on this one thread.
        unsafe {
            let item = EKEvent::eventWithEventStore(&store);
            item.setTitle(Some(&NSString::from_str(event.title.trim())));
            item.setCalendar(Some(&target));
            item.setAllDay(event.time.is_none());
            item.setStartDate(Some(&start));
            item.setEndDate(Some(&end));
            if let Some(location) = event
                .location
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
            {
                item.setLocation(Some(&NSString::from_str(location)));
            }
            if let Some(alert) = event.alert_minutes {
                item.addAlarm(&EKAlarm::alarmWithRelativeOffset(-f64::from(alert) * 60.0));
            }
            if let Some(repeat) = event.repeat {
                let end = match repeat.end {
                    RepeatEnd::Never => None,
                    RepeatEnd::Count(count) => Some(
                        EKRecurrenceEnd::recurrenceEndWithOccurrenceCount(count as usize),
                    ),
                    RepeatEnd::Until(until) => {
                        let last = start_of(&calendar, until)?;
                        // The last day whole, and not a moment more: EventKit counts an occurrence that starts
                        // at the end date itself, so the end lies a second before the next day.
                        let last = days_later(&calendar, &last, 1)
                            .map(|next| next.dateByAddingTimeInterval(-1.0))
                            .unwrap_or(last);
                        Some(EKRecurrenceEnd::recurrenceEndWithEndDate(&last))
                    }
                };
                let frequency = match repeat.frequency {
                    Frequency::Daily => EKRecurrenceFrequency::Daily,
                    Frequency::Weekly => EKRecurrenceFrequency::Weekly,
                    Frequency::Monthly => EKRecurrenceFrequency::Monthly,
                    Frequency::Yearly => EKRecurrenceFrequency::Yearly,
                };
                let rule = EKRecurrenceRule::initRecurrenceWithFrequency_interval_end(
                    EKRecurrenceRule::alloc(),
                    frequency,
                    repeat.interval as isize,
                    end.as_deref(),
                );
                item.addRecurrenceRule(&rule);
            }
            if let Some(notes) = notes {
                item.setNotes(Some(&NSString::from_str(notes)));
            }
            store
                .saveEvent_span_error(&item, EKSpan::FutureEvents)
                .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))?;
            created(item.eventIdentifier(), &target)
        }
    }

    pub(super) fn create_reminder(
        reminder: &NewReminder,
        closed: &[String],
    ) -> Result<Created, CalendarError> {
        create_reminder_with(reminder, None, false, closed)
    }

    /// [`create_reminder`] with the notes it had and ticked off as it was: what brings a deleted one back.
    fn create_reminder_with(
        reminder: &NewReminder,
        notes: Option<&str>,
        done: bool,
        closed: &[String],
    ) -> Result<Created, CalendarError> {
        let store = store(Kind::Reminders)?;
        let target = target(&store, Kind::Reminders, reminder.list.as_deref(), closed)?;
        // SAFETY: plain EventKit calls on objects created and used on this one thread.
        unsafe {
            let item = EKReminder::reminderWithEventStore(&store);
            item.setTitle(Some(&NSString::from_str(reminder.title.trim())));
            item.setCalendar(Some(&target));
            if let Some((day, time)) = reminder.due {
                // Checked through the calendar first: a day that does not exist is refused, not rolled over.
                start_of(&gregorian(), day)?;
                let parts = NSDateComponents::new();
                parts.setYear(day.year as isize);
                parts.setMonth(day.month as isize);
                parts.setDay(day.day as isize);
                if let Some((hour, minute)) = time {
                    parts.setHour(hour as isize);
                    parts.setMinute(minute as isize);
                }
                item.setDueDateComponents(Some(&parts));
                // A reminder with a time gives its hint then, as Reminders.app does.
                if time.is_some()
                    && let Some(date) = gregorian().dateFromComponents(&parts)
                {
                    item.addAlarm(&EKAlarm::alarmWithAbsoluteDate(&date));
                }
            }
            if let Some(notes) = notes {
                item.setNotes(Some(&NSString::from_str(notes)));
            }
            if done {
                item.setCompleted(true);
            }
            store
                .saveReminder_commit_error(&item, true)
                .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))?;
            created(Some(item.calendarItemIdentifier()), &target)
        }
    }

    pub(super) fn reminder(
        store: &EKEventStore,
        id: &str,
    ) -> Result<Retained<EKReminder>, CalendarError> {
        // SAFETY: a lookup by identifier on a store this thread holds.
        let item = unsafe { store.calendarItemWithIdentifier(&NSString::from_str(id)) }
            .ok_or(CalendarError::NotFound)?;
        item.downcast::<EKReminder>()
            .map_err(|_| CalendarError::NotFound)
    }

    pub(super) fn event(
        store: &EKEventStore,
        id: &str,
    ) -> Result<Retained<EKEvent>, CalendarError> {
        // SAFETY: a lookup by identifier on a store this thread holds.
        unsafe { store.eventWithIdentifier(&NSString::from_str(id)) }.ok_or(CalendarError::NotFound)
    }

    /// Whether `item`'s calendar is open and writable.
    pub(super) fn may_change(calendar: Option<Retained<EKCalendar>>, closed: &[String]) -> bool {
        // SAFETY: reading properties of an object this thread holds.
        calendar.is_some_and(|calendar| unsafe {
            calendar.allowsContentModifications()
                && !closed.contains(&calendar.calendarIdentifier().to_string())
        })
    }

    pub(super) fn complete_reminder(
        id: &str,
        done: bool,
        closed: &[String],
    ) -> Result<bool, CalendarError> {
        let store = store(Kind::Reminders)?;
        let item = reminder(&store, id)?;
        // SAFETY: plain EventKit calls on objects this thread holds.
        unsafe {
            if !may_change(item.calendar(), closed) {
                return Err(CalendarError::ReadOnly);
            }
            let before = item.isCompleted();
            item.setCompleted(done);
            store
                .saveReminder_commit_error(&item, true)
                .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))?;
            Ok(before)
        }
    }

    pub(super) fn reminder_done(id: &str) -> Result<bool, CalendarError> {
        let store = store(Kind::Reminders)?;
        let item = reminder(&store, id)?;
        // SAFETY: reading a property of an object this thread holds.
        Ok(unsafe { item.isCompleted() })
    }

    fn snapshot(calendar: &NSCalendar, item: &EKEvent) -> Snapshot {
        // SAFETY: reading properties of an object this thread holds.
        unsafe {
            Snapshot {
                title: super::super::clean(&item.title().to_string()),
                start: moment(calendar, &item.startDate()),
                end: moment(calendar, &item.endDate()),
                all_day: item.isAllDay(),
                location: item
                    .location()
                    .map(|location| super::super::clean(&location.to_string()))
                    .filter(|location| !location.is_empty()),
                attendees: item.hasAttendees(),
                repeats: item.hasRecurrenceRules(),
                notes: item.hasNotes(),
            }
        }
    }

    pub(super) fn move_event(
        id: &str,
        day: Day,
        time: Option<(u32, u32)>,
        minutes: Option<u32>,
        title: Option<&str>,
        closed: &[String],
    ) -> Result<Snapshot, CalendarError> {
        let store = store(Kind::Events)?;
        let calendar = gregorian();
        let item = event(&store, id)?;
        // SAFETY: plain EventKit calls on objects this thread holds.
        unsafe {
            if !may_change(item.calendar(), closed)
                || item.hasAttendees()
                || item.hasRecurrenceRules()
            {
                return Err(CalendarError::ReadOnly);
            }
            let before = snapshot(&calendar, &item);
            let lasted = item.endDate().timeIntervalSinceDate(&item.startDate());
            let start = at(&calendar, day, time)?;
            let end = match (time, minutes) {
                (Some(_), Some(minutes)) => minutes_later(&start, minutes),
                (Some(_), None) if !before.all_day => {
                    start.dateByAddingTimeInterval(lasted.max(60.0))
                }
                (Some(_), None) => minutes_later(&start, 60),
                (None, minutes) => days_later(
                    &calendar,
                    &start,
                    i64::from(minutes.map_or(1, |minutes| minutes.div_ceil(24 * 60).max(1))),
                )
                .ok_or(CalendarError::Invalid("no such day"))?,
            };
            item.setAllDay(time.is_none());
            item.setStartDate(Some(&start));
            item.setEndDate(Some(&end));
            if let Some(title) = title {
                item.setTitle(Some(&NSString::from_str(title.trim())));
            }
            store
                .saveEvent_span_error(&item, EKSpan::ThisEvent)
                .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))?;
            Ok(before)
        }
    }

    pub(super) fn event_snapshot(id: &str) -> Result<Snapshot, CalendarError> {
        let store = store(Kind::Events)?;
        let item = event(&store, id)?;
        Ok(snapshot(&gregorian(), &item))
    }

    pub(super) fn default_collection(
        kind: Kind,
    ) -> Result<Option<super::Collection>, CalendarError> {
        let store = store(kind)?;
        // SAFETY: reading properties of objects this thread holds.
        unsafe {
            let calendar = match kind {
                Kind::Events => store.defaultCalendarForNewEvents(),
                Kind::Reminders => store.defaultCalendarForNewReminders(),
            };
            Ok(calendar.map(|calendar| super::Collection {
                id: calendar.calendarIdentifier().to_string(),
                title: super::super::clean(&calendar.title().to_string()),
                account: calendar
                    .source()
                    .map(|source| super::super::clean(&source.title().to_string()))
                    .unwrap_or_default(),
                writable: calendar.allowsContentModifications(),
            }))
        }
    }

    /// A rule's repetition as plain values.
    fn repetition(calendar: &NSCalendar, item: &EKEvent) -> Option<super::Repeat> {
        // SAFETY: reading properties of objects this thread holds.
        unsafe {
            let rules = item.recurrenceRules()?;
            let rule = rules.firstObject()?;
            let frequency = match rule.frequency() {
                EKRecurrenceFrequency::Daily => Frequency::Daily,
                EKRecurrenceFrequency::Weekly => Frequency::Weekly,
                EKRecurrenceFrequency::Monthly => Frequency::Monthly,
                _ => Frequency::Yearly,
            };
            let end = match rule.recurrenceEnd() {
                None => RepeatEnd::Never,
                Some(end) => match end.endDate() {
                    // An end that lies before the occurrences' time on its day leaves that day's one out.
                    Some(date) => {
                        let last = moment(calendar, &date);
                        let first = moment(calendar, &item.startDate());
                        let day = if (last.hour, last.minute) < (first.hour, first.minute) {
                            start_of(calendar, last.day)
                                .ok()
                                .and_then(|start| days_later(calendar, &start, -1))
                                .map_or(last.day, |before| moment(calendar, &before).day)
                        } else {
                            last.day
                        };
                        RepeatEnd::Until(day)
                    }
                    None => {
                        RepeatEnd::Count(u32::try_from(end.occurrenceCount()).unwrap_or(1).max(1))
                    }
                },
            };
            Some(super::Repeat {
                frequency,
                interval: u32::try_from(rule.interval())
                    .unwrap_or(1)
                    .clamp(1, super::MAX_INTERVAL),
                end,
            })
        }
    }

    /// Whether the series' rule is one that comes back as it was: one rule, a plain "every N", nothing more.
    fn plain_rule(item: &EKEvent) -> bool {
        // SAFETY: reading properties of objects this thread holds.
        unsafe {
            let Some(rules) = item.recurrenceRules() else {
                return false;
            };
            if rules.count() != 1 {
                return false;
            }
            let Some(rule) = rules.firstObject() else {
                return false;
            };
            let empty = |list: Option<
                Retained<objc2_foundation::NSArray<objc2_foundation::NSNumber>>,
            >| { list.is_none_or(|list| list.count() == 0) };
            rule.daysOfTheWeek().is_none_or(|days| days.count() == 0)
                && empty(rule.daysOfTheMonth())
                && empty(rule.daysOfTheYear())
                && empty(rule.weeksOfTheYear())
                && empty(rule.monthsOfTheYear())
                && empty(rule.setPositions())
        }
    }

    /// The whole days from `start` to `end`, by the calendar: a day of 23 or 25 hours is one day.
    fn days_between(calendar: &NSCalendar, start: &NSDate, end: &NSDate) -> u32 {
        let parts = calendar.components_fromDate_toDate_options(
            objc2_foundation::NSCalendarUnit::Day,
            start,
            end,
            objc2_foundation::NSCalendarOptions::empty(),
        );
        u32::try_from(parts.day()).unwrap_or(1).max(1)
    }

    pub(super) fn delete_event(
        id: &str,
        scope: super::Scope,
        title: &str,
        closed: &[String],
    ) -> Result<super::DeletedEvent, CalendarError> {
        let store = store(Kind::Events)?;
        let calendar = gregorian();
        let first = event(&store, id)?;
        // SAFETY: plain EventKit calls on objects this thread holds.
        unsafe {
            if !may_change(first.calendar(), closed) || first.hasAttendees() {
                return Err(CalendarError::ReadOnly);
            }
            let (item, span) = match scope {
                // The one that starts then: a plain event, or one occurrence of a series (also one the owner moved,
                // detached) — found by its start on its day, in its calendar; never falls through to the series.
                super::Scope::One(at) => {
                    let start = start_of(&calendar, at.day)?;
                    let end = days_later(&calendar, &start, 1)
                        .ok_or(CalendarError::Invalid("no such day"))?;
                    let own = first.calendar().ok_or(CalendarError::NotFound)?;
                    let calendars = objc2_foundation::NSArray::from_retained_slice(&[own]);
                    let predicate = store.predicateForEventsWithStartDate_endDate_calendars(
                        &start,
                        &end,
                        Some(&calendars),
                    );
                    let found = store
                        .eventsMatchingPredicate(&predicate)
                        .iter()
                        .find(|candidate| {
                            candidate
                                .eventIdentifier()
                                .is_some_and(|known| known.to_string() == id)
                                && moment(&calendar, &candidate.startDate()) == at
                        })
                        .ok_or(CalendarError::NotFound)?;
                    (found, EKSpan::ThisEvent)
                }
                super::Scope::Series => {
                    if !first.hasRecurrenceRules() {
                        return Err(CalendarError::Invalid("that is no series"));
                    }
                    if !plain_rule(&first) {
                        return Err(CalendarError::Invalid(
                            "that series repeats in a way that would not come back as it was",
                        ));
                    }
                    (first.clone(), EKSpan::FutureEvents)
                }
            };
            if item.hasAttendees() {
                return Err(CalendarError::ReadOnly);
            }
            // Still the one named: a title changed since is another thing to delete.
            if super::super::clean(&item.title().to_string()) != super::super::clean(title) {
                return Err(CalendarError::NotFound);
            }
            let start = item.startDate();
            let all_day = item.isAllDay();
            let at = moment(&calendar, &start);
            let minutes = if all_day {
                // EventKit ends an all-day event a second before midnight: the second back makes the last day count.
                let end = item.endDate().dateByAddingTimeInterval(1.0);
                days_between(&calendar, &start, &end) * 24 * 60
            } else {
                (item.endDate().timeIntervalSinceDate(&start) / 60.0)
                    .round()
                    .max(1.0) as u32
            };
            // A hint comes back only as one of minutes before: more than one, or one at a fixed time, would not.
            let alarms = item
                .alarms()
                .map(|alarms| alarms.to_vec())
                .unwrap_or_default();
            if alarms.len() > 1 || alarms.iter().any(|alarm| alarm.absoluteDate().is_some()) {
                return Err(CalendarError::Invalid(
                    "its hints would not come back as they were",
                ));
            }
            let alert_minutes = match alarms.first() {
                None => None,
                Some(alarm) => {
                    let offset = -alarm.relativeOffset() / 60.0;
                    if !(0.0..=f64::from(super::MAX_ALERT_MINUTES)).contains(&offset) {
                        return Err(CalendarError::Invalid(
                            "its hint would not come back as it was",
                        ));
                    }
                    Some(offset.round() as u32)
                }
            };
            let kept = super::DeletedEvent {
                event: NewEvent {
                    title: super::super::clean(&item.title().to_string()),
                    calendar: item.calendar().map(|c| c.calendarIdentifier().to_string()),
                    day: at.day,
                    time: (!all_day).then_some((at.hour, at.minute)),
                    minutes,
                    location: item
                        .location()
                        .map(|l| super::super::clean(&l.to_string()))
                        .filter(|l| !l.trim().is_empty()),
                    alert_minutes,
                    // One occurrence comes back as an event of its own; the whole series as a series.
                    repeat: if span == EKSpan::FutureEvents {
                        repetition(&calendar, &item)
                    } else {
                        None
                    },
                },
                notes: item
                    .notes()
                    .map(|n| n.to_string())
                    .filter(|n| !n.trim().is_empty()),
            };
            // Nothing is deleted that could not come back.
            kept.event
                .check()
                .map_err(|_| CalendarError::Invalid("it would not come back as it was"))?;
            store
                .removeEvent_span_error(&item, span)
                .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))?;
            Ok(kept)
        }
    }

    pub(super) fn delete_reminder(
        id: &str,
        title: &str,
        closed: &[String],
    ) -> Result<super::DeletedReminder, CalendarError> {
        let store = store(Kind::Reminders)?;
        let item = reminder(&store, id)?;
        // SAFETY: plain EventKit calls on objects this thread holds.
        unsafe {
            if !may_change(item.calendar(), closed) {
                return Err(CalendarError::ReadOnly);
            }
            // A repeating one would come back once only; a title changed since is another reminder to delete.
            if item.hasRecurrenceRules() {
                return Err(CalendarError::Invalid(
                    "it repeats, and that would not come back",
                ));
            }
            if super::super::clean(&item.title().to_string()) != super::super::clean(title) {
                return Err(CalendarError::NotFound);
            }
            let due = match item.dueDateComponents() {
                None => None,
                Some(parts) => {
                    let defined =
                        |v: isize| (v != objc2_foundation::NSDateComponentUndefined).then_some(v);
                    // A due date without its day would come back without one: not deleted then.
                    let day = (|| {
                        Some(Day {
                            year: defined(parts.year())? as i32,
                            month: defined(parts.month())? as u32,
                            day: defined(parts.day())? as u32,
                        })
                    })()
                    .ok_or(CalendarError::Invalid(
                        "its due date would not come back as it was",
                    ))?;
                    let time = defined(parts.hour())
                        .map(|h| (h as u32, defined(parts.minute()).unwrap_or(0) as u32));
                    Some((day, time))
                }
            };
            let kept = super::DeletedReminder {
                reminder: NewReminder {
                    title: super::super::clean(&item.title().to_string()),
                    list: item.calendar().map(|c| c.calendarIdentifier().to_string()),
                    due,
                },
                notes: item
                    .notes()
                    .map(|n| n.to_string())
                    .filter(|n| !n.trim().is_empty()),
                done: item.isCompleted(),
            };
            kept.reminder
                .check()
                .map_err(|_| CalendarError::Invalid("it would not come back as it was"))?;
            store
                .removeReminder_commit_error(&item, true)
                .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))?;
            Ok(kept)
        }
    }

    pub(super) fn restore_event(
        kept: &super::DeletedEvent,
        closed: &[String],
    ) -> Result<Created, CalendarError> {
        create_event_with(&kept.event, kept.notes.as_deref(), closed)
    }

    pub(super) fn restore_reminder(
        kept: &super::DeletedReminder,
        closed: &[String],
    ) -> Result<Created, CalendarError> {
        create_reminder_with(&kept.reminder, kept.notes.as_deref(), kept.done, closed)
    }

    pub(super) fn remove_event(id: &str) -> Result<(), CalendarError> {
        let store = store(Kind::Events)?;
        let item = event(&store, id)?;
        // SAFETY: plain EventKit calls on objects this thread holds. The identifier gives a series' first occurrence:
        // "future events" from there is the whole series.
        unsafe { store.removeEvent_span_error(&item, EKSpan::FutureEvents) }
            .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))
    }

    pub(super) fn remove_reminder(id: &str) -> Result<(), CalendarError> {
        let store = store(Kind::Reminders)?;
        let item = reminder(&store, id)?;
        // SAFETY: plain EventKit calls on objects this thread holds.
        unsafe { store.removeReminder_commit_error(&item, true) }
            .map_err(|err| CalendarError::Refused(err.localizedDescription().to_string()))
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::super::{CalendarError, Day};
    use super::{Created, NewEvent, NewReminder, Snapshot};

    pub(super) fn create_event(_: &NewEvent, _: &[String]) -> Result<Created, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn delete_event(
        _: &str,
        _: super::Scope,
        _: &str,
        _: &[String],
    ) -> Result<super::DeletedEvent, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn delete_reminder(
        _: &str,
        _: &str,
        _: &[String],
    ) -> Result<super::DeletedReminder, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn restore_event(
        _: &super::DeletedEvent,
        _: &[String],
    ) -> Result<Created, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn restore_reminder(
        _: &super::DeletedReminder,
        _: &[String],
    ) -> Result<Created, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn create_reminder(_: &NewReminder, _: &[String]) -> Result<Created, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn complete_reminder(_: &str, _: bool, _: &[String]) -> Result<bool, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn reminder_done(_: &str) -> Result<bool, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn move_event(
        _: &str,
        _: Day,
        _: Option<(u32, u32)>,
        _: Option<u32>,
        _: Option<&str>,
        _: &[String],
    ) -> Result<Snapshot, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn event_snapshot(_: &str) -> Result<Snapshot, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn remove_event(_: &str) -> Result<(), CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn default_collection(
        _: super::Kind,
    ) -> Result<Option<super::Collection>, CalendarError> {
        Err(CalendarError::Unsupported)
    }
    pub(super) fn remove_reminder(_: &str) -> Result<(), CalendarError> {
        Err(CalendarError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Day = Day {
        year: 2026,
        month: 11,
        day: 14,
    };

    fn event() -> NewEvent {
        NewEvent {
            title: "Zahnarzt".into(),
            calendar: None,
            day: DAY,
            time: Some((10, 0)),
            minutes: 60,
            location: None,
            alert_minutes: None,
            repeat: None,
        }
    }

    #[test]
    fn what_a_delete_keeps_comes_back_from_its_json() {
        let kept = DeletedEvent {
            event: NewEvent {
                repeat: Some(Repeat {
                    frequency: Frequency::Weekly,
                    interval: 2,
                    end: RepeatEnd::Until(DAY),
                }),
                time: None,
                ..event()
            },
            notes: Some("Karte".into()),
        };
        let json = serde_json::to_string(&kept).unwrap();
        assert_eq!(serde_json::from_str::<DeletedEvent>(&json).unwrap(), kept);
        let reminder = DeletedReminder {
            reminder: NewReminder {
                title: "Milch".into(),
                list: Some("l".into()),
                due: Some((DAY, Some((9, 0)))),
            },
            notes: None,
            done: true,
        };
        let json = serde_json::to_string(&reminder).unwrap();
        assert_eq!(
            serde_json::from_str::<DeletedReminder>(&json).unwrap(),
            reminder
        );
        // What could not come back is refused before anything is deleted.
        let long = NewEvent {
            minutes: 21 * 24 * 60,
            ..event()
        };
        assert!(long.check().is_err());
    }

    #[test]
    fn what_eventkit_would_refuse_or_crash_on_is_refused_first() {
        assert!(event().check().is_ok());
        let refused = |event: NewEvent| event.check().is_err();
        assert!(refused(NewEvent {
            title: " ".into(),
            ..event()
        }));
        assert!(refused(NewEvent {
            title: "a\u{202e}b".into(),
            ..event()
        }));
        assert!(refused(NewEvent {
            time: Some((24, 0)),
            ..event()
        }));
        assert!(refused(NewEvent {
            minutes: 0,
            ..event()
        }));
        assert!(refused(NewEvent {
            minutes: MAX_MINUTES + 1,
            ..event()
        }));
        assert!(refused(NewEvent {
            day: Day { month: 13, ..DAY },
            ..event()
        }));
        let repeat = |interval, end| Repeat {
            frequency: Frequency::Weekly,
            interval,
            end,
        };
        assert!(refused(NewEvent {
            repeat: Some(repeat(0, RepeatEnd::Never)),
            ..event()
        }));
        assert!(refused(NewEvent {
            repeat: Some(repeat(1, RepeatEnd::Count(0))),
            ..event()
        }));
        assert!(refused(NewEvent {
            repeat: Some(repeat(1, RepeatEnd::Until(Day { day: 1, ..DAY }))),
            ..event()
        }));
        assert!(
            NewEvent {
                repeat: Some(repeat(2, RepeatEnd::Count(10))),
                ..event()
            }
            .check()
            .is_ok()
        );
        let reminder = NewReminder {
            title: "Milch".into(),
            list: None,
            due: Some((DAY, Some((7, 60)))),
        };
        assert!(reminder.check().is_err());
        assert!(move_event("x", DAY, Some((25, 0)), None, None, &[]).is_err());
    }
}
