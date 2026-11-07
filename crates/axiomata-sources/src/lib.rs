//! What an app of the Axiomata family may read on the Mac (Lirvel `docs/plans/companion.md` §8j): the owner's
//! calendars and reminders, files, mail. Every source asks the [`Fence`] first; the fence's [`baseline`] holds for
//! every app and cannot be lifted, the owner's own list comes on top. Nothing here knows of models: what an app tells
//! one, and how it frames it, is the app's.

pub mod calendar;
pub mod fence;

pub use fence::{Baseline, Fence, baseline};
