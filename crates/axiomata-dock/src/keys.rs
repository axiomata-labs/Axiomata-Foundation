//! The tab keys (⌃Tab, ⌘1–9, ⌘\) on a dock. The dock keeps no "focused group": the caller names the tab the owner last
//! worked in, and the group holding it counts as focused — without one, or with a stale id, the first group does.

use serde::{Deserialize, Serialize};

use crate::ids::TabId;
use crate::layout::{DockSide, DockTarget, Layout, TabGroup};

/// Which way ⌃Tab goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Cycle {
    /// ⌃Tab.
    Next,
    /// ⇧⌃Tab.
    Previous,
}

/// Where ⌘\ puts the visible tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SplitSide {
    /// ⌘\: beside it.
    Right,
    /// ⇧⌘\: below it.
    Bottom,
}

impl Layout {
    /// The group holding `focus`, else the first group.
    #[must_use]
    pub fn focused_group(&self, focus: Option<&TabId>) -> Option<&TabGroup> {
        focus
            .and_then(|tab| self.find_tab(tab))
            .map(|hit| hit.group)
            .or_else(|| self.groups().into_iter().next())
    }

    /// ⌃Tab / ⇧⌃Tab: the next or previous tab of the focused group, wrapping round.
    pub fn cycle_tab(&mut self, focus: Option<&TabId>, way: Cycle) -> bool {
        let Some(group) = self.focused_group(focus) else {
            return false;
        };
        let count = group.tabs().len();
        if count == 0 {
            return false;
        }
        // A group without a known active tab counts as standing before its first: ⌃Tab then goes to the first tab.
        let at = group
            .tabs()
            .iter()
            .position(|tab| Some(&tab.id) == group.active());
        let next = match (way, at) {
            (Cycle::Next, Some(at)) => (at + 1) % count,
            (Cycle::Next, None) => 0,
            (Cycle::Previous, Some(at)) => (at + count - 1) % count,
            (Cycle::Previous, None) => (count + count - 2) % count,
        };
        let id = group.tabs()[next].id.clone();
        self.activate_tab(&id)
    }

    /// ⌘1–⌘9: the `n`-th tab (1-based) of the focused group; ⌘9 the last, as in browsers.
    pub fn nth_tab(&mut self, focus: Option<&TabId>, n: usize) -> bool {
        let Some(tabs) = self.focused_group(focus).map(TabGroup::tabs) else {
            return false;
        };
        let tab = if n >= 9 {
            tabs.last()
        } else {
            n.checked_sub(1).and_then(|i| tabs.get(i))
        };
        let Some(id) = tab.map(|tab| tab.id.clone()) else {
            return false;
        };
        self.activate_tab(&id)
    }

    /// ⌘\ / ⇧⌘\: the focused group's visible tab moves into a new group beside or below it. A lone tab stays.
    pub fn split_active(&mut self, focus: Option<&TabId>, side: SplitSide) -> bool {
        let Some(group) = self.focused_group(focus) else {
            return false;
        };
        let Some(active) = group.active().cloned().filter(|_| group.tabs().len() >= 2) else {
            return false;
        };
        let side = match side {
            SplitSide::Right => DockSide::Right,
            SplitSide::Bottom => DockSide::Bottom,
        };
        let target = DockTarget::new(group.id().clone(), side);
        self.move_tab(&active, &target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::single;

    fn active(layout: &Layout) -> String {
        layout.groups()[0].active().unwrap().to_string()
    }

    #[test]
    fn a_tab_cycles_forwards_and_backwards_wrapping() {
        let mut layout = single(&["a", "b", "c"]);
        for expected in ["b", "c", "a"] {
            assert!(layout.cycle_tab(None, Cycle::Next));
            assert_eq!(active(&layout), expected);
        }
        layout.cycle_tab(None, Cycle::Previous);
        assert_eq!(active(&layout), "c");
    }

    #[test]
    fn the_nth_tab_is_picked_and_nine_is_the_last() {
        let mut layout = single(&["a", "b", "c"]);
        assert!(layout.nth_tab(None, 2));
        assert_eq!(active(&layout), "b");
        layout.nth_tab(None, 9);
        assert_eq!(active(&layout), "c");
        let before = layout.clone();
        assert!(!layout.nth_tab(None, 5));
        assert!(!layout.nth_tab(None, 0));
        assert_eq!(layout, before);
    }

    #[test]
    fn a_stale_tab_falls_back_to_the_first_group() {
        let layout = single(&["a", "b", "c"]);
        assert_eq!(
            layout
                .focused_group(Some(&"gone".into()))
                .unwrap()
                .tabs()
                .len(),
            3
        );
    }

    #[test]
    fn the_focus_is_the_group_holding_the_tab_named() {
        let mut layout = single(&["a", "b"]);
        let root = layout.root().id().clone();
        layout.add_tab(
            crate::layout::tests::tab("c"),
            &DockTarget::new(root, DockSide::Right),
        );
        layout.add_tab(
            crate::layout::tests::tab("d"),
            &DockTarget::tab_at(layout.groups()[1].id().clone(), 0),
        );
        assert!(layout.cycle_tab(Some(&"c".into()), Cycle::Next));
        assert_eq!(layout.groups()[1].active().unwrap().as_str(), "c");
        assert_eq!(active(&layout), "a", "the other group is untouched");
        assert!(layout.nth_tab(Some(&"c".into()), 1));
        assert_eq!(layout.groups()[1].active().unwrap().as_str(), "d");
    }

    #[test]
    fn the_visible_tab_splits_off_below_too() {
        let mut layout = single(&["a", "b"]);
        layout.activate_tab(&"b".into());
        assert!(layout.split_active(None, SplitSide::Bottom));
        assert_eq!(crate::layout::tests::shape(layout.root()), "col([*a] [*b])");
    }

    #[test]
    fn the_visible_tab_splits_off_but_a_lone_one_stays() {
        let mut layout = single(&["a", "b", "c"]);
        assert!(layout.split_active(Some(&"a".into()), SplitSide::Right));
        assert_eq!(layout.groups().len(), 2);
        let mut lone = single(&["x"]);
        assert!(!lone.split_active(None, SplitSide::Right));
    }
}
