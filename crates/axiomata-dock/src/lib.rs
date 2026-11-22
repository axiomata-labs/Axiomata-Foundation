//! A dock layout (Axiomata-OS `docs/plans/rust-core.md`, E2): the tree of splits and tab groups a studio arranges its
//! panes in, every operation a view performs on it, where a dragged pane lands, and the form it is stored in. No UI:
//! the Axiomata studio draws it in a webview, Lohe on Schimmer, both from this one model.
//!
//! The shape is a tree of [`Split`] and [`TabGroup`] nodes. Five decisions carry it:
//!
//! * **An operation changes nothing it cannot finish.** Each one first checks what it needs (the tab, the target) and
//!   says with its `bool` whether the layout changed; a call on a tab that is gone — a click that came too late — is
//!   no error, just no change.
//! * **Sizes are fractions of their split, summing to 1**, never pixels: a window resized, or a 21:9 monitor after a
//!   16:9 one, needs no layout rewrite. No pane shrinks under [`MIN_PANE_FRACTION`] — below it a pane is a sliver with
//!   no reachable content and no grabbable divider.
//! * **The active tab is an id, not an index**: an index points at another tab the moment a neighbour closes.
//! * **The tree is normalised after every operation**: empty groups disappear, a split left with one child collapses
//!   into it, and a split nested in a split of the same direction is flattened into its parent — repeated docking
//!   would otherwise grow a deeper and deeper tree that draws the same.
//! * **The root always exists.** A dock with everything closed is one empty group, so there is still something to
//!   drop a pane onto.
//!
//! A tab's `kind` and `config` are opaque: this is a tree, not a registry of panes, and a layout stored by a newer app
//! must not be emptied by an older one that has never heard of its panes.

#![forbid(unsafe_code)]

mod geometry;
mod ids;
mod keys;
mod layout;
mod sizes;
mod stored;

pub use geometry::{
    DOCK_EDGE_RATIO, DRAG_THRESHOLD_PX, DropAt, DropNode, GroupGeometry, ROOT_EDGE_RATIO, Rect,
    crossed_drag_threshold, dock_side_at, drop_target, split_fraction_at, tab_drop_index,
};
pub use ids::{NodeId, TabId};
pub use keys::{Cycle, SplitSide};
pub use layout::{
    DividerRange, DockSide, DockTarget, Layout, Node, PaneTab, Split, SplitDir, TabGroup,
    TabLocation,
};
pub use sizes::MIN_PANE_FRACTION;
pub use stored::LAYOUT_VERSION;
