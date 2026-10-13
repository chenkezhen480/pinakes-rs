//! `POST /graph` and `POST /graph/path`: follow-ups after `/search`, over the in-memory graph.
//! They take node ids as search returns them (`space:type/key`); a node that doesn't exist and
//! a node in a space the caller can't see are both 404. Edges never cross spaces, so a walk
//! from a visible node only ever reaches its own space. No LLM, no Neo4j.

use std::collections::HashSet;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use petgraph::graph::{EdgeIndex, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::graph::walk::{self, Direction};
use crate::graph::{EntityRef, GraphSnapshot, Visible};
use crate::identity::Scope;
use crate::kinds::Kind;
use crate::search::relations::{Edge, edge_out};
use crate::search::tracker::ReadState;
use crate::search::{SearchSnapshot, validate_session};
use crate::trace::{Endpoint, TraceRecord};

/// Nodes a walk returns when the caller doesn't say.
const DEFAULT_MAX_NODES: usize = 25;
/// Pages anchored to the walked nodes, at most.
const MAX_PAGES: usize = 20;
/// Longest path `/graph/path` looks for.
const MAX_PATH_HOPS: usize = 6;
/// Most relation types a caller may filter on.
const MAX_RELATIONS: usize = 50;

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GraphRequest {
    pub node: String,
    #[serde(default)]
    pub depth: Option<usize>,
    #[serde(default)]
    pub direction: Direction,
    /// Relation types to follow (`placed_by` or `commerce:placed_by`); all when empty.
    #[serde(default)]
    pub relations: Vec<String>,
    #[serde(default)]
    pub max_nodes: Option<usize>,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeOut {
    pub id: String,
    pub space: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub key: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// Values of the properties its type declares.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub properties: std::collections::BTreeMap<String, serde_json::Value>,
    /// Edges from the node the walk started at.
    pub depth: usize,
}

/// A page anchored to a walked node, or to the start node's type.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AnchoredPage {
    pub page: String,
    pub kind: Kind,
    pub title: String,
    pub summary: String,
    /// The node or node type it is anchored to.
    pub anchored_to: String,
    pub read: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct GraphResponse {
    pub node: NodeOut,
    /// Every node reached, the start first.
    pub nodes: Vec<NodeOut>,
    pub edges: Vec<Edge>,
    pub pages: Vec<AnchoredPage>,
    /// The walk stopped at `max_nodes` with more to see.
    pub truncated: bool,
}

pub(crate) fn node_out(g: &GraphSnapshot, n: NodeIndex, depth: usize) -> NodeOut {
    let d = &g.graph[n];
    NodeOut {
        id: d.id.clone(),
        space: g.spaces[d.space].def.space.clone(),
        ty: d.ty.clone(),
        key: d.key.clone(),
        name: d.name.clone(),
        aliases: d.aliases.clone(),
        properties: d.properties.clone(),
        depth,
    }
}

/// A node the caller may see, by its id. Malformed ids are 400; unknown and hidden ones 404.
fn visible_node(g: &GraphSnapshot, vis: &Visible, id: &str) -> Result<NodeIndex, ApiError> {
    let id = id.trim();
    if !matches!(EntityRef::parse(id), Some(EntityRef::Node { .. })) {
        return Err(ApiError::BadRequest(format!(
            "{id:?} is not a node id; use space:type/key as /search returns it"
        )));
    }
    g.node(id)
        .filter(|&n| vis.node(g, n))
        .ok_or(ApiError::NotFound)
}

/// The relation names to follow from a node's schema; `None` follows all.
fn relation_filter(
    g: &GraphSnapshot,
    start: NodeIndex,
    relations: &[String],
) -> Result<Option<HashSet<String>>, ApiError> {
    if relations.is_empty() {
        return Ok(None);
    }
    if relations.len() > MAX_RELATIONS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_RELATIONS} relation types"
        )));
    }
    let schema_name = g.schema_of(start);
    let schema = g.schemas.get(schema_name);
    let mut out = HashSet::new();
    for r in relations {
        let name = match r.split_once(':') {
            Some((s, name)) if s == schema_name => name,
            Some(_) => {
                return Err(ApiError::BadRequest(format!(
                    "relation {r:?} is not in schema {schema_name}"
                )));
            }
            None => r.as_str(),
        };
        if !schema.is_some_and(|s| s.relations.contains_key(name)) {
            return Err(ApiError::BadRequest(format!(
                "relation {r:?} is not in schema {schema_name}"
            )));
        }
        out.insert(name.to_owned());
    }
    Ok(Some(out))
}

fn follows<'a>(
    g: &'a GraphSnapshot,
    filter: &'a Option<HashSet<String>>,
) -> impl Fn(EdgeIndex) -> bool + 'a {
    move |e| filter.as_ref().is_none_or(|f| f.contains(&g.graph[e].ty))
}

/// Pages anchored to the walked nodes (and to the start node's type) that the caller may see.
fn anchored_pages(
    snap: &SearchSnapshot,
    g: &GraphSnapshot,
    scope: &Scope,
    nodes: &[(NodeIndex, usize)],
    reads: &ReadState,
) -> Vec<AnchoredPage> {
    let mut entities: Vec<String> = nodes.iter().map(|(n, _)| g.graph[*n].id.clone()).collect();
    if let Some(&(start, _)) = nodes.first() {
        entities.push(format!("{}:{}", g.schema_of(start), g.graph[start].ty));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entity in entities {
        for &ord in snap.catalog.anchored_to(&entity) {
            let page = &snap.catalog.pages[ord];
            if !scope.can_see(&page.path) || !seen.insert(ord) {
                continue;
            }
            if out.len() >= MAX_PAGES {
                return out;
            }
            let path = page.path.as_str().to_owned();
            out.push(AnchoredPage {
                read: reads.has_read(&path, None),
                page: path,
                kind: page.kind,
                title: page.title.clone(),
                summary: page.summary.clone(),
                anchored_to: entity.clone(),
            });
        }
    }
    out
}

#[tracing::instrument(name = "graph", skip_all)]
pub async fn graph(
    State(app): State<AppState>,
    scope: Scope,
    body: Result<Json<GraphRequest>, JsonRejection>,
) -> Result<Json<GraphResponse>, ApiError> {
    let started = Instant::now();
    let Json(req) = body?;
    let cfg = &app.config.graph;
    let depth = req.depth.unwrap_or(1);
    if !(1..=cfg.max_depth).contains(&depth) {
        return Err(ApiError::BadRequest(format!(
            "depth is 1 to {}",
            cfg.max_depth
        )));
    }
    let max_nodes = req
        .max_nodes
        .unwrap_or(DEFAULT_MAX_NODES.min(cfg.max_nodes));
    if !(1..=cfg.max_nodes).contains(&max_nodes) {
        return Err(ApiError::BadRequest(format!(
            "max_nodes is 1 to {}",
            cfg.max_nodes
        )));
    }
    if let Some(s) = &req.session {
        validate_session(s).map_err(ApiError::BadRequest)?;
    }
    let tenant = scope.tenant();
    let g = tenant.graph().await?;
    let vis = g.visible(&scope);
    let start = visible_node(&g, &vis, &req.node)?;
    let filter = relation_filter(&g, start, &req.relations)?;

    let found = tracing::info_span!("walk").in_scope(|| {
        walk::walk(
            &g.graph,
            start,
            depth,
            req.direction,
            follows(&g, &filter),
            max_nodes,
        )
    });
    let snap = tenant.snapshot().await?;
    let reads = scope.read_state(req.session.as_deref());
    let pages = anchored_pages(&snap, &g, &scope, &found.nodes, &reads);
    let response = GraphResponse {
        node: node_out(&g, start, 0),
        nodes: found
            .nodes
            .iter()
            .map(|&(n, d)| node_out(&g, n, d))
            .collect(),
        edges: found.edges.iter().map(|&e| edge_out(&g, e, None)).collect(),
        pages,
        truncated: found.truncated,
    };
    let mut record = trace_record(&app, &scope, Endpoint::Graph, &g, &req, started);
    record.snapshot = Some(snap.version.clone());
    record.result = serde_json::json!({
        "nodes": response.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
        "edges": response.edges.len(),
        "pages": response.pages.iter().map(|p| &p.page).collect::<Vec<_>>(),
        "truncated": response.truncated,
    });
    app.traces.record(record);
    Ok(Json(response))
}

/// A follow-up's trace line: who asked what, on which graph, and how long it took. Only
/// answered calls leave one, as with `/read`.
fn trace_record(
    app: &AppState,
    scope: &Scope,
    endpoint: Endpoint,
    g: &GraphSnapshot,
    req: &impl Serialize,
    started: Instant,
) -> TraceRecord {
    let mut record = scope.trace_record(endpoint);
    record.graph = Some(g.version.clone());
    record.search_config = app.search_fingerprint().to_owned();
    record.request = serde_json::to_value(req).unwrap_or_default();
    record.timings_us.insert(
        "total".into(),
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
    record
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PathRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub relations: Vec<String>,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PathResponse {
    /// A chain of at most `MAX_PATH_HOPS` edges exists. Nodes of two spaces are never joined.
    pub found: bool,
    /// The path's nodes from `from` to `to`, with their distance from `from`.
    pub nodes: Vec<NodeOut>,
    /// The edges in path order; each keeps its own direction.
    pub edges: Vec<Edge>,
}

#[tracing::instrument(name = "graph_path", skip_all)]
pub async fn path(
    State(app): State<AppState>,
    scope: Scope,
    body: Result<Json<PathRequest>, JsonRejection>,
) -> Result<Json<PathResponse>, ApiError> {
    let started = Instant::now();
    let Json(req) = body?;
    if let Some(s) = &req.session {
        validate_session(s).map_err(ApiError::BadRequest)?;
    }
    let g = scope.tenant().graph().await?;
    let vis = g.visible(&scope);
    let from = visible_node(&g, &vis, &req.from)?;
    let to = visible_node(&g, &vis, &req.to)?;
    let filter = relation_filter(&g, from, &req.relations)?;
    let chain = tracing::info_span!("shortest_path").in_scope(|| {
        if g.graph[from].space != g.graph[to].space {
            return None;
        }
        walk::shortest_path(&g.graph, from, to, follows(&g, &filter), MAX_PATH_HOPS)
    });
    let response = match chain {
        None => PathResponse {
            found: false,
            nodes: Vec::new(),
            edges: Vec::new(),
        },
        Some(chain) => {
            let mut nodes = vec![node_out(&g, from, 0)];
            let mut at = from;
            for (i, &e) in chain.iter().enumerate() {
                let (a, b) = g.graph.edge_endpoints(e).expect("edge exists");
                at = if a == at { b } else { a };
                nodes.push(node_out(&g, at, i + 1));
            }
            PathResponse {
                found: true,
                nodes,
                edges: chain.iter().map(|&e| edge_out(&g, e, None)).collect(),
            }
        }
    };
    let mut record = trace_record(&app, &scope, Endpoint::Path, &g, &req, started);
    record.result = serde_json::json!({
        "found": response.found,
        "nodes": response.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
    });
    app.traces.record(record);
    Ok(Json(response))
}
