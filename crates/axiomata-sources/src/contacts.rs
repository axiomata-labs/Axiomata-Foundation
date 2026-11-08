//! The owner's contacts (§8l Q4/Q15): found by name, with their addresses and
//! numbers and what each is called ("privat", "Arbeit"), and their birthday — through the Contacts framework, read
//! only. Contacts the owner closed are not among what is found; a caller writes only to an address found here (or to
//! the sender of a mail it answers), never to one a text named.

use crate::calendar::{Status, invisible};

/// The most contacts one search returns.
pub const MAX_FOUND: usize = 20;
/// The longest name searched for.
const MAX_NAME_CHARS: usize = 100;
/// The longest text kept of a field.
const MAX_FIELD_CHARS: usize = 200;

/// A contact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    /// The Contacts framework's identifier: what a closed one is kept as.
    pub id: String,
    /// Given and family name, as the owner wrote them.
    pub name: String,
    /// Mail addresses with what each is called, lower case.
    pub emails: Vec<(String, String)>,
    /// Phone numbers with what each is called.
    pub phones: Vec<(String, String)>,
    /// The birthday: year when known, month, day.
    pub birthday: Option<(Option<i32>, u32, u32)>,
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContactsError {
    /// The owner has not let the app read the contacts.
    AccessDenied,
    /// A name that is none.
    Invalid(&'static str),
    /// Off macOS.
    Unsupported,
    /// The framework failed; its own message.
    Failed(String),
}

impl std::fmt::Display for ContactsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContactsError::AccessDenied => write!(
                f,
                "no access to the contacts — allow it in System Settings → Privacy & Security → Contacts"
            ),
            ContactsError::Invalid(why) => write!(f, "{why}"),
            ContactsError::Unsupported => write!(f, "contacts are read on macOS only"),
            ContactsError::Failed(why) => write!(f, "the contacts could not be read: {why}"),
        }
    }
}

impl std::error::Error for ContactsError {}

/// Whether this app may read the contacts.
pub fn status() -> Status {
    imp::status()
}

/// Asks the owner for the contacts when nobody has yet — the system's dialog; blocks until the owner answered (up to
/// two minutes).
///
/// # Errors
///
/// [`ContactsError::AccessDenied`] when the owner says no, or said so before.
pub fn ask() -> Result<(), ContactsError> {
    imp::ask()
}

/// The contacts whose name holds `name`, at most [`MAX_FOUND`], the `closed` ones left out.
///
/// # Errors
///
/// [`ContactsError::Invalid`] for an empty or too long name, [`ContactsError::AccessDenied`] without access.
pub fn find(name: &str, closed: &[String]) -> Result<Vec<Contact>, ContactsError> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(|c| c.is_control() || invisible(c))
    {
        return Err(ContactsError::Invalid("say a name"));
    }
    let mut found = imp::find(name)?;
    found.retain(|contact| !closed.contains(&contact.id));
    found.truncate(MAX_FOUND);
    Ok(found)
}

/// The contact `id` as it is now; `None` when it is gone or closed.
///
/// # Errors
///
/// [`ContactsError::AccessDenied`] without access.
pub fn get(id: &str, closed: &[String]) -> Result<Option<Contact>, ContactsError> {
    if closed.iter().any(|known| known == id) || id.trim().is_empty() {
        return Ok(None);
    }
    imp::get(id)
}

/// The contact a number or mail address (as Messages keeps it) belongs to; `None` when it is nobody's, or a closed
/// one's.
///
/// # Errors
///
/// [`ContactsError::Invalid`] for no number or address, [`ContactsError::AccessDenied`] without access.
pub fn of_handle(handle: &str, closed: &[String]) -> Result<Option<Contact>, ContactsError> {
    let handle = handle.trim();
    if handle.is_empty()
        || handle.chars().count() > MAX_FIELD_CHARS
        || handle.chars().any(|c| c.is_control() || invisible(c))
    {
        return Err(ContactsError::Invalid("no number or address"));
    }
    Ok(imp::of_handle(handle)?.filter(|contact| !closed.contains(&contact.id)))
}

/// `text` on one line, without what shows nothing, cut.
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
        .take(MAX_FIELD_CHARS)
        .collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, ProtocolObject};
    use objc2_contacts::{
        CNAuthorizationStatus, CNContact, CNContactBirthdayKey, CNContactEmailAddressesKey,
        CNContactFamilyNameKey, CNContactGivenNameKey, CNContactIdentifierKey,
        CNContactPhoneNumbersKey, CNContactStore, CNEntityType, CNKeyDescriptor, CNLabeledValue,
        CNPhoneNumber,
    };
    use objc2_foundation::{NSArray, NSDateComponentUndefined, NSError, NSString};

    use super::{Contact, ContactsError, Status, line};

    /// How long the owner gets to answer the system's dialog.
    const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

    pub(super) fn status() -> Status {
        // SAFETY: a class method without arguments that hold state.
        match unsafe { CNContactStore::authorizationStatusForEntityType(CNEntityType::Contacts) } {
            CNAuthorizationStatus::Authorized => Status::Granted,
            CNAuthorizationStatus::NotDetermined => Status::NotAsked,
            _ => Status::Denied,
        }
    }

    pub(super) fn ask() -> Result<(), ContactsError> {
        match status() {
            Status::Granted => return Ok(()),
            Status::Denied => return Err(ContactsError::AccessDenied),
            Status::NotAsked => {}
        }
        let (tx, rx) = mpsc::channel();
        let answer = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            let _ = tx.send(granted.as_bool());
        });
        // SAFETY: a plain constructor; the store lives until the answer came, the block is copied by the framework.
        let store = unsafe { CNContactStore::new() };
        unsafe {
            store.requestAccessForEntityType_completionHandler(CNEntityType::Contacts, &answer)
        };
        let granted = rx.recv_timeout(ANSWER_TIMEOUT);
        drop(store);
        match granted {
            Ok(true) => Ok(()),
            _ => Err(ContactsError::AccessDenied),
        }
    }

    fn store() -> Result<Retained<CNContactStore>, ContactsError> {
        if status() != Status::Granted {
            return Err(ContactsError::AccessDenied);
        }
        // SAFETY: a plain constructor.
        Ok(unsafe { CNContactStore::new() })
    }

    /// The fields read: nothing else of a contact is fetched.
    fn keys() -> Retained<NSArray<ProtocolObject<dyn CNKeyDescriptor>>> {
        // SAFETY: static strings of the framework's.
        let keys: [&NSString; 6] = unsafe {
            [
                CNContactIdentifierKey,
                CNContactGivenNameKey,
                CNContactFamilyNameKey,
                CNContactEmailAddressesKey,
                CNContactPhoneNumbersKey,
                CNContactBirthdayKey,
            ]
        };
        let keys: Vec<&ProtocolObject<dyn CNKeyDescriptor>> = keys
            .iter()
            .map(|key| ProtocolObject::from_ref(*key))
            .collect();
        NSArray::from_slice(&keys)
    }

    /// What a label is called in the owner's language ("privat", "Arbeit").
    fn label(raw: Option<Retained<NSString>>) -> String {
        // SAFETY: a class method on a string this thread holds.
        raw.map(|raw| {
            line(&unsafe { CNLabeledValue::<NSString>::localizedStringForLabel(&raw) }.to_string())
        })
        .unwrap_or_default()
    }

    fn plain(contact: &CNContact) -> Contact {
        // SAFETY: reading the fetched properties of an object this thread holds.
        unsafe {
            let name = line(&format!("{} {}", contact.givenName(), contact.familyName()));
            let emails = contact
                .emailAddresses()
                .iter()
                .map(|value| {
                    (
                        label(value.label()),
                        line(&value.value().to_string()).to_lowercase(),
                    )
                })
                .filter(|(_, address)| address.contains('@'))
                .collect();
            let phones = contact
                .phoneNumbers()
                .iter()
                .map(|value| {
                    (
                        label(value.label()),
                        line(&value.value().stringValue().to_string()),
                    )
                })
                .filter(|(_, number)| !number.is_empty())
                .collect();
            let birthday = contact.birthday().and_then(|parts| {
                let defined = |v: isize| (v != NSDateComponentUndefined).then_some(v);
                Some((
                    defined(parts.year()).map(|y| y as i32),
                    defined(parts.month())? as u32,
                    defined(parts.day())? as u32,
                ))
            });
            Contact {
                id: contact.identifier().to_string(),
                name,
                emails,
                phones,
                birthday,
            }
        }
    }

    pub(super) fn find(name: &str) -> Result<Vec<Contact>, ContactsError> {
        let store = store()?;
        // SAFETY: the predicate and the keys live across the call; the contacts are read on this thread.
        unsafe {
            let predicate = CNContact::predicateForContactsMatchingName(&NSString::from_str(name));
            let found = store
                .unifiedContactsMatchingPredicate_keysToFetch_error(&predicate, &keys())
                .map_err(|err| ContactsError::Failed(err.localizedDescription().to_string()))?;
            Ok(found.iter().map(|contact| plain(&contact)).collect())
        }
    }

    pub(super) fn of_handle(handle: &str) -> Result<Option<Contact>, ContactsError> {
        let store = store()?;
        // SAFETY: as in `find`; the number object lives across the call.
        unsafe {
            let predicate = if handle.contains('@') {
                CNContact::predicateForContactsMatchingEmailAddress(&NSString::from_str(handle))
            } else {
                let Some(number) =
                    CNPhoneNumber::phoneNumberWithStringValue(&NSString::from_str(handle))
                else {
                    return Ok(None);
                };
                CNContact::predicateForContactsMatchingPhoneNumber(&number)
            };
            let found = store
                .unifiedContactsMatchingPredicate_keysToFetch_error(&predicate, &keys())
                .map_err(|err| ContactsError::Failed(err.localizedDescription().to_string()))?;
            Ok(found.iter().next().map(|contact| plain(&contact)))
        }
    }

    pub(super) fn get(id: &str) -> Result<Option<Contact>, ContactsError> {
        let store = store()?;
        // SAFETY: as in `find`.
        unsafe {
            Ok(store
                .unifiedContactWithIdentifier_keysToFetch_error(&NSString::from_str(id), &keys())
                .ok()
                .map(|contact| plain(&contact)))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Contact, ContactsError, Status};

    pub(super) fn status() -> Status {
        Status::Denied
    }
    pub(super) fn ask() -> Result<(), ContactsError> {
        Err(ContactsError::Unsupported)
    }
    pub(super) fn find(_: &str) -> Result<Vec<Contact>, ContactsError> {
        Err(ContactsError::Unsupported)
    }
    pub(super) fn get(_: &str) -> Result<Option<Contact>, ContactsError> {
        Err(ContactsError::Unsupported)
    }
    pub(super) fn of_handle(_: &str) -> Result<Option<Contact>, ContactsError> {
        Err(ContactsError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_is_none_is_not_searched_and_a_closed_contact_is_not_got() {
        for name in ["", "  ", "a\u{202e}b", &"n".repeat(101)] {
            assert_eq!(
                find(name, &[]),
                Err(ContactsError::Invalid("say a name")),
                "{name:?}"
            );
        }
        assert_eq!(get("x", &["x".into()]), Ok(None));
        assert_eq!(
            of_handle(" ", &[]),
            Err(ContactsError::Invalid("no number or address"))
        );
    }

    #[test]
    #[ignore = "reads this Mac's contacts; run by hand — prints only counts"]
    fn on_this_mac() {
        let found = find("a", &[]).unwrap();
        eprintln!("{} contacts hold \"a\"", found.len());
    }
}
