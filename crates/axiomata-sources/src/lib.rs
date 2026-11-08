//! What an app of the Axiomata family may read on the Mac (§8j): the owner's calendars and reminders, files, mail,
//! messages and photos. Every source asks the [`Fence`] first; the fence's [`baseline`] holds for every app and cannot
//! be lifted, the owner's own list comes on top. Nothing here knows of models: what an app tells one, and how it
//! frames it, is the app's.

pub mod apps;
pub mod calendar;
pub mod contacts;
pub mod fence;
pub mod files;
pub mod imessage;
pub mod mail;
pub mod photos;
pub mod shortcuts;

pub use fence::{Baseline, Fence, baseline};
