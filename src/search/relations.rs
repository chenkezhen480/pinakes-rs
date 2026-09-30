//! The relations channel: real edges from the in-memory graph, never summaries.
//!
//! It starts from the entities a search found: nodes named in the query, keywords or `entities`
//! (by name, alias, identifier key, or a number next to a type name such as "order 1042"), and,
//! with less weight, nodes that the returned memory and knowledge hits are anchored to (the
//! lift). The node types and relation types the query names decide which of their edges come
//! first. Edges are ranked in tiers, so a question's own words can't be outranked by
//! neighbourhood noise:
//!
//! 0. the shortest path between two named nodes (only with two or more of them);
//! 1. edges of a named relation type at a named node, and one-hop edges to a node of a named
//!    type;
//! 2. two- and three-hop chains to a node of a named type ("which plan is Lumen on" goes
//!    workspace → subscription → plan);
//! 3. the other edges of named nodes;
//! 4. edges of lifted nodes that have a named relation type or lead to a named type.
//!
//! Only visible nodes start a walk, and edges never cross spaces, so nothing from a hidden space
//! can appear.

use std::collections::HashMap;

use petgraph::graph::{EdgeIndex, NodeIndex};
use serde::Serialize;

use crate::graph::walk::{Direction, bfs_tree, chain, incident, shortest_path};
use crate::graph::{GraphSnapshot, Visible};

/// Deepest chain from a named node to a node of a named type.
const TARGET_DEPTH: usize = 3;
/// Most nodes one such search visits.
const TARGET_VISIT_CAP: usize = 200;
/// Longest path looked for between two named nodes.
const PATH_HOPS: usize = 4;

/// How an edge reached the relations slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeMatch {
    /// On the shortest path between two named nodes.
    Path,
    /// A relation type the query names, at a named node.
    Relation,
    /// On the way from a named node to a node of a type the query names.
    NodeType,
    /// Another edge of a named node.
    Neighbor,
    /// An edge of a node that a returned page is anchored to.
    Anchor,
}

#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub space: String,
    pub from: String,
    pub from_name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub to: String,
    pub to_name: String,
    /// The key the relation joins on, from the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Why a search returned it; absent in walks and paths.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<EdgeMatch>,
}

/// What the relations channel starts from.
pub struct Start<'a> {
    /// Nodes named in the request, visible to the caller, in the order found.
    pub named: &'a [NodeIndex],
    /// Visible nodes the returned hits are anchored to, not already named.
    pub lifted: &'a [NodeIndex],
    /// Node types the request names, as `schema:type`.
    pub node_types: &'a [String],
    /// Relation types the request names, as `schema:relation`.
    pub relation_types: &'a [String],
}

pub struct Found {
    pub edges: Vec<Edge>,
    /// At least one edge touches a node the request named.
    pub strong: bool,
}

/// Collected edges, each with its best rank.
#[derive(Default)]
struct Ranked {
    best: HashMap<EdgeIndex, (u8, usize, usize, EdgeMatch)>,
    seq: usize,
}

impl Ranked {
    fn add(&mut self, e: EdgeIndex, tier: u8, start: usize, how: EdgeMatch) {
        self.seq += 1;
        let rank = (tier, start, self.seq, how);
        let entry = self.best.entry(e).or_insert(rank);
        if (rank.0, rank.1, rank.2) < (entry.0, entry.1, entry.2) {
            *entry = rank;
        }
    }
}

fn split(entity: &str) -> Option<(&str, &str)> {
    entity.split_once(':')
}

/// The relations slot's edges for one search, best first, at most `size`.
pub fn find(g: &GraphSnapshot, visible: &Visible, start: &Start<'_>, size: usize) -> Found {
    let mut ranked = Ranked::default();
    let named: Vec<NodeIndex> = start
        .named
        .iter()
        .copied()
        .filter(|&n| visible.node(g, n))
        .collect();

    // 0. Paths between named nodes of one space.
    for (i, &a) in named.iter().enumerate() {
        for &b in &named[i + 1..] {
            if g.graph[a].space != g.graph[b].space {
                continue;
            }
            if let Some(path) = shortest_path(&g.graph, a, b, |_| true, PATH_HOPS) {
                for e in path {
                    ranked.add(e, 0, i, EdgeMatch::Path);
                }
            }
        }
    }

    let is_named_relation = |e: EdgeIndex, schema: &str| {
        start
            .relation_types
            .iter()
            .filter_map(|r| split(r))
            .any(|(s, r)| s == schema && r == g.graph[e].ty)
    };
    // The named types to look for from a node: those of its schema, except its own type unless
    // the schema relates that type to itself.
    let targets_for = |n: NodeIndex| -> Vec<&str> {
        let schema = g.schema_of(n);
        let own = g.graph[n].ty.as_str();
        start
            .node_types
            .iter()
            .filter_map(|t| split(t))
            .filter(|(s, t)| *s == schema && (*t != own || g.has_self_relation(schema, t)))
            .map(|(_, t)| t)
            .collect()
    };

    // 1–3. Named nodes.
    for (i, &n) in named.iter().enumerate() {
        let schema = g.schema_of(n);
        for (e, _) in incident(&g.graph, n, Direction::Both) {
            if is_named_relation(e, schema) {
                ranked.add(e, 1, i, EdgeMatch::Relation);
            } else {
                ranked.add(e, 3, i, EdgeMatch::Neighbor);
            }
        }
        let targets = targets_for(n);
        if targets.is_empty() {
            continue;
        }
        let tree = bfs_tree(&g.graph, n, TARGET_DEPTH, TARGET_VISIT_CAP);
        for ty in targets {
            // The nearest nodes of the type only, so "which plan" doesn't list every plan.
            let reached: Vec<(NodeIndex, usize)> = tree
                .iter()
                .filter(|(m, d, _)| *d > 0 && g.graph[*m].ty == ty)
                .map(|(m, d, _)| (*m, *d))
                .collect();
            let Some(nearest) = reached.iter().map(|(_, d)| *d).min() else {
                continue;
            };
            for (m, _) in reached.iter().filter(|(_, d)| *d == nearest).take(size) {
                let tier = if nearest == 1 { 1 } else { 2 };
                for e in chain(&tree, *m) {
                    ranked.add(e, tier, i, EdgeMatch::NodeType);
                }
            }
        }
    }

    // 4. Lifted nodes: only edges the request points at.
    let offset = named.len();
    for (i, &n) in start
        .lifted
        .iter()
        .filter(|&&n| visible.node(g, n) && !named.contains(&n))
        .enumerate()
    {
        let schema = g.schema_of(n);
        let targets = targets_for(n);
        for (e, other) in incident(&g.graph, n, Direction::Both) {
            if is_named_relation(e, schema) || targets.contains(&g.graph[other].ty.as_str()) {
                ranked.add(e, 4, offset + i, EdgeMatch::Anchor);
            }
        }
    }

    let mut order: Vec<(EdgeIndex, (u8, usize, usize, EdgeMatch))> =
        ranked.best.into_iter().collect();
    order.sort_by_key(|(_, (tier, start, seq, _))| (*tier, *start, *seq));
    order.truncate(size);
    let strong = order.iter().any(|(_, (tier, ..))| *tier <= 3);
    let edges = order
        .into_iter()
        .map(|(e, (.., how))| edge_out(g, e, Some(how)))
        .collect();
    Found { edges, strong }
}

/// An edge as the response shows it.
pub fn edge_out(g: &GraphSnapshot, e: EdgeIndex, how: Option<EdgeMatch>) -> Edge {
    let (a, b) = g.graph.edge_endpoints(e).expect("edge exists");
    let (from, to) = (&g.graph[a], &g.graph[b]);
    Edge {
        space: g.spaces[from.space].def.space.clone(),
        from: from.id.clone(),
        from_name: from.name.clone(),
        ty: g.graph[e].ty.clone(),
        to: to.id.clone(),
        to_name: to.name.clone(),
        via: g.relation(e).and_then(|r| r.via.clone()),
        matched_by: how,
    }
}
