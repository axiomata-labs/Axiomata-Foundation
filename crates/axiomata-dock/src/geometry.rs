//! Where a drag lands: a pointer position over measured rectangles → the [`DockTarget`] the layout takes. The view
//! measures (a webview with `getBoundingClientRect`, Schimmer with its own layout pass); this is the arithmetic.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;
use crate::layout::{DockSide, DockTarget, Layout, SplitDir};

/// A measured rectangle, in the view's coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Whether the point lies inside, edges included.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

/// How far into a pane counts as its edge rather than its middle: a quarter from each side leaves the middle half as
/// "drop as a tab", the common case, which deserves the larger target; a pane the size of a terminal still has a
/// grabbable edge.
pub const DOCK_EDGE_RATIO: f64 = 0.25;

/// How much of the whole dock's edge docks against the root: much narrower than [`DOCK_EDGE_RATIO`] — the strip along
/// the outside of everything, reachable without being in the way of the pane edges just inside it.
pub const ROOT_EDGE_RATIO: f64 = 0.03;

/// How far a pointer travels before a press on a tab is a drag: below it a slightly shaky click still selects the tab.
pub const DRAG_THRESHOLD_PX: f64 = 4.0;

/// The side of `rect` a pointer drops on, or `None` when it is not over it.
///
/// The nearest edge wins, measured as a fraction of the rectangle rather than in pixels, so a wide pane's edge is no
/// easier to hit than a narrow one's; everything further in than `ratio` from all four edges is the middle. In a corner
/// the deeper edge wins, which is what the eye expects.
#[must_use]
pub fn dock_side_at(rect: &Rect, x: f64, y: f64, ratio: f64) -> Option<DockSide> {
    if !rect.contains(x, y) || rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    let left = (x - rect.x) / rect.w;
    let top = (y - rect.y) / rect.h;
    let mut nearest = (DockSide::Left, left);
    for candidate in [
        (DockSide::Right, 1.0 - left),
        (DockSide::Top, top),
        (DockSide::Bottom, 1.0 - top),
    ] {
        if candidate.1 < nearest.1 {
            nearest = candidate;
        }
    }
    Some(if nearest.1 > ratio {
        DockSide::Center
    } else {
        nearest.0
    })
}

/// Where a tab dragged along a tab bar goes: before the first tab whose midpoint the pointer has not passed, or last.
/// `tabs` are the tab buttons in drawing order **without the dragged one** — the index counts the same way, as
/// [`DockTarget::index`] does.
#[must_use]
pub fn tab_drop_index(tabs: &[Rect], x: f64) -> usize {
    tabs.iter()
        .position(|rect| x < rect.x + rect.w / 2.0)
        .unwrap_or(tabs.len())
}

/// One group as measured on screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct GroupGeometry {
    /// The group's node id.
    pub node_id: NodeId,
    /// The whole group, tab bar included.
    pub rect: Rect,
    /// The tab bar alone.
    pub tab_bar: Rect,
    /// The tab buttons in drawing order, **without the tab being dragged**.
    pub tabs: Vec<Rect>,
}

/// What a drop names: the whole layout's root, or a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum DropNode {
    /// The root, whatever its id: the measured rectangles do not know the tree.
    Root,
    /// A group.
    Group(NodeId),
}

/// Where a drag would land, as [`drop_target`] reads it off the rectangles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DropAt {
    /// What it lands on.
    pub node: DropNode,
    /// Which part of it.
    pub side: DockSide,
    /// On a tab bar: the slot among the other tabs.
    pub index: Option<usize>,
}

impl DropAt {
    /// The target in `layout`: the root named by its id.
    #[must_use]
    pub fn target(&self, layout: &Layout) -> DockTarget {
        DockTarget {
            node_id: match &self.node {
                DropNode::Root => layout.root().id().clone(),
                DropNode::Group(id) => id.clone(),
            },
            side: self.side,
            index: self.index,
        }
    }
}

/// Where a drag lands, or `None` over nothing. `root` is the whole dock; `groups` are measured once when the drag
/// starts — measuring again while the preview changes the picture would read the drag's own effect.
///
/// Three questions in this order, and the order is the design:
///
/// 1. **On a tab bar?** A tab drop at a position — even inside the root's edge strip: the bar is the smaller, more
///    deliberate target.
/// 2. **In the root's outer strip?** Then it docks against the whole layout, the only way to put a pane down the full
///    side of a dock that is already split. Only with more than one group: with one, the root *is* that group.
/// 3. **Otherwise** it belongs to the group it is over.
#[must_use]
pub fn drop_target(root: &Rect, groups: &[GroupGeometry], x: f64, y: f64) -> Option<DropAt> {
    if let Some(group) = groups.iter().find(|group| group.tab_bar.contains(x, y)) {
        return Some(DropAt {
            node: DropNode::Group(group.node_id.clone()),
            side: DockSide::Center,
            index: Some(tab_drop_index(&group.tabs, x)),
        });
    }
    if groups.len() > 1
        && let Some(side) =
            dock_side_at(root, x, y, ROOT_EDGE_RATIO).filter(|side| *side != DockSide::Center)
    {
        return Some(DropAt {
            node: DropNode::Root,
            side,
            index: None,
        });
    }
    groups.iter().find_map(|group| {
        dock_side_at(&group.rect, x, y, DOCK_EDGE_RATIO).map(|side| DropAt {
            node: DropNode::Group(group.node_id.clone()),
            side,
            index: None,
        })
    })
}

/// Where a pointer sits along a split, as a fraction of it, clamped to 0…1: a divider drag routinely leaves the
/// split's own rectangle.
#[must_use]
pub fn split_fraction_at(rect: &Rect, dir: SplitDir, x: f64, y: f64) -> f64 {
    let along = match dir {
        SplitDir::Row => (x - rect.x) / rect.w,
        SplitDir::Col => (y - rect.y) / rect.h,
    };
    if along.is_finite() {
        along.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Whether a press that started at `start` has travelled far enough to be a drag rather than a click.
#[must_use]
pub fn crossed_drag_threshold(start: (f64, f64), x: f64, y: f64, threshold: f64) -> bool {
    (x - start.0).hypot(y - start.1) >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 200×100 pane at the origin, so a tenth is a round number in both axes.
    const PANE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 200.0,
        h: 100.0,
    };

    fn side(x: f64, y: f64) -> Option<DockSide> {
        dock_side_at(&PANE, x, y, DOCK_EDGE_RATIO)
    }

    #[test]
    fn the_middle_is_a_tab_drop_and_the_nearest_edge_wins_elsewhere() {
        for (x, y) in [
            (100.0, 50.0),
            (55.0, 50.0),
            (145.0, 50.0),
            (100.0, 30.0),
            (100.0, 70.0),
        ] {
            assert_eq!(side(x, y), Some(DockSide::Center), "{x},{y}");
        }
        assert_eq!(side(10.0, 50.0), Some(DockSide::Left));
        assert_eq!(side(190.0, 50.0), Some(DockSide::Right));
        assert_eq!(side(100.0, 5.0), Some(DockSide::Top));
        assert_eq!(side(100.0, 95.0), Some(DockSide::Bottom));
    }

    #[test]
    fn edges_are_fractions_so_a_wide_panes_edge_is_no_easier_to_hit() {
        let wide = Rect { w: 2000.0, ..PANE };
        assert_eq!(side(100.0, 50.0), Some(DockSide::Center));
        assert_eq!(
            dock_side_at(&wide, 100.0, 50.0, DOCK_EDGE_RATIO),
            Some(DockSide::Left)
        );
    }

    #[test]
    fn a_corner_goes_to_the_deeper_edge() {
        assert_eq!(side(10.0, 10.0), Some(DockSide::Left));
        assert_eq!(side(30.0, 5.0), Some(DockSide::Top));
    }

    #[test]
    fn outside_and_without_area_is_nothing_and_the_ratio_is_the_callers() {
        assert_eq!(side(-1.0, 50.0), None);
        assert_eq!(side(201.0, 50.0), None);
        assert_eq!(side(100.0, 101.0), None);
        assert_eq!(
            dock_side_at(
                &Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0
                },
                0.0,
                0.0,
                DOCK_EDGE_RATIO
            ),
            None
        );
        assert_eq!(dock_side_at(&PANE, 100.0, 30.0, 0.4), Some(DockSide::Top));
        assert!(
            PANE.contains(0.0, 0.0) && PANE.contains(200.0, 100.0) && !PANE.contains(200.5, 100.0)
        );
    }

    #[test]
    fn a_tab_goes_before_the_first_midpoint_not_passed() {
        let tabs: Vec<Rect> = (0..3)
            .map(|i| Rect {
                x: f64::from(i) * 100.0,
                y: 0.0,
                w: 100.0,
                h: 30.0,
            })
            .collect();
        assert_eq!(tab_drop_index(&tabs, 10.0), 0);
        assert_eq!(tab_drop_index(&tabs, 49.0), 0);
        assert_eq!(tab_drop_index(&tabs, 51.0), 1);
        assert_eq!(tab_drop_index(&tabs, 151.0), 2);
        assert_eq!(tab_drop_index(&tabs, 290.0), 3);
        assert_eq!(tab_drop_index(&[], 10.0), 0);
    }

    /// A group taking the given horizontal half of a 1000×600 dock, with a 30px tab bar on top.
    fn half(id: &str, x: f64, tabs: u32) -> GroupGeometry {
        GroupGeometry {
            node_id: id.into(),
            rect: Rect {
                x,
                y: 0.0,
                w: 500.0,
                h: 600.0,
            },
            tab_bar: Rect {
                x,
                y: 0.0,
                w: 500.0,
                h: 30.0,
            },
            tabs: (0..tabs)
                .map(|i| Rect {
                    x: x + f64::from(i) * 100.0,
                    y: 0.0,
                    w: 100.0,
                    h: 30.0,
                })
                .collect(),
        }
    }

    const ROOT: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 600.0,
    };

    #[allow(
        clippy::unnecessary_wraps,
        reason = "compared with what `drop_target` answers"
    )]
    fn group_drop(id: &str, side: DockSide, index: Option<usize>) -> Option<DropAt> {
        Some(DropAt {
            node: DropNode::Group(id.into()),
            side,
            index,
        })
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "compared with what `drop_target` answers"
    )]
    fn root_drop(side: DockSide) -> Option<DropAt> {
        Some(DropAt {
            node: DropNode::Root,
            side,
            index: None,
        })
    }

    #[test]
    fn a_tab_bar_is_a_tab_drop_even_in_the_roots_strip() {
        let groups = [half("left", 0.0, 2), half("right", 500.0, 1)];
        assert_eq!(
            drop_target(&ROOT, &groups, 250.0, 15.0),
            group_drop("left", DockSide::Center, Some(2))
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 40.0, 15.0),
            group_drop("left", DockSide::Center, Some(0))
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 520.0, 15.0),
            group_drop("right", DockSide::Center, Some(0))
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 5.0, 15.0),
            group_drop("left", DockSide::Center, Some(0))
        );
    }

    #[test]
    fn the_outer_strip_docks_against_the_whole_layout() {
        let groups = [half("left", 0.0, 2), half("right", 500.0, 1)];
        assert_eq!(
            drop_target(&ROOT, &groups, 5.0, 300.0),
            root_drop(DockSide::Left)
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 995.0, 300.0),
            root_drop(DockSide::Right)
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 500.0, 595.0),
            root_drop(DockSide::Bottom)
        );
    }

    #[test]
    fn elsewhere_the_group_under_the_pointer_takes_it() {
        let groups = [half("left", 0.0, 2), half("right", 500.0, 1)];
        assert_eq!(
            drop_target(&ROOT, &groups, 250.0, 300.0),
            group_drop("left", DockSide::Center, None)
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 40.0, 300.0),
            group_drop("left", DockSide::Left, None)
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 250.0, 580.0),
            group_drop("left", DockSide::Bottom, None)
        );
        assert_eq!(
            drop_target(&ROOT, &groups, 750.0, 300.0),
            group_drop("right", DockSide::Center, None)
        );
        assert_eq!(drop_target(&ROOT, &groups, 2000.0, 300.0), None);
        assert_eq!(drop_target(&ROOT, &[], 500.0, 300.0), None);
    }

    #[test]
    fn one_group_offers_no_root_edge() {
        assert_eq!(
            drop_target(&ROOT, &[half("only", 0.0, 1)], 5.0, 300.0),
            group_drop("only", DockSide::Left, None)
        );
    }

    #[test]
    fn a_drop_and_a_target_cross_to_the_webview_in_its_shape() {
        let drop = group_drop("g", DockSide::Center, Some(1)).unwrap();
        assert_eq!(
            serde_json::to_value(&drop).unwrap(),
            serde_json::json!({ "node": { "kind": "group", "id": "g" }, "side": "center", "index": 1 })
        );
        assert_eq!(
            serde_json::to_value(root_drop(DockSide::Left).unwrap()).unwrap(),
            serde_json::json!({ "node": { "kind": "root" }, "side": "left", "index": null })
        );
        let target: DockTarget =
            serde_json::from_value(serde_json::json!({ "nodeId": "g", "side": "top" })).unwrap();
        assert_eq!(target, DockTarget::new("g".into(), DockSide::Top));
        let geometry: GroupGeometry = serde_json::from_value(serde_json::json!({
            "nodeId": "g", "rect": { "x": 0, "y": 0, "w": 1, "h": 1 },
            "tabBar": { "x": 0, "y": 0, "w": 1, "h": 1 }, "tabs": [],
        }))
        .unwrap();
        assert_eq!(geometry.node_id.as_str(), "g");
    }

    #[test]
    fn a_root_drop_names_the_root_by_its_id() {
        let layout = Layout::empty();
        let target = root_drop(DockSide::Left).unwrap().target(&layout);
        assert_eq!(&target.node_id, layout.root().id());
    }

    #[test]
    fn a_split_fraction_is_measured_along_its_axis_and_clamped() {
        assert!((split_fraction_at(&PANE, SplitDir::Row, 50.0, 10.0) - 0.25).abs() < 1e-9);
        assert!((split_fraction_at(&PANE, SplitDir::Col, 10.0, 25.0) - 0.25).abs() < 1e-9);
        assert!(split_fraction_at(&PANE, SplitDir::Row, -400.0, 50.0).abs() < 1e-9);
        assert!((split_fraction_at(&PANE, SplitDir::Row, 4000.0, 50.0) - 1.0).abs() < 1e-9);
        assert!(
            split_fraction_at(
                &Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0
                },
                SplitDir::Row,
                10.0,
                10.0
            )
            .abs()
                < 1e-9
        );
    }

    #[test]
    fn a_shaky_click_stays_a_click_in_any_direction() {
        let start = (100.0, 100.0);
        assert!(!crossed_drag_threshold(
            start,
            102.0,
            101.0,
            DRAG_THRESHOLD_PX
        ));
        assert!(!crossed_drag_threshold(
            start,
            100.0,
            100.0,
            DRAG_THRESHOLD_PX
        ));
        assert!(crossed_drag_threshold(
            start,
            100.0,
            104.0,
            DRAG_THRESHOLD_PX
        ));
        assert!(crossed_drag_threshold(start, 97.0, 97.0, DRAG_THRESHOLD_PX));
        assert!(!crossed_drag_threshold(start, 97.0, 97.0, 20.0));
    }
}
