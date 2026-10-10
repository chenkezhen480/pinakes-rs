//! The business graph (design, "The business graph: a schema description plus live data").
//!
//! A schema defines node and relation types; graph spaces are bodies of data built from a
//! schema, each shared, private or restricted. Schema definitions and the space registry live
//! in the tenant's SQLite store; nodes and edges in a [`GraphStore`] (Neo4j, or SQLite). When a
//! tenant opens, all of it is read into a [`GraphSnapshot`]: a petgraph copy that searches and
//! walks read, never the stores.

pub mod def;
pub mod describe;
pub mod draft;
pub mod migrate;
pub mod neo4j;
pub mod sqlite;
pub mod store;
pub mod walk;

use std::collections::{BTreeMap, HashMap};
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

    /// The backend's name, as config and the tenant's record of it say it.
    pub fn name(&self) -> &'static str {
        match self {
            GraphBackend::Sqlite => "sqlite",
            GraphBackend::Neo4j(_) => "neo4j",
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
    /// Its owner's user folder, when it has one.
    pub owner_dir: Option<String>,
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

    /// Whether the user with this folder may see the space as its owner or a listed user
    /// (shared spaces always; a group's membership isn't known from a folder alone).
    pub fn visible_to_user(&self, user_dir: &str) -> bool {
        let named = self.owner_dir.as_deref() == Some(user_dir)
            || self.user_dirs.iter().any(|d| d == user_dir);
        match self.def.visibility {
            Visibility::Shared => true,
            Visibility::Private => self.owner_dir.as_deref() == Some(user_dir),
            Visibility::Restricted => named,
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
    /// Values of the properties its type declares.
    pub properties: std::collections::BTreeMap<String, serde_json::Value>,
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
    /// For each pair of adjacent characters, the `all_names` entries holding it, in order: a
    /// keyword is looked for only in the names holding its rarest pair.
    name_pairs: HashMap<(char, char), Vec<u32>>,
    /// The sum of every node's and edge's hash: what the version says about the data, kept
    /// up to date by a write without hashing the whole graph again.
    data_sum: u128,
}

/// A graph and its node ids, while a snapshot is being made.
struct Parts {
    graph: DiGraph<NodeData, EdgeData>,
    by_id: HashMap<String, NodeIndex>,
    data_sum: u128,
}

impl Parts {
    /// Adds nodes (replacing the name and aliases of a node already there) and edges (once
    /// each), skipping nodes of unknown spaces and edges with a missing end.
    fn add(
        &mut self,
        space_by_name: &HashMap<String, usize>,
        nodes: Vec<NodeRecord>,
        edges: Vec<EdgeRecord>,
    ) {
        for n in nodes {
            let Some(&space) = space_by_name.get(&n.space) else {
                tracing::warn!(space = %n.space, "skipping a node of an unknown space");
                continue;
            };
            let id = node_ref(&n.space, &n.ty, &n.key);
            let hash = record_hash(&n);
            match self.by_id.get(&id) {
                Some(&ix) => {
                    let d = &mut self.graph[ix];
                    let old = record_hash(&NodeRecord {
                        space: n.space.clone(),
                        ty: d.ty.clone(),
                        key: d.key.clone(),
                        name: d.name.clone(),
                        aliases: d.aliases.clone(),
                        properties: d.properties.clone(),
                    });
                    self.data_sum = self.data_sum.wrapping_sub(old).wrapping_add(hash);
                    d.name = n.name;
                    d.aliases = n.aliases;
                    d.properties = n.properties;
                }
                None => {
                    let ix = self.graph.add_node(NodeData {
                        space,
                        ty: n.ty,
                        key: n.key,
                        name: n.name,
                        aliases: n.aliases,
                        properties: n.properties,
                        id: id.clone(),
                    });
                    self.by_id.insert(id, ix);
                    self.data_sum = self.data_sum.wrapping_add(hash);
                }
            }
        }
        for e in edges {
            let from = self
                .by_id
                .get(&node_ref(&e.space, &e.from_type, &e.from_key));
            let to = self.by_id.get(&node_ref(&e.space, &e.to_type, &e.to_key));
            let (Some(&from), Some(&to)) = (from, to) else {
                tracing::warn!(space = %e.space, relation = %e.ty, "skipping an edge with a missing end");
                continue;
            };
            if self
                .graph
                .edges_connecting(from, to)
                .any(|x| x.weight().ty == e.ty)
            {
                continue;
            }
            self.data_sum = self.data_sum.wrapping_add(record_hash(&e));
            self.graph.add_edge(from, to, EdgeData { ty: e.ty });
        }
    }
}

/// 128 bits of a SHA-256 over a record as JSON.
fn record_hash(record: &impl serde::Serialize) -> u128 {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_vec(record).unwrap_or_default();
    let digest = Sha256::digest(&json);
    u128::from_le_bytes(digest[..16].try_into().expect("16 bytes"))
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
        let mut spaces: Vec<SpaceDef> = spaces;
        spaces.sort_by(|a, b| a.space.cmp(&b.space));
        let space_by_name: HashMap<String, usize> = spaces
            .iter()
            .enumerate()
            .map(|(i, s)| (s.space.clone(), i))
            .collect();
        let mut parts = Parts {
            graph: DiGraph::new(),
            by_id: HashMap::new(),
            data_sum: 0,
        };
        parts.add(&space_by_name, data.nodes, data.edges);
        Self::derive(schemas, spaces, parts)
    }

    /// The indexes over a graph: names for the matcher, numeric keys, type names, the names
    /// for partial matches, and the version.
    fn derive(schemas: Vec<SchemaDef>, spaces: Vec<SpaceDef>, parts: Parts) -> Self {
        let schemas: BTreeMap<String, SchemaDef> =
            schemas.into_iter().map(|s| (s.schema.clone(), s)).collect();
        let spaces: Vec<SpaceEntry> = spaces.into_iter().map(SpaceEntry::new).collect();
        let space_by_name: HashMap<String, usize> = spaces
            .iter()
            .enumerate()
            .map(|(i, s)| (s.def.space.clone(), i))
            .collect();
        let Parts {
            graph,
            by_id,
            data_sum,
        } = parts;
        let mut names: Vec<(String, Target)> = Vec::new();
        let mut numeric_keys: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        let mut all_names = Vec::new();
        for ix in graph.node_indices() {
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
        let mut name_pairs: HashMap<(char, char), Vec<u32>> = HashMap::new();
        for (i, (name, _)) in all_names.iter().enumerate() {
            let chars: Vec<char> = name.chars().collect();
            for w in chars.windows(2) {
                let list = name_pairs.entry((w[0], w[1])).or_default();
                if list.last() != Some(&(i as u32)) {
                    list.push(i as u32);
                }
            }
        }
        let version = version_of(&schemas, &spaces, data_sum);
        Self {
            version,
            schemas,
            spaces,
            space_by_name,
            graph,
            by_id,
            names: NameMatcher::new(names),
            numeric_keys,
            type_names,
            all_names,
            name_pairs,
            data_sum,
        }
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

    /// The same data with other schemas and spaces, after one of them changed; the nodes and
    /// edges of a space no longer listed (deleted with its owner) are left out.
    pub fn with_meta(&self, schemas: Vec<SchemaDef>, spaces: Vec<SpaceDef>) -> Self {
        let mut data = self.data();
        let kept: std::collections::HashSet<&str> =
            spaces.iter().map(|s| s.space.as_str()).collect();
        data.nodes.retain(|n| kept.contains(n.space.as_str()));
        data.edges.retain(|e| kept.contains(e.space.as_str()));
        Self::build(schemas, spaces, data)
    }

    /// The same schemas and spaces without some nodes (and every edge of theirs) and edges:
    /// built again from the records left, which is fine for an approved change, not for every
    /// write.
    pub fn without(&self, nodes: &[def::NodeKey], edges: &[EdgeRecord]) -> Self {
        let gone_nodes: std::collections::HashSet<(&str, &str, &str)> = nodes
            .iter()
            .map(|n| (n.space.as_str(), n.ty.as_str(), n.key.as_str()))
            .collect();
        let gone_edges: std::collections::HashSet<&EdgeRecord> = edges.iter().collect();
        let mut data = self.data();
        data.nodes
            .retain(|n| !gone_nodes.contains(&(n.space.as_str(), n.ty.as_str(), n.key.as_str())));
        data.edges.retain(|e| {
            !gone_edges.contains(e)
                && !gone_nodes.contains(&(
                    e.space.as_str(),
                    e.from_type.as_str(),
                    e.from_key.as_str(),
                ))
                && !gone_nodes.contains(&(e.space.as_str(), e.to_type.as_str(), e.to_key.as_str()))
        });
        Self::build(
            self.schemas.values().cloned().collect(),
            self.spaces.iter().map(|s| s.def.clone()).collect(),
            data,
        )
    }

    /// The same schemas and spaces with nodes and edges added; a node with the type and key of
    /// a stored one in the same space replaces its name, aliases and properties. The graph is
    /// copied and changed, not built again from its records.
    pub fn with_data(&self, nodes: Vec<NodeRecord>, edges: Vec<EdgeRecord>) -> Self {
        let mut parts = Parts {
            graph: self.graph.clone(),
            by_id: self.by_id.clone(),
            data_sum: self.data_sum,
        };
        parts.add(&self.space_by_name, nodes, edges);
        Self::derive(
            self.schemas.values().cloned().collect(),
            self.spaces.iter().map(|s| s.def.clone()).collect(),
            parts,
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
                properties: n.properties.clone(),
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

    /// Whether an edge is stored.
    pub fn has_edge(&self, e: &EdgeRecord) -> bool {
        let (Some(a), Some(b)) = (
            self.node(&def::node_ref(&e.space, &e.from_type, &e.from_key)),
            self.node(&def::node_ref(&e.space, &e.to_type, &e.to_key)),
        ) else {
            return false;
        };
        self.graph
            .edges_connecting(a, b)
            .any(|x| x.weight().ty == e.ty)
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
        // Only names holding every pair of the keyword's characters can hold the keyword; look
        // in those holding its rarest pair.
        let chars: Vec<char> = keyword.chars().collect();
        let mut rarest: Option<&Vec<u32>> = None;
        for w in chars.windows(2) {
            let Some(list) = self.name_pairs.get(&(w[0], w[1])) else {
                return Vec::new();
            };
            if rarest.is_none_or(|r| list.len() < r.len()) {
                rarest = Some(list);
            }
        }
        let mut out = Vec::new();
        for &i in rarest.into_iter().flatten() {
            let (name, ix) = &self.all_names[i as usize];
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

/// 16 hex digits of a SHA-256 over the schemas, the spaces and the sum of the data's record
/// hashes: the same graph has the same version however its records arrived.
fn version_of(
    schemas: &BTreeMap<String, SchemaDef>,
    spaces: &[SpaceEntry],
    data_sum: u128,
) -> String {
    use sha2::{Digest, Sha256};
    let spaces: Vec<&SpaceDef> = spaces.iter().map(|s| &s.def).collect();
    let mut h = Sha256::new();
    for part in [
        serde_json::to_string(schemas),
        serde_json::to_string(&spaces),
    ] {
        h.update(part.unwrap_or_default().as_bytes());
        h.update([0u8]);
    }
    h.update(data_sum.to_le_bytes());
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
            properties: Default::default(),
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
                properties: Default::default(),
            }],
            vec![],
        );
        assert_eq!(more.graph.node_count(), 4);
        let n = more.node("mainland:customer/c_5521").unwrap();
        assert_eq!(more.graph[n].name, "Lin Xiao");
        assert_eq!(more.graph.edge_count(), 2, "edges survive a node update");
        assert_ne!(more.version, g.version);
        // Changed in place or built from its records, the same graph has the same version.
        let rebuilt = GraphSnapshot::build(
            more.schemas.values().cloned().collect(),
            more.spaces.iter().map(|s| s.def.clone()).collect(),
            more.data(),
        );
        assert_eq!(rebuilt.version, more.version);

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
