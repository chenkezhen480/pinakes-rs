//! Changes to a graph space proposed by an agent and held for a person's review (design,
//! "Agents propose graph changes"). A space has at most one pending draft. A later change is
//! checked against the space as the draft would leave it (the stored graph with the draft
//! applied) and merged into the draft, so it can build on what the draft adds, change it or
//! take it back. The draft keeps the net change: nodes put (new or changed), nodes removed
//! (with their edges), edges added and edges removed; removals are applied first, so a node
//! removed and put again comes back without its old edges. A draft may instead delete the whole
//! space. Approving applies it whole, in one transaction; nothing is applied before. Graph data
//! never touches the wiki.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::GraphSnapshot;
use super::def::{self, EdgeIn, EdgeRecord, NodeIn, NodeKey, NodeRecord, SchemaDef};

/// Most changes one call may send.
pub const MAX_CHANGES: usize = 200;
/// Most nodes plus edges a draft may hold.
pub const MAX_DRAFT: usize = 5_000;

/// One change, as a caller sends it. Node types, relation types and keys are the space's
/// schema's; edge ends are `type/key` in the space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Adds a node, or changes one. What it leaves out stays as the node has it (in the
    /// draft, else in the graph): a new node's name is its key. `aliases` replaces the list;
    /// `properties` are merged in by name, `null` taking one out.
    PutNode {
        #[serde(rename = "type")]
        ty: String,
        key: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        aliases: Option<Vec<String>>,
        #[serde(default)]
        properties: Option<BTreeMap<String, serde_json::Value>>,
    },
    /// Removes a node and every edge of it.
    RemoveNode {
        #[serde(rename = "type")]
        ty: String,
        key: String,
    },
    AddEdge {
        from: String,
        #[serde(rename = "type")]
        ty: String,
        to: String,
    },
    RemoveEdge {
        from: String,
        #[serde(rename = "type")]
        ty: String,
        to: String,
    },
    /// Deletes the whole space, with every node and edge in it, once approved. The draft then
    /// holds only that: the changes it had are dropped, and no other change is taken.
    DeleteSpace,
}

const DELETES: &str =
    "the draft deletes the space; once that is approved or rejected, propose other changes";

/// One call that changed the draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DraftNote {
    pub at: String,
    /// The calling user's folder, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub reason: String,
    pub changes: usize,
}

/// The pending draft of one space: its net change from the stored graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct GraphDraft {
    pub space: String,
    /// Nodes it adds or changes, as they would be.
    pub put_nodes: Vec<NodeRecord>,
    /// Stored nodes it removes, with their edges.
    pub remove_nodes: Vec<NodeKey>,
    pub add_edges: Vec<EdgeRecord>,
    /// Stored edges it removes.
    pub remove_edges: Vec<EdgeRecord>,
    /// The latest calls that changed it.
    pub notes: Vec<DraftNote>,
    /// Every user who changed it (folders), kept however many notes are: deleting one of them
    /// deletes the draft.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// Approving it deletes the whole space instead.
    #[serde(default)]
    pub delete_space: bool,
}

impl GraphDraft {
    pub fn new(space: &str) -> Self {
        Self {
            space: space.to_owned(),
            ..Self::default()
        }
    }

    /// The draft as callers see it: without the folders of the users who wrote it, as no
    /// proposal shows its writer's.
    pub fn shown(mut self) -> Self {
        self.authors.clear();
        for n in &mut self.notes {
            n.by = None;
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        !self.delete_space
            && self.put_nodes.is_empty()
            && self.remove_nodes.is_empty()
            && self.add_edges.is_empty()
            && self.remove_edges.is_empty()
    }

    fn size(&self) -> usize {
        self.put_nodes.len()
            + self.remove_nodes.len()
            + self.add_edges.len()
            + self.remove_edges.len()
    }

    fn key(&self, ty: &str, key: &str) -> NodeKey {
        NodeKey {
            space: self.space.clone(),
            ty: ty.to_owned(),
            key: key.to_owned(),
        }
    }

    fn puts(&self, k: &NodeKey) -> Option<usize> {
        self.put_nodes
            .iter()
            .position(|n| n.ty == k.ty && n.key == k.key)
    }

    /// A node as it is once the draft is applied, if it is there.
    fn node_now(&self, live: &GraphSnapshot, k: &NodeKey) -> Option<NodeRecord> {
        if let Some(i) = self.puts(k) {
            return Some(self.put_nodes[i].clone());
        }
        if self.remove_nodes.contains(k) {
            return None;
        }
        let ix = live.node(&def::node_ref(&k.space, &k.ty, &k.key))?;
        let d = &live.graph[ix];
        Some(NodeRecord {
            space: k.space.clone(),
            ty: d.ty.clone(),
            key: d.key.clone(),
            name: d.name.clone(),
            aliases: d.aliases.clone(),
            properties: d.properties.clone(),
        })
    }

    /// Whether a node is there once the draft is applied.
    fn has_node(&self, live: &GraphSnapshot, k: &NodeKey) -> bool {
        self.puts(k).is_some()
            || (!self.remove_nodes.contains(k)
                && live.node(&def::node_ref(&k.space, &k.ty, &k.key)).is_some())
    }

    /// Whether an edge is there once the draft is applied.
    fn has_edge(&self, live: &GraphSnapshot, e: &EdgeRecord) -> bool {
        self.add_edges.contains(e)
            || (!self.remove_edges.contains(e)
                && live.has_edge(e)
                && !self
                    .remove_nodes
                    .contains(&self.key(&e.from_type, &e.from_key))
                && !self.remove_nodes.contains(&self.key(&e.to_type, &e.to_key)))
    }

    /// Merges changes into the draft, each checked against the space as the draft (with the
    /// changes before it) would leave it. All or none: on an error the draft is as it was.
    pub fn amend(
        &mut self,
        live: &GraphSnapshot,
        schema: &SchemaDef,
        changes: Vec<Change>,
    ) -> Result<(), String> {
        if changes.is_empty() || changes.len() > MAX_CHANGES {
            return Err(format!("send 1 to {MAX_CHANGES} changes"));
        }
        let mut next = self.clone();
        for (i, change) in changes.into_iter().enumerate() {
            next.amend_one(live, schema, change)
                .map_err(|e| format!("change {}: {e}", i + 1))?;
        }
        if next.size() > MAX_DRAFT {
            return Err(format!(
                "a draft holds at most {MAX_DRAFT} nodes and edges; have it reviewed first"
            ));
        }
        *self = next;
        Ok(())
    }

    fn edge(
        &self,
        schema: &SchemaDef,
        from: String,
        ty: String,
        to: String,
    ) -> Result<EdgeRecord, String> {
        def::check_edges(schema, &self.space, vec![EdgeIn { from, ty, to }])?
            .into_iter()
            .next()
            .ok_or_else(|| "no edge".to_owned())
    }

    fn amend_one(
        &mut self,
        live: &GraphSnapshot,
        schema: &SchemaDef,
        change: Change,
    ) -> Result<(), String> {
        if self.delete_space {
            return Err(DELETES.into());
        }
        match change {
            Change::DeleteSpace => {
                self.delete_space = true;
                self.put_nodes.clear();
                self.remove_nodes.clear();
                self.add_edges.clear();
                self.remove_edges.clear();
            }
            Change::PutNode {
                ty,
                key,
                name,
                aliases,
                properties,
            } => {
                // What the call leaves out stays as the node has it now.
                let now = self.node_now(live, &self.key(&ty, &key));
                let name = name.or_else(|| now.as_ref().map(|n| n.name.clone()));
                let aliases = aliases
                    .or_else(|| now.as_ref().map(|n| n.aliases.clone()))
                    .unwrap_or_default();
                let mut merged = now.map(|n| n.properties).unwrap_or_default();
                for (prop, value) in properties.unwrap_or_default() {
                    if value.is_null() {
                        merged.remove(&prop);
                    } else {
                        merged.insert(prop, value);
                    }
                }
                let node = def::check_nodes(
                    schema,
                    &self.space,
                    vec![NodeIn {
                        ty,
                        key,
                        name,
                        aliases,
                        properties: merged,
                    }],
                )?
                .into_iter()
                .next()
                .ok_or("no node")?;
                // A stored node the draft removes stays removed first: put again, it is a new
                // node, without the edges it had.
                let k = self.key(&node.ty, &node.key);
                match self.puts(&k) {
                    Some(i) => self.put_nodes[i] = node,
                    None => self.put_nodes.push(node),
                }
            }
            Change::RemoveNode { ty, key } => {
                if !schema.nodes.contains_key(&ty) {
                    return Err(format!(
                        "node type {ty:?} is not in schema {}",
                        schema.schema
                    ));
                }
                let k = self.key(&ty, &key);
                if !self.has_node(live, &k) {
                    return Err(format!("no node {ty}/{key} in space {}", self.space));
                }
                if let Some(i) = self.puts(&k) {
                    self.put_nodes.remove(i);
                }
                let stored = live.node(&def::node_ref(&self.space, &ty, &key)).is_some();
                if stored && !self.remove_nodes.contains(&k) {
                    self.remove_nodes.push(k.clone());
                }
                // Its edges go with it: those the draft adds, and the removal of stored ones
                // is implied.
                let touches = |e: &EdgeRecord| {
                    (e.from_type == k.ty && e.from_key == k.key)
                        || (e.to_type == k.ty && e.to_key == k.key)
                };
                self.add_edges.retain(|e| !touches(e));
                self.remove_edges.retain(|e| !touches(e));
            }
            Change::AddEdge { from, ty, to } => {
                let e = self.edge(schema, from, ty, to)?;
                for (t, k) in [(&e.from_type, &e.from_key), (&e.to_type, &e.to_key)] {
                    if !self.has_node(live, &self.key(t, k)) {
                        return Err(format!("no node {t}/{k} in space {}", self.space));
                    }
                }
                if let Some(i) = self.remove_edges.iter().position(|r| *r == e) {
                    self.remove_edges.remove(i);
                } else if !self.has_edge(live, &e) {
                    self.add_edges.push(e);
                }
            }
            Change::RemoveEdge { from, ty, to } => {
                let e = self.edge(schema, from, ty, to)?;
                if let Some(i) = self.add_edges.iter().position(|a| *a == e) {
                    self.add_edges.remove(i);
                } else if self.has_edge(live, &e) {
                    self.remove_edges.push(e);
                } else {
                    return Err(format!(
                        "no edge {}/{} {} {}/{} in space {}",
                        e.from_type, e.from_key, e.ty, e.to_type, e.to_key, self.space
                    ));
                }
            }
        }
        Ok(())
    }

    /// What no longer applies to the stored graph, which may have changed since the draft was
    /// made (the host writes too): a node or edge to remove that is gone, an edge whose end is
    /// gone. Empty when the draft applies as it is.
    pub fn conflicts(&self, live: &GraphSnapshot) -> Vec<String> {
        let mut out = Vec::new();
        for k in &self.remove_nodes {
            if live.node(&def::node_ref(&k.space, &k.ty, &k.key)).is_none() {
                out.push(format!("node {}/{} to remove is gone", k.ty, k.key));
            }
        }
        for e in &self.remove_edges {
            if !live.has_edge(e) {
                out.push(format!(
                    "edge {}/{} {} {}/{} to remove is gone",
                    e.from_type, e.from_key, e.ty, e.to_type, e.to_key
                ));
            }
        }
        let puts: BTreeMap<(&str, &str), ()> = self
            .put_nodes
            .iter()
            .map(|n| ((n.ty.as_str(), n.key.as_str()), ()))
            .collect();
        for e in &self.add_edges {
            for (t, k) in [(&e.from_type, &e.from_key), (&e.to_type, &e.to_key)] {
                let there = puts.contains_key(&(t.as_str(), k.as_str()))
                    || (live.node(&def::node_ref(&e.space, t, k)).is_some()
                        && !self.remove_nodes.contains(&self.key(t, k)));
                if !there {
                    out.push(format!("node {t}/{k} of a new edge is gone"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::def::SpaceDef;

    fn schema() -> SchemaDef {
        serde_json::from_value(serde_json::json!({
            "schema": "novel",
            "nodes": { "person": { "key": "id" }, "place": { "key": "id" } },
            "relations": {
                "friend_of": { "from": "person", "to": "person" },
                "lives_in": { "from": "person", "to": "place" }
            }
        }))
        .unwrap()
    }

    fn live() -> GraphSnapshot {
        let space: SpaceDef = serde_json::from_value(serde_json::json!({
            "space": "book1", "schema": "novel", "visibility": "shared"
        }))
        .unwrap();
        let node = |ty: &str, key: &str| NodeRecord {
            space: "book1".into(),
            ty: ty.into(),
            key: key.into(),
            name: key.into(),
            aliases: Vec::new(),
            properties: Default::default(),
        };
        let edge = |ty: &str, f: (&str, &str), t: (&str, &str)| EdgeRecord {
            space: "book1".into(),
            ty: ty.into(),
            from_type: f.0.into(),
            from_key: f.1.into(),
            to_type: t.0.into(),
            to_key: t.1.into(),
        };
        GraphSnapshot::build(
            vec![schema()],
            vec![space],
            def::GraphData {
                nodes: vec![
                    node("person", "anna"),
                    node("person", "ben"),
                    node("place", "town"),
                ],
                edges: vec![
                    edge("friend_of", ("person", "anna"), ("person", "ben")),
                    edge("lives_in", ("person", "ben"), ("place", "town")),
                ],
            },
        )
    }

    fn put(ty: &str, key: &str, name: &str) -> Change {
        Change::PutNode {
            ty: ty.into(),
            key: key.into(),
            name: Some(name.into()),
            aliases: None,
            properties: None,
        }
    }

    fn add(from: &str, ty: &str, to: &str) -> Change {
        Change::AddEdge {
            from: from.into(),
            ty: ty.into(),
            to: to.into(),
        }
    }

    #[test]
    fn later_changes_build_on_the_draft() {
        let (live, schema) = (live(), schema());
        let mut d = GraphDraft::new("book1");
        // An edge to a node nobody added yet is refused, and nothing changes.
        assert!(
            d.amend(
                &live,
                &schema,
                vec![add("person/cleo", "friend_of", "person/anna")]
            )
            .is_err()
        );
        assert!(d.is_empty());
        d.amend(&live, &schema, vec![put("person", "cleo", "Cleo")])
            .unwrap();
        // The next call may link the node the draft adds, and rename it.
        d.amend(
            &live,
            &schema,
            vec![
                add("person/cleo", "friend_of", "person/anna"),
                put("person", "cleo", "Cleopatra"),
            ],
        )
        .unwrap();
        assert_eq!(d.put_nodes.len(), 1);
        assert_eq!(d.put_nodes[0].name, "Cleopatra");
        assert_eq!(d.add_edges.len(), 1);
        // Removing what the draft added takes it out of the draft, edges included.
        d.amend(
            &live,
            &schema,
            vec![Change::RemoveNode {
                ty: "person".into(),
                key: "cleo".into(),
            }],
        )
        .unwrap();
        assert!(d.is_empty(), "{d:?}");
        // Removing a stored node takes its edges; adding one back to it is refused.
        d.amend(
            &live,
            &schema,
            vec![Change::RemoveNode {
                ty: "person".into(),
                key: "ben".into(),
            }],
        )
        .unwrap();
        assert!(
            d.amend(
                &live,
                &schema,
                vec![add("person/ben", "lives_in", "place/town")]
            )
            .is_err()
        );
        // An edge removed and added again is no change.
        let mut e = GraphDraft::new("book1");
        e.amend(
            &live,
            &schema,
            vec![
                Change::RemoveEdge {
                    from: "person/anna".into(),
                    ty: "friend_of".into(),
                    to: "person/ben".into(),
                },
                add("person/anna", "friend_of", "person/ben"),
            ],
        )
        .unwrap();
        assert!(e.is_empty(), "{e:?}");
        // The schema holds: a relation between the wrong types is refused.
        assert!(
            e.amend(
                &live,
                &schema,
                vec![add("place/town", "lives_in", "person/anna")]
            )
            .is_err()
        );
    }

    #[test]
    fn a_draft_that_no_longer_applies_says_why() {
        let (live, schema) = (live(), schema());
        let mut d = GraphDraft::new("book1");
        d.amend(
            &live,
            &schema,
            vec![
                Change::RemoveEdge {
                    from: "person/anna".into(),
                    ty: "friend_of".into(),
                    to: "person/ben".into(),
                },
                add("person/anna", "lives_in", "place/town"),
            ],
        )
        .unwrap();
        assert!(d.conflicts(&live).is_empty());
        let gone = live.without(
            &[NodeKey {
                space: "book1".into(),
                ty: "place".into(),
                key: "town".into(),
            }],
            &[],
        );
        let c = d.conflicts(&gone);
        assert_eq!(
            c,
            vec!["node place/town of a new edge is gone".to_string()],
            "{c:?}"
        );
    }

    /// A change leaves what it doesn't name as the node has it: a rename keeps aliases and
    /// properties, a property is merged in, `null` takes one out.
    #[test]
    fn what_a_change_leaves_out_stays() {
        let schema: SchemaDef = serde_json::from_value(serde_json::json!({
            "schema": "novel",
            "nodes": { "person": { "key": "id",
                "properties": { "age": { "type": "number" }, "role": { "type": "text" } } } }
        }))
        .unwrap();
        let space: def::SpaceDef = serde_json::from_value(serde_json::json!({
            "space": "book1", "schema": "novel", "visibility": "shared"
        }))
        .unwrap();
        let live = GraphSnapshot::build(
            vec![schema.clone()],
            vec![space],
            def::GraphData {
                nodes: vec![NodeRecord {
                    space: "book1".into(),
                    ty: "person".into(),
                    key: "anna".into(),
                    name: "Anna".into(),
                    aliases: vec!["Annie".into()],
                    properties: serde_json::from_value(
                        serde_json::json!({ "age": 30, "role": "hero" }),
                    )
                    .unwrap(),
                }],
                edges: vec![],
            },
        );
        let change = |v: serde_json::Value| -> Change { serde_json::from_value(v).unwrap() };
        let mut d = GraphDraft::new("book1");
        d.amend(
            &live,
            &schema,
            vec![
                change(serde_json::json!({ "op": "put_node", "type": "person", "key": "anna", "name": "Anna K." })),
                change(serde_json::json!({ "op": "put_node", "type": "person", "key": "anna",
                                           "properties": { "age": 31, "role": null } })),
            ],
        )
        .unwrap();
        let n = &d.put_nodes[0];
        assert_eq!(n.name, "Anna K.");
        assert_eq!(n.aliases, vec!["Annie".to_string()]);
        assert_eq!(
            serde_json::json!(n.properties),
            serde_json::json!({ "age": 31 })
        );
        // A new node's name is its key.
        d.amend(
            &live,
            &schema,
            vec![change(
                serde_json::json!({ "op": "put_node", "type": "person", "key": "ben" }),
            )],
        )
        .unwrap();
        assert_eq!(d.put_nodes[1].name, "ben");
    }

    /// A stored node removed and put again comes back as a new node: its stored edges stay
    /// gone, and removing it again doesn't list it twice.
    #[test]
    fn a_node_removed_and_put_again_is_new() {
        let live = live();
        let mut d = GraphDraft::new("book1");
        let remove = || Change::RemoveNode {
            ty: "person".into(),
            key: "ben".into(),
        };
        d.amend(
            &live,
            &schema(),
            vec![remove(), put("person", "ben", "Ben")],
        )
        .unwrap();
        let ben_town = EdgeRecord {
            space: "book1".into(),
            ty: "lives_in".into(),
            from_type: "person".into(),
            from_key: "ben".into(),
            to_type: "place".into(),
            to_key: "town".into(),
        };
        assert!(!d.has_edge(&live, &ben_town), "its old edge is gone");
        assert_eq!(d.remove_nodes.len(), 1);
        assert_eq!(d.put_nodes[0].name, "Ben");
        assert!(d.put_nodes[0].aliases.is_empty());
        d.amend(&live, &schema(), vec![remove()]).unwrap();
        assert_eq!(d.remove_nodes.len(), 1);
        assert!(d.put_nodes.is_empty());
        let after = live
            .without(&d.remove_nodes, &d.remove_edges)
            .with_data(d.put_nodes.clone(), d.add_edges.clone());
        assert!(after.node("book1:person/ben").is_none());
    }
}
