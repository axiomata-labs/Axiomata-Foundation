//! The stored form, `{ "version": 1, "root": … }`: written as it is, read back with everything repaired that can be.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::ids::{NodeId, TabId};
use crate::layout::{Layout, Node, PaneTab, Split, SplitDir, TabGroup, finalize};
use crate::sizes;

/// The version written into a stored layout. Not branched on yet: with one version there is nothing to branch — the
/// first change to the tree's shape is what has to add that branch.
pub const LAYOUT_VERSION: u32 = 1;

#[derive(Serialize)]
struct Stored<'a> {
    version: u32,
    root: &'a Node,
}

impl Layout {
    /// The layout as it is stored.
    ///
    /// # Panics
    ///
    /// Never: the tree holds strings, finite numbers (every size passes [`sizes::normalize`]) and JSON values, which
    /// all serialise. Should that ever change, a loud failure beats storing an empty row that reads back as lost work.
    #[must_use]
    pub fn to_stored(&self) -> String {
        let stored = Stored {
            version: LAYOUT_VERSION,
            root: &self.root,
        };
        serde_json::to_string(&stored)
            .expect("a layout of strings, finite numbers and JSON values serialises")
    }

    /// The stored form as a JSON value, for a caller that stores the layout inside a document of its own.
    #[must_use]
    pub fn to_stored_value(&self) -> Value {
        serde_json::json!({ "version": LAYOUT_VERSION, "root": &self.root })
    }

    /// A stored layout read back: JSON text.
    #[must_use]
    pub fn from_stored(raw: &str) -> Option<Self> {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| Self::from_value(&value))
    }

    /// A stored layout read back, repairing what it can and refusing what it cannot.
    ///
    /// Anything malformed is dropped rather than trusted: unknown node types, tabs without an id or kind, a second tab
    /// reusing an id, sizes that do not match the children. `None` when nothing usable survives, so the caller falls
    /// back to its starting layout — a hand-edited or cut-off row, and a dock that silently opened empty would look
    /// like lost work. A tab's `pinned` is not read back: pinning belongs to the mode that arranges the panes, which
    /// pins them again.
    #[must_use]
    pub fn from_value(raw: &Value) -> Option<Self> {
        let mut seen = Seen::default();
        let root = node(raw.as_object()?.get("root")?, &mut seen)?;
        // A node read back holds at least one tab, so normalising keeps it.
        Some(Self {
            root: finalize(root),
        })
    }
}

/// The ids handed out while reading one stored layout.
#[derive(Default)]
struct Seen {
    nodes: HashSet<String>,
    tabs: HashSet<String>,
}

/// A non-empty string.
fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// One node, or `None` when it cannot be trusted.
///
/// A node id an earlier node already used is replaced by a fresh one rather than kept, for the reason a repeated tab
/// id drops the tab: every operation addresses nodes by id. Dropping the node would throw away panes the owner still
/// has; renaming it only costs the stored target of a node nobody can reach anyway.
fn node(raw: &Value, seen: &mut Seen) -> Option<Node> {
    let record = raw.as_object()?;
    let id = match text(record.get("id")) {
        Some(id) if !seen.nodes.contains(id) => NodeId::from(id),
        _ => NodeId::fresh(),
    };
    seen.nodes.insert(id.as_str().to_owned());
    match record.get("type").and_then(Value::as_str) {
        Some("tabs") => {
            let tabs: Vec<PaneTab> = record
                .get("tabs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|raw| tab(raw, seen))
                .collect();
            if tabs.is_empty() {
                return None;
            }
            let active = text(record.get("active"))
                .map(TabId::from)
                .filter(|active| tabs.iter().any(|tab| &tab.id == active))
                .unwrap_or_else(|| tabs[0].id.clone());
            Some(Node::Tabs(TabGroup {
                id,
                tabs,
                active: Some(active),
            }))
        }
        Some("split") => {
            let dir = if record.get("dir").and_then(Value::as_str) == Some("col") {
                SplitDir::Col
            } else {
                SplitDir::Row
            };
            let raw_sizes = record.get("sizes").and_then(Value::as_array);
            let mut children = Vec::new();
            let mut shares = Vec::new();
            for (i, child) in record
                .get("children")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(child) = node(child, seen) {
                    children.push(child);
                    shares.push(
                        raw_sizes
                            .and_then(|sizes| sizes.get(i))
                            .and_then(Value::as_f64)
                            .unwrap_or(f64::NAN),
                    );
                }
            }
            match children.len() {
                0 => None,
                1 => children.pop(),
                count => Some(Node::Split(Split {
                    id,
                    dir,
                    sizes: sizes::normalize(&shares, count),
                    children,
                })),
            }
        }
        _ => None,
    }
}

/// One tab, or `None` when it lacks an id or kind, or reuses an id.
fn tab(raw: &Value, seen: &mut Seen) -> Option<PaneTab> {
    let record = raw.as_object()?;
    let id = text(record.get("id"))?;
    let kind = text(record.get("kind"))?;
    if !seen.tabs.insert(id.to_owned()) {
        return None;
    }
    let config: Option<Map<String, Value>> =
        record.get("config").and_then(Value::as_object).cloned();
    Some(PaneTab {
        id: TabId::from(id),
        kind: kind.to_owned(),
        title: text(record.get("title")).unwrap_or(kind).to_owned(),
        config,
        pinned: false,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::layout::tests::{shape, single, tab as pane};
    use crate::layout::{DockSide, DockTarget};

    fn sample() -> Layout {
        let mut layout = single(&["a", "b"]);
        let root = layout.root().id().clone();
        layout.add_tab(
            PaneTab::new("c", "agent", "c"),
            &DockTarget::new(root, DockSide::Right),
        );
        let c = layout.find_tab(&"c".into()).unwrap().group.id().clone();
        layout.add_tab(
            PaneTab::new("d", "diff", "d"),
            &DockTarget::new(c, DockSide::Bottom),
        );
        layout
    }

    #[test]
    fn a_layout_comes_back_as_it_was_stored_with_its_version() {
        let layout = sample();
        let stored = layout.to_stored();
        assert_eq!(Layout::from_stored(&stored), Some(layout));
        let value: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(value["version"], LAYOUT_VERSION);
        assert_eq!(
            layout.to_stored_value(),
            value,
            "the text and the value say the same"
        );
    }

    #[test]
    fn the_stored_form_is_the_one_the_app_has_always_written() {
        let mut layout = single(&["a"]);
        let mut config = Map::new();
        config.insert("path".into(), "/tmp/a.rs".into());
        layout.set_tab_config(&"a".into(), config);
        let value: Value = serde_json::from_str(&layout.to_stored()).unwrap();
        let id = layout.root().id().as_str();
        assert_eq!(
            value,
            json!({
                "version": 1,
                "root": {
                    "type": "tabs",
                    "id": id,
                    "tabs": [{ "id": "a", "kind": "terminal", "title": "a", "config": { "path": "/tmp/a.rs" } }],
                    "active": "a",
                },
            })
        );
    }

    #[test]
    fn an_empty_root_writes_no_active_tab_and_a_pin_is_written() {
        let layout = Layout::empty();
        let value: Value = serde_json::from_str(&layout.to_stored()).unwrap();
        assert_eq!(value["root"]["active"], Value::Null);
        assert_eq!(value["root"]["tabs"], json!([]));

        let pinned = Layout::single_group(vec![PaneTab {
            pinned: true,
            ..pane("plan")
        }]);
        let value: Value = serde_json::from_str(&pinned.to_stored()).unwrap();
        assert_eq!(
            value["root"]["tabs"][0],
            json!({ "id": "plan", "kind": "terminal", "title": "plan", "pinned": true })
        );
    }

    #[test]
    fn a_stored_split_in_a_split_of_its_direction_is_flattened_and_still_takes_tabs() {
        let group =
            |id: &str, tab: &str| json!({ "type": "tabs", "id": id, "tabs": [pane_json(tab)] });
        let raw = json!({ "version": 1, "root": { "type": "split", "id": "s1", "dir": "row", "sizes": [0.5],
            "children": [group("g1", "a"), { "type": "split", "id": "s2", "dir": "row", "sizes": [0.5, 0.5],
                "children": [group("g2", "b"), group("g3", "c")] }] } });
        let mut layout = Layout::from_value(&raw).unwrap();
        assert_eq!(shape(layout.root()), "row([*a] [*b] [*c])");
        assert!(layout.add_tab(pane("d"), &DockTarget::new("g3".into(), DockSide::Right)));
        let Node::Split(split) = layout.root() else {
            panic!()
        };
        assert_eq!(split.children().len(), 4);
        assert!((split.sizes().iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(Layout::from_stored("not json at all {"), None);
        for raw in [
            json!(null),
            json!(42),
            json!({}),
            json!({ "version": 1, "root": { "type": "tabs", "tabs": [] } }),
            json!({ "version": 1, "root": { "type": "dock" } }),
        ] {
            assert_eq!(Layout::from_value(&raw), None, "{raw}");
        }
    }

    #[test]
    fn a_tab_without_id_or_kind_and_a_second_one_reusing_an_id_are_dropped() {
        let raw = json!({ "version": 1, "root": { "type": "tabs", "id": "g1", "active": "a", "tabs": [
            { "id": "a", "kind": "terminal", "title": "a" }, { "id": "b" }, { "kind": "terminal" },
            { "id": "a", "kind": "terminal", "title": "a" }, { "id": "c", "kind": "terminal", "title": "c" },
        ] } });
        let ids: Vec<String> = Layout::from_value(&raw)
            .unwrap()
            .tabs()
            .map(|tab| tab.id.to_string())
            .collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn a_gone_active_tab_falls_back_to_the_first_and_a_missing_title_to_the_kind() {
        let raw = json!({ "version": 1, "root": { "type": "tabs", "id": "g1", "active": "gone",
            "tabs": [{ "id": "a", "kind": "terminal" }, { "id": "b", "kind": "terminal" }] } });
        let layout = Layout::from_value(&raw).unwrap();
        assert_eq!(shape(layout.root()), "[*a b]");
        assert_eq!(layout.tabs().next().unwrap().title, "terminal");
    }

    #[test]
    fn a_split_is_repaired_or_collapsed() {
        let group = |id: &str, tab: &str| json!({ "type": "tabs", "id": id, "active": tab, "tabs": [pane_json(tab)] });
        let raw = json!({ "version": 1, "root": { "type": "split", "id": "s1", "dir": "row", "sizes": [0.5],
            "children": [group("g1", "a"), group("g2", "b")] } });
        let layout = Layout::from_value(&raw).unwrap();
        assert_eq!(shape(layout.root()), "row([*a] [*b])");
        let Node::Split(split) = layout.root() else {
            panic!()
        };
        assert!((split.sizes().iter().sum::<f64>() - 1.0).abs() < 1e-9);

        let raw = json!({ "version": 1, "root": { "type": "split", "id": "s1", "dir": "row", "sizes": [0.5, 0.5],
            "children": [{ "type": "tabs", "id": "g1", "tabs": [] }, group("g2", "a")] } });
        assert_eq!(shape(Layout::from_value(&raw).unwrap().root()), "[*a]");
    }

    #[test]
    fn a_reused_node_id_is_renamed_and_the_first_keeps_it() {
        let group = |tab: &str| json!({ "type": "tabs", "id": "g1", "active": tab, "tabs": [pane_json(tab)] });
        let raw = json!({ "version": 1, "root": { "type": "split", "id": "g1", "dir": "row", "sizes": [0.5, 0.5],
            "children": [group("a"), group("b")] } });
        let layout = Layout::from_value(&raw).unwrap();
        let mut ids: Vec<&str> = layout
            .groups()
            .iter()
            .map(|group| group.id().as_str())
            .collect();
        ids.push(layout.root().id().as_str());
        let unique: HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        assert_eq!(layout.find_node(&"g1".into()), Some(layout.root()));
    }

    #[test]
    fn an_unnamed_node_gets_an_id() {
        let raw = json!({ "version": 1, "root": { "type": "tabs", "tabs": [pane_json("a")] } });
        let layout = Layout::from_value(&raw).unwrap();
        assert_ne!(layout.root().id().as_str(), "");
    }

    #[test]
    fn a_pin_is_not_read_back_and_a_config_is() {
        let mut layout = Layout::single_group(vec![PaneTab {
            pinned: true,
            ..pane("plan")
        }]);
        let mut config = Map::new();
        config.insert("line".into(), 12.into());
        layout.set_tab_config(&"plan".into(), config.clone());
        let back = Layout::from_stored(&layout.to_stored()).unwrap();
        let tab = back.tabs().next().unwrap();
        assert!(!tab.pinned);
        assert_eq!(tab.config, Some(config));
    }

    #[test]
    fn sixteen_stored_panes_share_evenly() {
        let children: Vec<Value> =
            (0..16).map(|i| json!({ "type": "tabs", "id": format!("g{i}"), "tabs": [pane_json(&format!("t{i}"))] })).collect();
        let mut sizes = vec![json!(0.1 / 15.0); 16];
        sizes[0] = json!(0.9);
        let raw = json!({ "version": 1, "root": { "type": "split", "id": "s1", "dir": "row", "sizes": sizes,
            "children": children } });
        let Node::Split(split) = Layout::from_value(&raw).unwrap().root().clone() else {
            panic!()
        };
        let least = split.sizes().iter().copied().fold(f64::INFINITY, f64::min);
        assert!((least - 1.0 / 16.0).abs() < 1e-9);
    }

    fn pane_json(id: &str) -> Value {
        json!({ "id": id, "kind": "terminal", "title": id })
    }
}
