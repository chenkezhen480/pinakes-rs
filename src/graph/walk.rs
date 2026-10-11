//! Breadth-first walks and shortest paths over the in-memory graph. Edges never cross spaces,
//! so a walk from a node stays in its space.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::Direction as PgDir;
use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};

/// Which edges a walk follows, seen from the node it stands on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Out,
    In,
    #[default]
    Both,
}

/// Edges of `n` in a direction, with the node at their other end, in a stable order.
pub fn incident<N, E>(
    g: &DiGraph<N, E>,
    n: NodeIndex,
    dir: Direction,
) -> Vec<(EdgeIndex, NodeIndex)> {
    let mut out = Vec::new();
    if matches!(dir, Direction::Out | Direction::Both) {
        out.extend(
            g.edges_directed(n, PgDir::Outgoing)
                .map(|e| (e.id(), e.target())),
        );
    }
    if matches!(dir, Direction::In | Direction::Both) {
        out.extend(
            g.edges_directed(n, PgDir::Incoming)
                .map(|e| (e.id(), e.source())),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// What a walk reached.
#[derive(Debug, Default)]
pub struct Walk {
    /// Nodes with their depth, in the order they were reached; the start is first, at 0.
    pub nodes: Vec<(NodeIndex, usize)>,
    /// Edges between reached nodes that the walk followed, in the order it met them.
    pub edges: Vec<EdgeIndex>,
    /// Stopped at `max_nodes` with more to see.
    pub truncated: bool,
}

/// Walks breadth-first from `start` up to `max_depth` edges away along the edges `follow`
/// accepts, returning at most `max_nodes` nodes.
pub fn walk<N, E>(
    g: &DiGraph<N, E>,
    start: NodeIndex,
    max_depth: usize,
    dir: Direction,
    follow: impl Fn(EdgeIndex) -> bool,
    max_nodes: usize,
) -> Walk {
    let mut out = Walk::default();
    let mut depth_of: HashMap<NodeIndex, usize> = HashMap::from([(start, 0)]);
    let mut seen_edges: HashSet<EdgeIndex> = HashSet::new();
    let mut queue = VecDeque::from([start]);
    out.nodes.push((start, 0));
    while let Some(u) = queue.pop_front() {
        let d = depth_of[&u];
        if d >= max_depth {
            continue;
        }
        for (e, v) in incident(g, u, dir) {
            if !follow(e) {
                continue;
            }
            if let Entry::Vacant(slot) = depth_of.entry(v) {
                if out.nodes.len() >= max_nodes.max(1) {
                    out.truncated = true;
                    continue;
                }
                slot.insert(d + 1);
                out.nodes.push((v, d + 1));
                queue.push_back(v);
            }
            if seen_edges.insert(e) {
                out.edges.push(e);
            }
        }
    }
    out
}

/// One node of a [`bfs_tree`]: the node, its depth, and the edge to its parent with the parent.
pub type TreeNode = (NodeIndex, usize, Option<(EdgeIndex, NodeIndex)>);

/// A breadth-first tree from `start` in both directions: each reached node with its depth and
/// the edge to its parent, in the order reached. Stops at `max_depth` and `max_nodes`.
pub fn bfs_tree<N, E>(
    g: &DiGraph<N, E>,
    start: NodeIndex,
    max_depth: usize,
    max_nodes: usize,
) -> Vec<TreeNode> {
    let mut out = vec![(start, 0, None)];
    let mut seen: HashSet<NodeIndex> = HashSet::from([start]);
    let mut i = 0;
    while i < out.len() {
        let (u, d, _) = out[i];
        i += 1;
        if d >= max_depth {
            continue;
        }
        for (e, v) in incident(g, u, Direction::Both) {
            if out.len() >= max_nodes {
                return out;
            }
            if seen.insert(v) {
                out.push((v, d + 1, Some((e, u))));
            }
        }
    }
    out
}

/// The edges from the root of a [`bfs_tree`] to one of its nodes, root first.
pub fn chain(tree: &[TreeNode], to: NodeIndex) -> Vec<EdgeIndex> {
    let parent: HashMap<NodeIndex, (EdgeIndex, NodeIndex)> = tree
        .iter()
        .filter_map(|(n, _, p)| p.map(|p| (*n, p)))
        .collect();
    let mut out = Vec::new();
    let mut at = to;
    while let Some(&(e, up)) = parent.get(&at) {
        out.push(e);
        at = up;
    }
    out.reverse();
    out
}

/// Most nodes a shortest-path search visits before giving up.
const PATH_VISIT_CAP: usize = 100_000;

/// The shortest chain of edges between two nodes, in either direction along each edge, using
/// only edges `follow` accepts and at most `max_hops` of them. `from == to` gives an empty
/// chain.
pub fn shortest_path<N, E>(
    g: &DiGraph<N, E>,
    from: NodeIndex,
    to: NodeIndex,
    follow: impl Fn(EdgeIndex) -> bool,
    max_hops: usize,
) -> Option<Vec<EdgeIndex>> {
    if from == to {
        return Some(Vec::new());
    }
    let mut parent: HashMap<NodeIndex, (EdgeIndex, NodeIndex)> = HashMap::new();
    let mut depth: HashMap<NodeIndex, usize> = HashMap::from([(from, 0)]);
    let mut queue = VecDeque::from([from]);
    while let Some(u) = queue.pop_front() {
        let d = depth[&u];
        if d >= max_hops || depth.len() > PATH_VISIT_CAP {
            continue;
        }
        for (e, v) in incident(g, u, Direction::Both) {
            if !follow(e) || depth.contains_key(&v) {
                continue;
            }
            depth.insert(v, d + 1);
            parent.insert(v, (e, u));
            if v == to {
                let mut out = Vec::new();
                let mut at = to;
                while let Some(&(e, up)) = parent.get(&at) {
                    out.push(e);
                    at = up;
                }
                out.reverse();
                return Some(out);
            }
            queue.push_back(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a -> b -> c -> d, a -> e, x (alone)
    fn sample() -> (DiGraph<&'static str, &'static str>, Vec<NodeIndex>) {
        let mut g = DiGraph::new();
        let n: Vec<NodeIndex> = ["a", "b", "c", "d", "e", "x"]
            .iter()
            .map(|s| g.add_node(*s))
            .collect();
        g.add_edge(n[0], n[1], "ab");
        g.add_edge(n[1], n[2], "bc");
        g.add_edge(n[2], n[3], "cd");
        g.add_edge(n[0], n[4], "ae");
        (g, n)
    }

    #[test]
    fn walks_respect_depth_direction_and_caps() {
        let (g, n) = sample();
        let w = walk(&g, n[1], 1, Direction::Both, |_| true, 100);
        let reached: Vec<_> = w.nodes.iter().map(|(i, d)| (g[*i], *d)).collect();
        assert_eq!(reached, vec![("b", 0), ("a", 1), ("c", 1)]);
        assert_eq!(w.edges.len(), 2);
        let out = walk(&g, n[1], 3, Direction::Out, |_| true, 100);
        assert_eq!(out.nodes.len(), 3, "b, c, d");
        let inward = walk(&g, n[1], 3, Direction::In, |_| true, 100);
        assert_eq!(inward.nodes.len(), 2, "b, a");
        let capped = walk(&g, n[0], 3, Direction::Both, |_| true, 2);
        assert!(capped.truncated);
        assert_eq!(capped.nodes.len(), 2);
        let filtered = walk(&g, n[0], 3, Direction::Both, |e| g[e] != "ab", 100);
        assert_eq!(filtered.nodes.len(), 2, "a, e");
    }

    #[test]
    fn paths_and_trees() {
        let (g, n) = sample();
        let p = shortest_path(&g, n[3], n[4], |_| true, 6).unwrap();
        let labels: Vec<_> = p.iter().map(|e| g[*e]).collect();
        assert_eq!(labels, vec!["cd", "bc", "ab", "ae"]);
        assert!(shortest_path(&g, n[3], n[4], |_| true, 3).is_none());
        assert!(shortest_path(&g, n[0], n[5], |_| true, 6).is_none());
        assert_eq!(shortest_path(&g, n[0], n[0], |_| true, 6), Some(vec![]));

        let tree = bfs_tree(&g, n[0], 2, 100);
        assert_eq!(tree.len(), 4, "a, b, e, c");
        let to_c: Vec<_> = chain(&tree, n[2]).iter().map(|e| g[*e]).collect();
        assert_eq!(to_c, vec!["ab", "bc"]);
    }
}
