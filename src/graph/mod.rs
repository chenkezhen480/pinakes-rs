//! The business graph (design, "The business graph: a schema description plus live data").
//!
//! A schema defines node and relation types; graph spaces are bodies of data built from a
//! schema, each shared, private or restricted. Schema definitions and the space registry live
//! in the tenant's SQLite store; nodes and edges in a [`GraphStore`] (Neo4j, or SQLite). When a
//! tenant opens, all of it is read into a [`GraphSnapshot`]: a petgraph copy that searches and
//! walks read, never the stores.

pub mod def;
pub mod describe;
pub mod neo4j;
pub mod sqlite;
pub mod store;
pub mod walk;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};

pub use def::{
    EdgeRecord, EntityRef, GraphData, NodeRecord, SchemaDef, SpaceDef, Visibility, node_ref,
};
pub use store::GraphStore;

use crate::identity::Scope;
use crate::search::names::{NameMatcher, Target};
use crate::search::text::{RunKind, normalize, on_word_boundary, runs};
use crate::tenants::{group_dir_name, user_dir_name};

/// Deepest walk a config may allow, and most nodes.
pub const MAX_WALK_DEPTH: usize = 6;
pub const MAX_WALK_NODES: usize = 1_000;

/// Where each tenant's graph data is stored.
#[derive(Clone)]
pub enum GraphBackend {
    /// `graph.sqlite` in the tenant's folder.
    Sqlite,
    Neo4j(Arc<neo4j::Neo4jClient>),
}

impl GraphBackend {
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        match cfg.graph.backend {
            crate::config::GraphBackendKind::Sqlite => GraphBackend::Sqlite,
            crate::config::GraphBackendKind::Neo4j => {
                GraphBackend::Neo4j(Arc::new(neo4j::Neo4jClient::new(cfg.neo4j.clone())))
            }
        }
    }

    /// Removes any graph data stored under a tenant id, before a tenant with that id is created:
    /// a deleted tenant may have left some in a shared Neo4j database. A SQLite graph lives in
    /// the tenant's own new folder, so there is nothing to clear.
    pub async fn clear_tenant(&self, tenant_id: &str) -> anyhow::Result<()> {
        match self {
            GraphBackend::Sqlite => Ok(()),
            GraphBackend::Neo4j(client) => {
                neo4j::Neo4jGraphStore::new(client.clone(), tenant_id)
                    .delete_all()
                    .await
            }
        }
    }

    /// The store of one tenant, bound to its id.
    pub fn open(
        &self,
        tenant_id: &str,
        tenant_dir: &std::path::Path,
    ) -> anyhow::Result<Arc<dyn GraphStore>> {
        Ok(match self {
            GraphBackend::Sqlite => Arc::new(sqlite::SqliteGraphStore::open(
                &tenant_dir.join("graph.sqlite"),
                tenant_id,
            )?),
            GraphBackend::Neo4j(client) => {
                Arc::new(neo4j::Neo4jGraphStore::new(client.clone(), tenant_id))
            }
        })
    }
}

/// A registered space, with its owner and members as folder names for comparing with a
/// caller's scope.
#[derive(Debug, Clone)]
pub struct SpaceEntry {
    pub def: SpaceDef,
    owner_dir: Option<String>,
    user_dirs: Vec<String>,
    group_dirs: Vec<String>,
}

impl SpaceEntry {
    fn new(def: SpaceDef) -> Self {
        Self {
            owner_dir: def.owner.as_deref().map(user_dir_name),
            user_dirs: def.members.users.iter().map(|u| user_dir_name(u)).collect(),
            group_dirs: def
                .members
                .groups
                .iter()
                .map(|g| group_dir_name(g))
                .collect(),
            def,
        }
    }

    /// Whether a caller may see this space: everyone for a shared space; its owner for a
    /// private one; its owner (the user who created it) and the listed users and groups for a
    /// restricted one.
    pub fn visible_to(&self, scope: &Scope) -> bool {
        let user = scope.user().map(|u| u.dir.as_str());
        let is_owner = user.is_some() && user == self.owner_dir.as_deref();
        match self.def.visibility {
            Visibility::Shared => true,
            Visibility::Private => is_owner,
            Visibility::Restricted => {
                is_owner
                    || user.is_some_and(|u| self.user_dirs.iter().any(|d| d == u))
                    || scope
                        .groups()
                        .iter()
                        .any(|g| self.group_dirs.contains(&g.dir))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeData {
    /// Index of its space in [`GraphSnapshot::spaces`].
    pub space: usize,
    pub ty: String,
    pub key: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// `space:type/key`.
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct EdgeData {
    pub ty: String,
}

/// Which spaces one caller may see, worked out once per request.
#[derive(Debug, Clone)]
pub struct Visible(Vec<bool>);

impl Visible {
    pub fn space(&self, i: usize) -> bool {
        self.0.get(i).copied().unwrap_or(false)
    }

    pub fn node(&self, g: &GraphSnapshot, n: NodeIndex) -> bool {
        g.graph.node_weight(n).is_some_and(|d| self.space(d.space))
    }

    /// Whether a node anchor (`space:type/key`) may be shown: only when its space is visible.
    /// Nodes of unknown spaces are hidden.
    pub fn node_anchor(&self, g: &GraphSnapshot, entity: &str) -> bool {
        match EntityRef::parse(entity) {
            Some(EntityRef::Node { space, .. }) => {
                g.space_index(space).is_some_and(|i| self.space(i))
            }
            _ => false,
        }
    }
}

/// Everything searches and walks read about a tenant's graph. Replaced whole on every change,
/// so running requests keep the copy they started with.
pub struct GraphSnapshot {
    /// A hash of the schemas, spaces, nodes and edges it was built from.
    pub version: String,
    pub schemas: BTreeMap<String, SchemaDef>,
    pub spaces: Vec<SpaceEntry>,
    space_by_name: HashMap<String, usize>,
    pub graph: DiGraph<NodeData, EdgeData>,
    by_id: HashMap<String, NodeIndex>,
    /// Node names and aliases, keys that look like identifiers, space names and aliases.
    pub names: NameMatcher,
    /// Keys made only of digits, such as order 1042: found only next to a name of the node's
    /// type ("order 1042", "订单1042").
    numeric_keys: HashMap<String, Vec<NodeIndex>>,
    /// Normalized names of each `(schema, type)`: the type name and its aliases.
    type_names: HashMap<(String, String), Vec<String>>,
    /// Every node's normalized name and aliases, for keywords that are part of a name.
    all_names: Vec<(String, NodeIndex)>,
}

/// A keyword that is part of more node names than this is too general to name a node.
const MAX_PARTIAL_HITS: usize = 3;

impl GraphSnapshot {
    pub fn empty() -> Self {
        Self::build(Vec::new(), Vec::new(), GraphData::default())
    }

    /// Builds the in-memory copy. Nodes of unknown spaces and edges with a missing end are
    /// skipped with a warning; the write path never produces them.
    pub fn build(schemas: Vec<SchemaDef>, spaces: Vec<SpaceDef>, data: GraphData) -> Self {
        let schemas: BTreeMap<String, SchemaDef> =
            schemas.into_iter().map(|s| (s.schema.clone(), s)).collect();
        let mut spaces: Vec<SpaceDef> = spaces;
        spaces.sort_by(|a, b| a.space.cmp(&b.space));
        let spaces: Vec<SpaceEntry> = spaces.into_iter().map(SpaceEntry::new).collect();
        let space_by_name: HashMap<String, usize> = spaces
            .iter()
            .enumerate()
            .map(|(i, s)| (s.def.space.clone(), i))
            .collect();

        let mut graph = DiGraph::new();
        let mut by_id = HashMap::new();
        let mut names: Vec<(String, Target)> = Vec::new();
        let mut numeric_keys: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        let mut all_names = Vec::new();
        for n in data.nodes {
            let Some(&space) = space_by_name.get(&n.space) else {
                tracing::warn!(space = %n.space, "skipping a node of an unknown space");
                continue;
            };
            let id = node_ref(&n.space, &n.ty, &n.key);
            if by_id.contains_key(&id) {
                continue;
            }
            let ix = graph.add_node(NodeData {
                space,
                ty: n.ty,
                key: n.key,
                name: n.name,
                aliases: n.aliases,
                id: id.clone(),
            });
            by_id.insert(id, ix);
            let d = &graph[ix];
            // A name that is only the key (the default when none was sent) is matched by the
            // key rules below, never as a name: a bare 1042 or "support" names nothing.
            let own_names = (d.name != d.key)
                .then_some(&d.name)
                .into_iter()
                .chain(&d.aliases);
            let target = || Target::Node(ix.index() as u32);
            for name in own_names {
                all_names.push((normalize(name.trim()), ix));
                names.push((name.clone(), target()));
            }
            let key = normalize(&d.key);
            if key.chars().all(|c| c.is_ascii_digit()) {
                numeric_keys.entry(key).or_default().push(ix);
            } else if key
                .chars()
                .any(|c| c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
            {
                // Identifiers such as SKU-AIR-01 or cb_301; plain words (team, card) as keys
                // would match ordinary text.
                names.push((d.key.clone(), target()));
            }
        }
        let mut seen_edges = HashSet::new();
        for e in data.edges {
            let from = by_id.get(&node_ref(&e.space, &e.from_type, &e.from_key));
            let to = by_id.get(&node_ref(&e.space, &e.to_type, &e.to_key));
            let (Some(&from), Some(&to)) = (from, to) else {
                tracing::warn!(space = %e.space, relation = %e.ty, "skipping an edge with a missing end");
                continue;
            };
            if seen_edges.insert((from, to, e.ty.clone())) {
                graph.add_edge(from, to, EdgeData { ty: e.ty });
            }
        }
        for (i, s) in spaces.iter().enumerate() {
            names.extend(s.def.name.iter().map(|n| (n.clone(), Target::Space(i))));
            names.extend(s.def.aliases.iter().map(|a| (a.clone(), Target::Space(i))));
        }
        let mut type_names = HashMap::new();
        for s in schemas.values() {
            for (ty, def) in &s.nodes {
                let mut list = vec![normalize(&ty.replace('_', " "))];
                list.extend(def.aliases.iter().map(|a| normalize(a.trim())));
                list.retain(|n| !n.is_empty());
                type_names.insert((s.schema.clone(), ty.clone()), list);
            }
        }
        let mut snapshot = Self {
            version: String::new(),
            schemas,
            spaces,
            space_by_name,
            graph,
            by_id,
            names: NameMatcher::new(names),
            numeric_keys,
            type_names,
            all_names,
        };
        // Hashed from what the snapshot holds, not the input: dropped and repeated records
        // don't change it, so the same graph always has the same version.
        let schemas: Vec<SchemaDef> = snapshot.schemas.values().cloned().collect();
        let spaces: Vec<SpaceDef> = snapshot.spaces.iter().map(|s| s.def.clone()).collect();
        snapshot.version = version_of(&schemas, &spaces, &snapshot.data());
        snapshot
    }

    /// Builds the next snapshot from `current` off the async threads.
    pub async fn build_blocking(
        current: Arc<GraphSnapshot>,
        f: impl FnOnce(&GraphSnapshot) -> GraphSnapshot + Send + 'static,
    ) -> anyhow::Result<GraphSnapshot> {
        tokio::task::spawn_blocking(move || f(&current))
            .await
            .map_err(|e| anyhow::anyhow!("building the graph snapshot panicked: {e}"))
    }

    /// The same data with other schemas and spaces, after one of them changed.
    pub fn with_meta(&self, schemas: Vec<SchemaDef>, spaces: Vec<SpaceDef>) -> Self {
        Self::build(schemas, spaces, self.data())
    }

    /// The same schemas and spaces with nodes and edges added; a node with the type and key of
    /// a stored one in the same space replaces it.
    pub fn with_data(&self, nodes: Vec<NodeRecord>, edges: Vec<EdgeRecord>) -> Self {
        let mut data = self.data();
        {
            let new: HashSet<(&str, &str, &str)> = nodes
                .iter()
                .map(|n| (n.space.as_str(), n.ty.as_str(), n.key.as_str()))
                .collect();
            data.nodes
                .retain(|n| !new.contains(&(n.space.as_str(), n.ty.as_str(), n.key.as_str())));
        }
        data.nodes.extend(nodes);
        // `build` keeps one copy of each edge.
        data.edges.extend(edges);
        Self::build(
            self.schemas.values().cloned().collect(),
            self.spaces.iter().map(|s| s.def.clone()).collect(),
            data,
        )
    }

    /// The nodes and edges as stored.
    pub fn data(&self) -> GraphData {
        let space = |i: usize| self.spaces[i].def.space.clone();
        let nodes = self
            .graph
            .node_weights()
            .map(|n| NodeRecord {
                space: space(n.space),
                ty: n.ty.clone(),
                key: n.key.clone(),
                name: n.name.clone(),
                aliases: n.aliases.clone(),
            })
            .collect();
        let edges = self
            .graph
            .edge_indices()
            .map(|e| {
                let (a, b) = self.graph.edge_endpoints(e).expect("edge exists");
                let (a, b) = (&self.graph[a], &self.graph[b]);
                EdgeRecord {
                    space: space(a.space),
                    ty: self.graph[e].ty.clone(),
                    from_type: a.ty.clone(),
                    from_key: a.key.clone(),
                    to_type: b.ty.clone(),
                    to_key: b.key.clone(),
                }
            })
            .collect();
        GraphData { nodes, edges }
    }

    /// Which spaces a caller may see.
    pub fn visible(&self, scope: &Scope) -> Visible {
        Visible(self.spaces.iter().map(|s| s.visible_to(scope)).collect())
    }

    pub fn space_index(&self, name: &str) -> Option<usize> {
        self.space_by_name.get(name).copied()
    }

    pub fn space(&self, name: &str) -> Option<&SpaceEntry> {
        self.space_index(name).map(|i| &self.spaces[i])
    }

    /// A node by its reference, `space:type/key`.
    pub fn node(&self, id: &str) -> Option<NodeIndex> {
        self.by_id.get(id).copied()
    }

    /// The schema of a node's space.
    pub fn schema_of(&self, n: NodeIndex) -> &str {
        &self.spaces[self.graph[n].space].def.schema
    }

    /// The relation definition behind an edge.
    pub fn relation(&self, e: EdgeIndex) -> Option<&def::RelationDef> {
        let (a, _) = self.graph.edge_endpoints(e)?;
        self.schemas
            .get(self.schema_of(a))?
            .relations
            .get(&self.graph[e].ty)
    }

    /// Nodes whose key is a number found in a normalized text right next to one of their
    /// type's names, before it ("order 1042", "order #1042", "订单1042") or after it ("1042
    /// order"): every visible order 1042. "customer 1042's order history" finds customer 1042
    /// only. Hidden nodes are left out.
    pub fn numeric_key_hits(&self, text: &str, visible: &Visible) -> Vec<NodeIndex> {
        let mut out = Vec::new();
        for run in runs(text) {
            if run.kind != RunKind::Word {
                continue;
            }
            let Some(nodes) = self.numeric_keys.get(run.text) else {
                continue;
            };
            let start = run.text.as_ptr() as usize - text.as_ptr() as usize;
            let end = start + run.text.len();
            let before = text[..start].trim_end_matches([' ', '#', ':', '-']);
            let after = text[end..].trim_start_matches(' ');
            let next_to = |name: &str| {
                (before.ends_with(name)
                    && on_word_boundary(text, before.len() - name.len(), before.len()))
                    || (after.starts_with(name) && {
                        let s = text.len() - after.len();
                        on_word_boundary(text, s, s + name.len())
                    })
            };
            for &n in nodes {
                if !visible.node(self, n) || out.contains(&n) {
                    continue;
                }
                let key = (self.schema_of(n).to_owned(), self.graph[n].ty.clone());
                let named = self
                    .type_names
                    .get(&key)
                    .is_some_and(|names| names.iter().any(|name| next_to(name)));
                if named {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Nodes one of whose names contains a normalized keyword: the agent wrote 冲压线 for
    /// "冲压线 L1" or 平安健康 for "平安健康高端医疗". Only specific keywords count (three or more
    /// Chinese characters, or four or more other characters on word boundaries), and only when
    /// at most a few visible nodes match; hidden nodes are never considered.
    pub fn partial_name_hits(&self, keyword: &str, visible: &Visible) -> Vec<NodeIndex> {
        let n = keyword.chars().count();
        let cjk = crate::search::text::all_cjk(keyword);
        if (cjk && n < 3) || (!cjk && n < 4) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (name, ix) in &self.all_names {
            if out.contains(ix) || !visible.node(self, *ix) {
                continue;
            }
            let inside = name
                .match_indices(keyword)
                .any(|(i, _)| cjk || on_word_boundary(name, i, i + keyword.len()));
            if inside {
                out.push(*ix);
                if out.len() > MAX_PARTIAL_HITS {
                    return Vec::new();
                }
            }
        }
        out
    }

    /// Whether a stored schema defines this node type.
    pub fn has_node_type(&self, schema: &str, ty: &str) -> bool {
        self.schemas
            .get(schema)
            .is_some_and(|s| s.nodes.contains_key(ty))
    }

    /// Whether this node type is a relation's both ends in its schema (such as a merchant
    /// under a parent merchant).
    pub fn has_self_relation(&self, schema: &str, ty: &str) -> bool {
        self.schemas
            .get(schema)
            .is_some_and(|s| s.relations.values().any(|r| r.from == ty && r.to == ty))
    }
}

impl def::Usage for GraphSnapshot {
    fn uses_node_type(&self, schema: &str, ty: &str) -> bool {
        self.graph
            .node_weights()
            .any(|n| n.ty == ty && self.spaces[n.space].def.schema == schema)
    }

    fn uses_relation(&self, schema: &str, rel: &str) -> bool {
        self.graph.edge_indices().any(|e| {
            self.graph[e].ty == rel && {
                let (a, _) = self.graph.edge_endpoints(e).expect("edge exists");
                self.schema_of(a) == schema
            }
        })
    }
}

/// 16 hex digits of a SHA-256 over the schemas, spaces, nodes and edges, in a stable order.
fn version_of(schemas: &[SchemaDef], spaces: &[SpaceDef], data: &GraphData) -> String {
    use sha2::{Digest, Sha256};
    let mut schemas: Vec<&SchemaDef> = schemas.iter().collect();
    schemas.sort_by(|a, b| a.schema.cmp(&b.schema));
    let mut spaces: Vec<&SpaceDef> = spaces.iter().collect();
    spaces.sort_by(|a, b| a.space.cmp(&b.space));
    let mut nodes: Vec<&NodeRecord> = data.nodes.iter().collect();
    nodes.sort_by(|a, b| (&a.space, &a.ty, &a.key).cmp(&(&b.space, &b.ty, &b.key)));
    let mut edges: Vec<&EdgeRecord> = data.edges.iter().collect();
    edges.sort();
    let mut h = Sha256::new();
    for part in [
        serde_json::to_string(&schemas),
        serde_json::to_string(&spaces),
        serde_json::to_string(&nodes),
        serde_json::to_string(&edges),
    ] {
        h.update(part.unwrap_or_default().as_bytes());
        h.update([0u8]);
    }
    hex::encode(&h.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::def::Members;

    fn schema() -> SchemaDef {
        serde_json::from_value(serde_json::json!({
            "schema": "commerce",
            "nodes": {
                "order": { "key": "order_id", "aliases": ["orders", "订单"] },
                "customer": { "key": "customer_id" }
            },
            "relations": { "placed_by": { "from": "order", "to": "customer" } }
        }))
        .unwrap()
    }

    fn space(name: &str, vis: Visibility, owner: Option<&str>, users: &[&str]) -> SpaceDef {
        SpaceDef {
            space: name.into(),
            schema: "commerce".into(),
            name: Some(format!("{name} store")),
            aliases: vec![],
            visibility: vis,
            owner: owner.map(str::to_owned),
            members: Members {
                users: users.iter().map(|u| u.to_string()).collect(),
                groups: vec![],
            },
        }
    }

    pub(crate) fn sample() -> GraphSnapshot {
        let node = |space: &str, ty: &str, key: &str, name: &str| NodeRecord {
            space: space.into(),
            ty: ty.into(),
            key: key.into(),
            name: name.into(),
            aliases: vec![],
        };
        let edge = |space: &str, o: &str, c: &str| EdgeRecord {
            space: space.into(),
            ty: "placed_by".into(),
            from_type: "order".into(),
            from_key: o.into(),
            to_type: "customer".into(),
            to_key: c.into(),
        };
        GraphSnapshot::build(
            vec![schema()],
            vec![
                space("mainland", Visibility::Shared, None, &[]),
                space("secret", Visibility::Private, Some("jin"), &[]),
            ],
            GraphData {
                nodes: vec![
                    node("mainland", "order", "1042", "Order 1042"),
                    node("mainland", "customer", "c_5521", "林晓"),
                    node("secret", "order", "1042", "Secret order"),
                    node("secret", "customer", "c_1", "Hidden Buyer"),
                    node("nowhere", "order", "1", "Lost"),
                ],
                edges: vec![
                    edge("mainland", "1042", "c_5521"),
                    edge("secret", "1042", "c_1"),
                    edge("mainland", "1042", "missing"),
                ],
            },
        )
    }

    #[test]
    fn builds_and_round_trips() {
        let g = sample();
        assert_eq!(
            g.graph.node_count(),
            4,
            "the node of an unknown space is skipped"
        );
        assert_eq!(
            g.graph.edge_count(),
            2,
            "the edge with a missing end is skipped"
        );
        let again = GraphSnapshot::build(
            g.schemas.values().cloned().collect(),
            g.spaces.iter().map(|s| s.def.clone()).collect(),
            g.data(),
        );
        assert_eq!(again.graph.node_count(), 4);
        assert_eq!(
            g.node("mainland:customer/c_5521").map(|n| &g.graph[n].name),
            Some(&"林晓".to_string())
        );

        let more = g.with_data(
            vec![NodeRecord {
                space: "mainland".into(),
                ty: "customer".into(),
                key: "c_5521".into(),
                name: "Lin Xiao".into(),
                aliases: vec![],
            }],
            vec![],
        );
        assert_eq!(more.graph.node_count(), 4);
        let n = more.node("mainland:customer/c_5521").unwrap();
        assert_eq!(more.graph[n].name, "Lin Xiao");
        assert_eq!(more.graph.edge_count(), 2, "edges survive a node update");
        assert_ne!(more.version, g.version);

        // Writing edges that are already there changes nothing, the version included.
        let same = g.with_data(vec![], g.data().edges);
        assert_eq!(same.graph.edge_count(), 2);
        assert_eq!(same.version, g.version);
    }

    #[test]
    fn numeric_keys_need_their_type_named() {
        let g = sample();
        let all = Visible(vec![true, true]);
        let hits = |text: &str| {
            let mut v: Vec<String> = g
                .numeric_key_hits(&normalize(text), &all)
                .into_iter()
                .map(|n| g.graph[n].id.clone())
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            hits("order 1042"),
            vec!["mainland:order/1042", "secret:order/1042"]
        );
        assert_eq!(
            hits("订单1042的客户"),
            vec!["mainland:order/1042", "secret:order/1042"]
        );
        assert!(hits("in 1042 we").is_empty());
        assert_eq!(hits("order #1042").len(), 2);
        assert_eq!(hits("the 1042 orders").len(), 2);
        // The type's name must be next to the number, not anywhere in the text.
        assert!(hits("order history for customer 1042").is_empty());
        assert!(hits("reorder 1042").is_empty());
        let mainland_only = Visible(vec![true, false]);
        assert_eq!(
            g.numeric_key_hits("order 1042", &mainland_only).len(),
            1,
            "hidden spaces' nodes are never found"
        );
    }
}
