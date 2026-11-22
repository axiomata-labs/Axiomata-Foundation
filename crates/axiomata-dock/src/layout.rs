//! The tree and every operation a view performs on it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{NodeId, TabId};
use crate::sizes::{self, MIN_PANE_FRACTION};

/// The axis a split divides along: `row` = side by side, `col` = stacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SplitDir {
    /// Side by side.
    Row,
    /// Stacked.
    Col,
}

/// One tab in a group — what the view draws inside a pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PaneTab {
    /// Unique across the whole tree; given by whoever opens the pane.
    pub id: TabId,
    /// Which pane draws it. Opaque here — a tree, not a registry of panes.
    pub kind: String,
    /// The label on the tab.
    pub title: String,
    /// The pane's own state (a file, a module instance …): carried, never read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>", optional))]
    pub config: Option<Map<String, Value>>,
    /// A pane a mode is built from: it has no close button and [`Layout::close_tab`] leaves it where it is. It can still
    /// be moved and resized; [`Layout::force_close_tab`] is for rebuilding the arrangement.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub pinned: bool,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's `skip_serializing_if` passes a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

impl PaneTab {
    /// A tab of `kind` titled `title`, with no config and not pinned.
    #[must_use]
    pub fn new(id: impl Into<TabId>, kind: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            config: None,
            pinned: false,
        }
    }
}

/// A stack of tabs, one of them visible. The only node that holds content.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TabGroup {
    pub(crate) id: NodeId,
    pub(crate) tabs: Vec<PaneTab>,
    /// The visible tab; `None` only while the group is empty, which only the root can be.
    pub(crate) active: Option<TabId>,
}

impl TabGroup {
    /// The group's id.
    #[must_use]
    pub fn id(&self) -> &NodeId {
        &self.id
    }

    /// Its tabs, in order.
    #[must_use]
    pub fn tabs(&self) -> &[PaneTab] {
        &self.tabs
    }

    /// The visible tab's id; `None` only for the empty root.
    #[must_use]
    pub fn active(&self) -> Option<&TabId> {
        self.active.as_ref()
    }

    pub(crate) fn empty(id: NodeId) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active: None,
        }
    }

    /// The group with `tab` inserted at `index` among its tabs (last when `None`, clamped), and made active.
    fn insert(&mut self, tab: PaneTab, index: Option<usize>) {
        let at = index.map_or(self.tabs.len(), |index| index.min(self.tabs.len()));
        self.active = Some(tab.id.clone());
        self.tabs.insert(at, tab);
    }

    /// Takes `tab_id` out; the tab that slides into its place becomes active, or the new last one.
    fn remove(&mut self, tab_id: &TabId) -> Option<PaneTab> {
        let index = self.tabs.iter().position(|tab| &tab.id == tab_id)?;
        let tab = self.tabs.remove(index);
        if self.active.as_ref() == Some(tab_id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id.clone());
        }
        Some(tab)
    }
}

/// A row or column of children, each taking its fraction of the split.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Split {
    pub(crate) id: NodeId,
    pub(crate) dir: SplitDir,
    /// At least two: a one-child split collapses.
    pub(crate) children: Vec<Node>,
    /// One fraction per child, in the same order, summing to 1.
    pub(crate) sizes: Vec<f64>,
}

impl Split {
    /// The split's id.
    #[must_use]
    pub fn id(&self) -> &NodeId {
        &self.id
    }

    /// Its axis.
    #[must_use]
    pub fn dir(&self) -> SplitDir {
        self.dir
    }

    /// Its children, at least two.
    #[must_use]
    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// One fraction per child, summing to 1.
    #[must_use]
    pub fn sizes(&self) -> &[f64] {
        &self.sizes
    }
}

/// A node of the tree.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(rename = "LayoutNode"))]
#[serde(tag = "type")]
pub enum Node {
    /// A group of tabs.
    #[serde(rename = "tabs")]
    Tabs(TabGroup),
    /// A row or column of nodes.
    #[serde(rename = "split")]
    Split(Split),
}

impl Node {
    /// The node's id.
    #[must_use]
    pub fn id(&self) -> &NodeId {
        match self {
            Self::Tabs(group) => &group.id,
            Self::Split(split) => &split.id,
        }
    }
}

/// Where a dragged pane lands: an edge of the target splits it, the centre joins its tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum DockSide {
    /// Into the group, as a tab.
    Center,
    /// A new group to the left.
    Left,
    /// A new group to the right.
    Right,
    /// A new group above.
    Top,
    /// A new group below.
    Bottom,
}

/// A place in the layout to put a tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DockTarget {
    /// The node dropped onto. For [`DockSide::Center`] a group; for an edge any node, the root included — that is how a
    /// pane docks against the outer edge of the whole view.
    pub node_id: NodeId,
    /// Which part of it.
    pub side: DockSide,
    /// Centre only: the slot among the group's *other* tabs, counted with the moved tab left out; last when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub index: Option<usize>,
}

impl DockTarget {
    /// The edge or centre `side` of `node_id`.
    #[must_use]
    pub fn new(node_id: NodeId, side: DockSide) -> Self {
        Self {
            node_id,
            side,
            index: None,
        }
    }

    /// Into group `node_id`, at `index` among its other tabs.
    #[must_use]
    pub fn tab_at(node_id: NodeId, index: usize) -> Self {
        Self {
            node_id,
            side: DockSide::Center,
            index: Some(index),
        }
    }
}

/// Where a tab sits, as [`Layout::find_tab`] reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabLocation<'a> {
    /// The group holding it.
    pub group: &'a TabGroup,
    /// The tab.
    pub tab: &'a PaneTab,
    /// Its position in the group.
    pub index: usize,
}

/// How far a divider may move, for a view that moves it on its own while it is dragged and says where it ended up only
/// when it is let go ([`Layout::resize_split`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DividerRange {
    /// What the children before the divider's pair take of the split.
    pub before: f64,
    /// The least the pointer may stand at, as a fraction of the split.
    pub min: f64,
    /// The most the pointer may stand at, as a fraction of the split.
    pub max: f64,
}

impl DividerRange {
    /// The pointer's position (a fraction of the split) clamped into the range.
    #[must_use]
    pub fn clamp(&self, pointer: f64) -> f64 {
        // Not `f64::clamp`, which panics on a range someone built the wrong way round.
        if pointer.is_finite() {
            pointer.max(self.min).min(self.max)
        } else {
            self.min
        }
    }

    /// The `fraction` [`Layout::resize_split`] takes for a pointer at `pointer`: the share of the child before the
    /// divider, within its pair.
    #[must_use]
    pub fn fraction(&self, pointer: f64) -> f64 {
        self.clamp(pointer) - self.before
    }
}

/// A whole dock layout. Its root always exists — see the crate's docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Layout {
    pub(crate) root: Node,
}

impl Default for Layout {
    fn default() -> Self {
        Self::empty()
    }
}

impl Layout {
    /// A layout holding nothing: one empty group, ready to be dropped onto.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            root: Node::Tabs(TabGroup::empty(NodeId::fresh())),
        }
    }

    /// A layout holding these tabs in one group, the first visible. A tab whose id came before is left out.
    #[must_use]
    pub fn single_group(tabs: Vec<PaneTab>) -> Self {
        let mut layout = Self::empty();
        let id = layout.root.id().clone();
        for tab in tabs {
            layout.add_tab(tab, &DockTarget::new(id.clone(), DockSide::Center));
        }
        if let Node::Tabs(group) = &mut layout.root {
            group.active = group.tabs.first().map(|tab| tab.id.clone());
        }
        layout
    }

    /// The root node.
    #[must_use]
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Every group, in reading order (left to right, top to bottom).
    #[must_use]
    pub fn groups(&self) -> Vec<&TabGroup> {
        let mut out = Vec::new();
        collect_groups(&self.root, &mut out);
        out
    }

    /// Every tab, in reading order.
    pub fn tabs(&self) -> impl Iterator<Item = &PaneTab> {
        self.groups()
            .into_iter()
            .flat_map(|group| group.tabs.iter())
    }

    /// Where a tab sits, if a tab has that id.
    #[must_use]
    pub fn find_tab(&self, tab_id: &TabId) -> Option<TabLocation<'_>> {
        self.groups().into_iter().find_map(|group| {
            let index = group.tabs.iter().position(|tab| &tab.id == tab_id)?;
            Some(TabLocation {
                group,
                tab: &group.tabs[index],
                index,
            })
        })
    }

    /// The node with that id: depth first, the first match — the same order every write takes, so a lookup and the
    /// write after it agree on the node even in a stored layout that reused an id.
    #[must_use]
    pub fn find_node(&self, node_id: &NodeId) -> Option<&Node> {
        find_node(&self.root, node_id)
    }

    /// Adds `tab` at `target`, made active. No change when its id is already in the tree (every operation addresses a
    /// tab by id), when the target does not exist, or when a centre drop names a split.
    pub fn add_tab(&mut self, tab: PaneTab, target: &DockTarget) -> bool {
        if self.find_tab(&tab.id).is_some() || !self.can_dock_at(target) {
            return false;
        }
        self.edit(|root| insert_at(root, tab, target))
    }

    /// Moves a tab that is in the layout to `target`.
    ///
    /// A group's only tab dropped back onto that same group changes nothing, on every side: that is what the owner
    /// did. Going the general way would empty the group, wrap it in a split and collapse it again — the same picture
    /// with a new group id, which the view keys its drag state off.
    pub fn move_tab(&mut self, tab_id: &TabId, target: &DockTarget) -> bool {
        let Some(hit) = self.find_tab(tab_id) else {
            return false;
        };
        if target.node_id == hit.group.id && hit.group.tabs.len() == 1 {
            return false;
        }
        if !self.can_dock_at(target) {
            return false;
        }
        let source = hit.group.id.clone();
        self.edit(|root| {
            // Not normalised in between: the source group may now be empty and still be the node docked onto.
            let Some(tab) = group_mut(root, &source).and_then(|group| group.remove(tab_id)) else {
                return false;
            };
            insert_at(root, tab, target)
        })
    }

    /// Closes a tab, unless it is pinned. The tab that slides into its place becomes active, or the new last one; an
    /// emptied group disappears with any split left with one child; closing the last tab leaves the empty root group.
    pub fn close_tab(&mut self, tab_id: &TabId) -> bool {
        match self.find_tab(tab_id) {
            Some(hit) if !hit.tab.pinned => self.force_close_tab(tab_id),
            _ => false,
        }
    }

    /// [`Self::close_tab`] for a pinned tab too: only for rebuilding the arrangement a mode is made of.
    pub fn force_close_tab(&mut self, tab_id: &TabId) -> bool {
        let Some(group) = self.find_tab(tab_id).map(|hit| hit.group.id.clone()) else {
            return false;
        };
        self.edit(|root| {
            group_mut(root, &group)
                .and_then(|group| group.remove(tab_id))
                .is_some()
        })
    }

    /// Replaces a tab's config — the pane's own state on its way to storage.
    pub fn set_tab_config(&mut self, tab_id: &TabId, config: Map<String, Value>) -> bool {
        self.edit_tab(tab_id, |tab| tab.config = Some(config))
    }

    /// Renames a tab.
    pub fn set_tab_title(&mut self, tab_id: &TabId, title: &str) -> bool {
        self.edit_tab(tab_id, |tab| title.clone_into(&mut tab.title))
    }

    /// Pins every tab `pin` names (see [`PaneTab::pinned`]).
    pub fn pin_where(&mut self, pin: impl Fn(&PaneTab) -> bool) -> bool {
        self.edit(|root| {
            for_each_tab_mut(root, &mut |tab| {
                if pin(tab) {
                    tab.pinned = true;
                }
            });
            true
        })
    }

    /// Makes a tab the visible one of its group.
    pub fn activate_tab(&mut self, tab_id: &TabId) -> bool {
        let Some(group) = self.find_tab(tab_id).map(|hit| hit.group.id.clone()) else {
            return false;
        };
        self.edit(|root| {
            group_mut(root, &group).is_some_and(|group| {
                group.active = Some(tab_id.clone());
                true
            })
        })
    }

    /// Drags the divider after child `boundary` of a split. Only the two children touching it change, as on screen;
    /// `fraction` is the new share of the one before the divider within their pair, kept so neither falls under
    /// [`MIN_PANE_FRACTION`].
    pub fn resize_split(&mut self, split_id: &NodeId, boundary: usize, fraction: f64) -> bool {
        if !fraction.is_finite() {
            return false;
        }
        self.edit(|root| {
            let Some(split) = split_mut(root, split_id) else {
                return false;
            };
            let Some((first, second)) = pair_of(&split.sizes, boundary) else {
                return false;
            };
            let pair = first + second;
            let least = MIN_PANE_FRACTION.min(pair / 2.0);
            let share = fraction.clamp(least, pair - least);
            split.sizes[boundary] = share;
            split.sizes[boundary + 1] = pair - share;
            true
        })
    }

    /// Replaces a split's sizes wholesale — a preset, a restored drag. Input of the wrong length, negative or not
    /// finite is repaired the way a stored layout's is, not refused.
    pub fn set_split_sizes(&mut self, split_id: &NodeId, sizes: &[f64]) -> bool {
        self.edit(|root| {
            split_mut(root, split_id).is_some_and(|split| {
                split.sizes = sizes::normalize(sizes, split.children.len());
                true
            })
        })
    }

    /// How far the divider after child `boundary` of a split may move; `None` for no such split or divider.
    #[must_use]
    pub fn divider_range(&self, split_id: &NodeId, boundary: usize) -> Option<DividerRange> {
        let Some(Node::Split(split)) = self.find_node(split_id) else {
            return None;
        };
        let (first, second) = pair_of(&split.sizes, boundary)?;
        let pair = first + second;
        let least = MIN_PANE_FRACTION.min(pair / 2.0);
        // A fold from 0.0: the sum of no floats is -0.0, which a webview would carry on as -0.
        let before = split.sizes[..boundary]
            .iter()
            .fold(0.0, |sum, size| sum + size);
        Some(DividerRange {
            before,
            min: before + least,
            max: before + pair - least,
        })
    }

    /// Whether `target` is a place a tab can go: its node exists, and a centre drop names a group.
    fn can_dock_at(&self, target: &DockTarget) -> bool {
        match self.find_node(&target.node_id) {
            Some(Node::Tabs(_)) => true,
            Some(Node::Split(_)) => target.side != DockSide::Center,
            None => false,
        }
    }

    fn edit_tab(&mut self, tab_id: &TabId, change: impl FnOnce(&mut PaneTab)) -> bool {
        let mut change = Some(change);
        self.edit(|root| {
            let mut found = false;
            for_each_tab_mut(root, &mut |tab| {
                if &tab.id == tab_id
                    && let Some(change) = change.take()
                {
                    change(tab);
                    found = true;
                }
            });
            found
        })
    }

    /// Runs `change` on a copy of the tree and takes the normalised result when `change` succeeded and something
    /// differs. A tree is a few dozen nodes, so the copy costs nothing worth saving — and it makes every operation
    /// all-or-nothing.
    fn edit(&mut self, change: impl FnOnce(&mut Node) -> bool) -> bool {
        let mut root = self.root.clone();
        if !change(&mut root) {
            return false;
        }
        let root = finalize(root);
        if root == self.root {
            return false;
        }
        self.root = root;
        true
    }
}

/// The sizes of the two children either side of divider `boundary`.
fn pair_of(sizes: &[f64], boundary: usize) -> Option<(f64, f64)> {
    Some((*sizes.get(boundary)?, *sizes.get(boundary.checked_add(1)?)?))
}

fn collect_groups<'a>(node: &'a Node, out: &mut Vec<&'a TabGroup>) {
    match node {
        Node::Tabs(group) => out.push(group),
        Node::Split(split) => {
            for child in &split.children {
                collect_groups(child, out);
            }
        }
    }
}

fn find_node<'a>(node: &'a Node, node_id: &NodeId) -> Option<&'a Node> {
    if node.id() == node_id {
        return Some(node);
    }
    match node {
        Node::Split(split) => split
            .children
            .iter()
            .find_map(|child| find_node(child, node_id)),
        Node::Tabs(_) => None,
    }
}

fn node_mut<'a>(node: &'a mut Node, node_id: &NodeId) -> Option<&'a mut Node> {
    if node.id() == node_id {
        return Some(node);
    }
    match node {
        Node::Split(split) => split
            .children
            .iter_mut()
            .find_map(|child| node_mut(child, node_id)),
        Node::Tabs(_) => None,
    }
}

fn group_mut<'a>(root: &'a mut Node, node_id: &NodeId) -> Option<&'a mut TabGroup> {
    match node_mut(root, node_id)? {
        Node::Tabs(group) => Some(group),
        Node::Split(_) => None,
    }
}

fn split_mut<'a>(root: &'a mut Node, node_id: &NodeId) -> Option<&'a mut Split> {
    match node_mut(root, node_id)? {
        Node::Split(split) => Some(split),
        Node::Tabs(_) => None,
    }
}

fn for_each_tab_mut(node: &mut Node, visit: &mut impl FnMut(&mut PaneTab)) {
    match node {
        Node::Tabs(group) => group.tabs.iter_mut().for_each(visit),
        Node::Split(split) => {
            for child in &mut split.children {
                for_each_tab_mut(child, visit);
            }
        }
    }
}

/// Puts `tab` at `target`: into its group at the centre, or into a new group that splits the target node in half.
fn insert_at(root: &mut Node, tab: PaneTab, target: &DockTarget) -> bool {
    if target.side == DockSide::Center {
        return group_mut(root, &target.node_id).is_some_and(|group| {
            group.insert(tab, target.index);
            true
        });
    }
    let Some(node) = node_mut(root, &target.node_id) else {
        return false;
    };
    let dir = match target.side {
        DockSide::Left | DockSide::Right => SplitDir::Row,
        DockSide::Top | DockSide::Bottom | DockSide::Center => SplitDir::Col,
    };
    let mut fresh = TabGroup::empty(NodeId::fresh());
    fresh.insert(tab, None);
    let fresh = Node::Tabs(fresh);
    let old = std::mem::replace(node, Node::Tabs(TabGroup::empty(NodeId::fresh())));
    let children = if matches!(target.side, DockSide::Left | DockSide::Top) {
        vec![fresh, old]
    } else {
        vec![old, fresh]
    };
    *node = Node::Split(Split {
        id: NodeId::fresh(),
        dir,
        children,
        sizes: vec![0.5, 0.5],
    });
    true
}

/// Normalisation, with the root surviving as an empty group — keeping its id if it was a group, so a view holding a
/// drop target for it does not lose its handle.
pub(crate) fn finalize(root: Node) -> Node {
    let root_group = match &root {
        Node::Tabs(group) => Some(group.id.clone()),
        Node::Split(_) => None,
    };
    normalize(root)
        .unwrap_or_else(|| Node::Tabs(TabGroup::empty(root_group.unwrap_or_else(NodeId::fresh))))
}

/// Drops empty groups, collapses one-child splits, flattens a split into its parent of the same direction and fixes
/// every `active` and size on the way. `None`: the whole subtree held nothing.
pub(crate) fn normalize(node: Node) -> Option<Node> {
    match node {
        Node::Tabs(mut group) => {
            let first = group.tabs.first()?.id.clone();
            if !group
                .active
                .as_ref()
                .is_some_and(|active| group.tabs.iter().any(|tab| &tab.id == active))
            {
                group.active = Some(first);
            }
            Some(Node::Tabs(group))
        }
        Node::Split(split) => {
            let mut children = Vec::new();
            let mut shares = Vec::new();
            for (i, child) in split.children.into_iter().enumerate() {
                let Some(child) = normalize(child) else {
                    continue;
                };
                let share = split.sizes.get(i).copied().unwrap_or(f64::NAN);
                match child {
                    // A row inside a row draws the same with a deeper tree: its children move up, each keeping its part
                    // of the share its parent had.
                    Node::Split(inner) if inner.dir == split.dir => {
                        for (grandchild, size) in inner.children.into_iter().zip(inner.sizes) {
                            children.push(grandchild);
                            shares.push(share * size);
                        }
                    }
                    other => {
                        children.push(other);
                        shares.push(share);
                    }
                }
            }
            match children.len() {
                0 => None,
                1 => children.pop(),
                count => Some(Node::Split(Split {
                    id: split.id,
                    dir: split.dir,
                    sizes: sizes::normalize(&shares, count),
                    children,
                })),
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tab(id: &str) -> PaneTab {
        PaneTab::new(id, "terminal", id)
    }

    /// The tree as a compact string, so a test asserts a shape instead of a page of structs.
    pub(crate) fn shape(node: &Node) -> String {
        match node {
            Node::Tabs(group) => {
                let tabs: Vec<String> = group
                    .tabs
                    .iter()
                    .map(|tab| {
                        if group.active.as_ref() == Some(&tab.id) {
                            format!("*{}", tab.id)
                        } else {
                            tab.id.to_string()
                        }
                    })
                    .collect();
                format!("[{}]", tabs.join(" "))
            }
            Node::Split(split) => {
                let dir = match split.dir {
                    SplitDir::Row => "row",
                    SplitDir::Col => "col",
                };
                let children: Vec<String> = split.children.iter().map(shape).collect();
                format!("{dir}({})", children.join(" "))
            }
        }
    }

    pub(crate) fn single(ids: &[&str]) -> Layout {
        Layout::single_group(ids.iter().map(|id| tab(id)).collect())
    }

    fn root_id(layout: &Layout) -> NodeId {
        layout.root.id().clone()
    }

    fn group_of(layout: &Layout, tab_id: &str) -> NodeId {
        layout
            .find_tab(&TabId::from(tab_id))
            .unwrap()
            .group
            .id
            .clone()
    }

    fn sizes(layout: &Layout) -> Vec<f64> {
        match &layout.root {
            Node::Split(split) => split.sizes.clone(),
            Node::Tabs(_) => panic!("the root is no split"),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn center(id: NodeId) -> DockTarget {
        DockTarget::new(id, DockSide::Center)
    }

    /// `layout` with `tab` added at `side` of `node`.
    fn with(mut layout: Layout, tab_id: &str, node: NodeId, side: DockSide) -> Layout {
        assert!(layout.add_tab(tab(tab_id), &DockTarget::new(node, side)));
        layout
    }

    #[test]
    fn an_empty_layout_is_one_empty_group() {
        let layout = Layout::empty();
        assert_eq!(shape(&layout.root), "[]");
        assert_eq!(layout.tabs().count(), 0);
    }

    #[test]
    fn a_single_group_holds_the_tabs_in_order_with_the_first_active() {
        assert_eq!(shape(&single(&["a", "b"]).root), "[*a b]");
        assert_eq!(
            shape(&single(&["a", "a", "b"]).root),
            "[*a b]",
            "a repeated id is left out"
        );
    }

    #[test]
    fn a_tab_is_appended_or_put_at_its_slot_and_made_active() {
        let mut base = single(&["a"]);
        assert!(base.add_tab(tab("b"), &center(root_id(&base))));
        assert_eq!(shape(&base.root), "[a *b]");
        let mut base = single(&["a", "b"]);
        assert!(base.add_tab(tab("c"), &DockTarget::tab_at(root_id(&base), 1)));
        assert_eq!(shape(&base.root), "[a *c b]");
    }

    #[test]
    fn an_edge_splits_along_the_right_axis_in_the_right_order_half_and_half() {
        let base = single(&["a"]);
        let right = with(base.clone(), "b", root_id(&base), DockSide::Right);
        assert_eq!(shape(&right.root), "row([*a] [*b])");
        assert_eq!(sizes(&right), [0.5, 0.5]);
        for (side, expected) in [
            (DockSide::Left, "row([*b] [*a])"),
            (DockSide::Top, "col([*b] [*a])"),
            (DockSide::Bottom, "col([*a] [*b])"),
        ] {
            assert_eq!(
                shape(&with(base.clone(), "b", root_id(&base), side).root),
                expected
            );
        }
    }

    #[test]
    fn an_edge_of_the_root_split_docks_against_the_whole_layout() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Bottom);
        let three = with(two.clone(), "c", root_id(&two), DockSide::Left);
        assert_eq!(shape(&three.root), "row([*c] col([*a] [*b]))");
    }

    #[test]
    fn a_duplicate_an_unknown_target_and_a_centre_drop_on_a_split_change_nothing() {
        let base = single(&["a"]);
        let split = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let mut copy = base.clone();
        assert!(!copy.add_tab(tab("a"), &center(root_id(&base))));
        assert!(!copy.add_tab(tab("z"), &center("no-such-node".into())));
        assert_eq!(copy, base);
        let mut copy = split.clone();
        assert!(!copy.add_tab(tab("z"), &center(root_id(&split))));
        assert_eq!(copy, split);
    }

    #[test]
    fn a_split_in_a_split_of_the_same_direction_is_flattened() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let three = with(two.clone(), "c", group_of(&two, "b"), DockSide::Right);
        assert_eq!(shape(&three.root), "row([*a] [*b] [*c])");
        assert_eq!(
            sizes(&three),
            [0.5, 0.25, 0.25],
            "b and c halved what b had"
        );
    }

    #[test]
    fn a_split_in_one_of_the_other_direction_stays() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let three = with(two.clone(), "c", group_of(&two, "b"), DockSide::Bottom);
        assert_eq!(shape(&three.root), "row([*a] col([*b] [*c]))");
    }

    #[test]
    fn closing_activates_the_tab_that_slid_into_its_place_or_the_new_last() {
        let mut layout = single(&["a", "b", "c"]);
        layout.activate_tab(&"b".into());
        assert!(layout.close_tab(&"b".into()));
        assert_eq!(shape(&layout.root), "[a *c]");

        let mut layout = single(&["a", "b"]);
        layout.activate_tab(&"b".into());
        layout.close_tab(&"b".into());
        assert_eq!(shape(&layout.root), "[*a]");

        let mut layout = single(&["a", "b"]);
        layout.close_tab(&"b".into());
        assert_eq!(
            shape(&layout.root),
            "[*a]",
            "the active tab stays when another closes"
        );
    }

    #[test]
    fn closing_collapses_the_emptied_group_and_its_one_child_split() {
        let base = single(&["a"]);
        let mut two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        two.close_tab(&"b".into());
        assert_eq!(shape(&two.root), "[*a]");
    }

    #[test]
    fn closing_the_last_tab_keeps_the_root_group_empty_under_its_id() {
        let base = single(&["a"]);
        let mut closed = base.clone();
        assert!(closed.close_tab(&"a".into()));
        assert_eq!(shape(&closed.root), "[]");
        assert_eq!(closed.root.id(), base.root.id());
        assert!(!closed.close_tab(&"nope".into()));
    }

    #[test]
    fn a_pinned_tab_closes_only_by_force() {
        let mut layout = Layout::single_group(vec![PaneTab {
            pinned: true,
            ..tab("plan")
        }]);
        assert!(!layout.close_tab(&"plan".into()));
        assert!(layout.force_close_tab(&"plan".into()));
        assert_eq!(shape(&layout.root), "[]");
    }

    #[test]
    fn a_tab_moves_into_another_group_and_its_emptied_group_collapses() {
        let base = single(&["a", "b"]);
        let two = with(base.clone(), "c", root_id(&base), DockSide::Right);
        let mut moved = two.clone();
        assert!(moved.move_tab(&"a".into(), &center(group_of(&two, "c"))));
        assert_eq!(shape(&moved.root), "row([*b] [c *a])");

        let base = single(&["a"]);
        let mut two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let target = group_of(&two, "a");
        two.move_tab(&"b".into(), &center(target));
        assert_eq!(shape(&two.root), "[a *b]");
    }

    #[test]
    fn a_reorder_counts_the_slot_without_the_moved_tab() {
        let mut layout = single(&["a", "b", "c"]);
        let id = root_id(&layout);
        assert!(layout.move_tab(&"c".into(), &DockTarget::tab_at(id, 0)));
        assert_eq!(shape(&layout.root), "[*c a b]");
    }

    #[test]
    fn a_group_is_split_with_one_of_its_own_tabs() {
        let mut layout = single(&["a", "b"]);
        let id = root_id(&layout);
        layout.move_tab(&"b".into(), &DockTarget::new(id, DockSide::Bottom));
        assert_eq!(shape(&layout.root), "col([*a] [*b])");
    }

    #[test]
    fn a_groups_only_tab_dropped_back_on_it_changes_nothing_on_every_side() {
        let base = single(&["a"]);
        for side in [
            DockSide::Center,
            DockSide::Left,
            DockSide::Right,
            DockSide::Top,
            DockSide::Bottom,
        ] {
            let mut copy = base.clone();
            assert!(!copy.move_tab(&"a".into(), &DockTarget::new(root_id(&base), side)));
            assert_eq!(
                copy, base,
                "the ids too: the view keys its drag state off them"
            );
        }
    }

    #[test]
    fn moving_an_unknown_tab_or_to_an_unknown_target_changes_nothing() {
        let base = single(&["a", "b"]);
        let mut copy = base.clone();
        assert!(!copy.move_tab(&"nope".into(), &center(root_id(&base))));
        assert!(!copy.move_tab(
            &"a".into(),
            &DockTarget::new("no-such-node".into(), DockSide::Right)
        ));
        assert_eq!(copy, base);
    }

    #[test]
    fn activating_switches_the_visible_tab_and_leaves_an_active_one_alone() {
        let mut layout = single(&["a", "b"]);
        assert!(layout.activate_tab(&"b".into()));
        assert_eq!(shape(&layout.root), "[a *b]");
        assert!(!layout.activate_tab(&"b".into()));
        assert!(!layout.activate_tab(&"nope".into()));
    }

    #[test]
    fn a_config_lands_on_its_tab_only() {
        let mut layout = single(&["a", "b"]);
        let mut config = Map::new();
        config.insert("cwd".into(), "/tmp".into());
        assert!(layout.set_tab_config(&"b".into(), config.clone()));
        assert_eq!(
            layout.find_tab(&"b".into()).unwrap().tab.config,
            Some(config.clone())
        );
        assert_eq!(layout.find_tab(&"a".into()).unwrap().tab.config, None);
        assert!(!layout.set_tab_config(&"nope".into(), config));
    }

    #[test]
    fn a_title_and_a_pin_change_only_what_they_name() {
        let mut layout = single(&["a", "b"]);
        assert!(layout.set_tab_title(&"a".into(), "Planning"));
        assert!(
            !layout.set_tab_title(&"a".into(), "Planning"),
            "the same title is no change"
        );
        assert!(layout.pin_where(|tab| tab.id.as_str() == "b"));
        assert!(!layout.pin_where(|tab| tab.id.as_str() == "b"));
        let titles: Vec<(&str, bool)> = layout
            .tabs()
            .map(|tab| (tab.title.as_str(), tab.pinned))
            .collect();
        assert_eq!(titles, [("Planning", false), ("b", true)]);
    }

    #[test]
    fn a_divider_drag_moves_only_the_two_panes_touching_it() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let mut three = with(two.clone(), "c", group_of(&two, "b"), DockSide::Right);
        let id = root_id(&three);
        assert!(three.resize_split(&id, 0, 0.6));
        let s = sizes(&three);
        assert!(
            close(s[0], 0.6) && close(s[1], 0.15) && close(s[2], 0.25),
            "{s:?}"
        );
    }

    #[test]
    fn a_divider_drag_stops_at_the_floor() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let id = root_id(&two);
        let mut squashed = two.clone();
        squashed.resize_split(&id, 0, 0.99);
        assert!(close(sizes(&squashed)[1], MIN_PANE_FRACTION));
        let mut other = two.clone();
        other.resize_split(&id, 0, -5.0);
        assert!(close(sizes(&other)[0], MIN_PANE_FRACTION));
    }

    #[test]
    fn a_divider_that_is_not_between_two_children_changes_nothing() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let id = root_id(&two);
        let mut copy = two.clone();
        assert!(!copy.resize_split(&id, 1, 0.5));
        assert!(!copy.resize_split(&group_of(&two, "a"), 0, 0.5));
        assert!(!copy.resize_split(&id, 0, f64::NAN));
        assert_eq!(copy, two);
    }

    #[test]
    fn wholesale_sizes_are_repaired_not_refused() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let id = root_id(&two);
        let mut layout = two.clone();
        layout.set_split_sizes(&id, &[3.0, 1.0]);
        assert_eq!(sizes(&layout), [0.75, 0.25]);
        layout.set_split_sizes(&id, &[2.0]);
        assert_eq!(sizes(&layout), [0.5, 0.5]);
        layout.set_split_sizes(&id, &[0.001, 0.999]);
        assert!(close(sizes(&layout)[0], MIN_PANE_FRACTION));
    }

    #[test]
    fn a_dividers_range_is_where_the_floor_lets_it_go() {
        let base = single(&["a"]);
        let two = with(base.clone(), "b", root_id(&base), DockSide::Right);
        let three = with(two.clone(), "c", group_of(&two, "b"), DockSide::Right);
        let id = root_id(&three);
        // 0.5 / 0.25 / 0.25: the second divider moves within the last 0.5, minus the floor on both sides.
        let range = three.divider_range(&id, 1).unwrap();
        assert!(close(range.before, 0.5));
        assert!(close(range.min, 0.5 + MIN_PANE_FRACTION));
        assert!(close(range.max, 1.0 - MIN_PANE_FRACTION));
        assert!(
            close(range.fraction(0.8), 0.3),
            "the pointer at 80% leaves the middle pane 0.3"
        );
        assert!(close(range.fraction(0.2), MIN_PANE_FRACTION), "clamped");
        assert!(three.divider_range(&id, 2).is_none());
        assert!(three.divider_range(&group_of(&three, "a"), 0).is_none());
    }
}
