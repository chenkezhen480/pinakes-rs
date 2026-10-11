//! Browsing the graph, for people and host code (the console): `GET /graph/schemas`,
//! `GET /graph/spaces` and `GET /graph/nodes`. Agents find nodes through `/search`; these list
//! what a caller may see, as `/pages` lists pages (design, "M10": confirmed 2026-10-10). Only
//! visible spaces are listed, and a hidden space is 404. Read from the in-memory graph.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use super::graph::{NodeOut, node_out};
use crate::AppState;
use crate::error::ApiError;
use crate::graph::def::{Members, SchemaDef, Visibility};
use crate::identity::Scope;

/// Nodes a page of `/graph/nodes` holds when the caller doesn't say, and at most.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SchemaOut {
    #[serde(flatten)]
    pub def: SchemaDef,
    /// Its description in the wiki (`graph/<schema>`), written by the librarian.
    pub page: String,
    /// Spaces under it the caller may see.
    pub spaces: Vec<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SchemaList {
    pub schemas: Vec<SchemaOut>,
}

/// Every schema, with the spaces under it the caller may see.
#[tracing::instrument(name = "list_schemas", skip_all)]
pub async fn schemas(scope: Scope) -> Result<Json<SchemaList>, ApiError> {
    let g = scope.tenant().graph().await?;
    let vis = g.visible(&scope);
    Ok(Json(SchemaList {
        schemas: g
            .schemas
            .values()
            .map(|def| SchemaOut {
                page: format!("graph/{}", def.schema),
                spaces: g
                    .spaces
                    .iter()
                    .enumerate()
                    .filter(|(i, s)| vis.space(*i) && s.def.schema == def.schema)
                    .map(|(_, s)| s.def.space.clone())
                    .collect(),
                def: def.clone(),
            })
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SpaceOut {
    pub space: String,
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub visibility: Visibility,
    /// Whether the caller created it.
    pub mine: bool,
    /// Who else may see a restricted space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Members>,
    /// Nodes per type.
    pub nodes: std::collections::BTreeMap<String, usize>,
    pub edges: usize,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SpaceList {
    pub spaces: Vec<SpaceOut>,
}

/// The spaces the caller may see, with how many nodes of each type and edges they hold.
#[tracing::instrument(name = "list_spaces", skip_all)]
pub async fn spaces(scope: Scope) -> Result<Json<SpaceList>, ApiError> {
    let g = scope.tenant().graph().await?;
    let vis = g.visible(&scope);
    let mut counts = vec![(std::collections::BTreeMap::new(), 0usize); g.spaces.len()];
    for n in g.graph.node_weights() {
        *counts[n.space].0.entry(n.ty.clone()).or_insert(0) += 1;
    }
    for e in g.graph.edge_indices() {
        if let Some((a, _)) = g.graph.edge_endpoints(e) {
            counts[g.graph[a].space].1 += 1;
        }
    }
    let me = scope.user().map(|u| u.dir.as_str());
    Ok(Json(SpaceList {
        spaces: g
            .spaces
            .iter()
            .zip(counts)
            .enumerate()
            .filter(|(i, _)| vis.space(*i))
            .map(|(_, (s, (nodes, edges)))| SpaceOut {
                space: s.def.space.clone(),
                schema: s.def.schema.clone(),
                name: s.def.name.clone(),
                aliases: s.def.aliases.clone(),
                visibility: s.def.visibility,
                mine: me.is_some() && me == s.owner_dir.as_deref(),
                members: (s.def.visibility == Visibility::Restricted)
                    .then(|| s.def.members.clone()),
                nodes,
                edges,
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NodesQuery {
    pub space: String,
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
    /// Part of a node's name, alias or key, any case.
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeList {
    pub nodes: Vec<NodeOut>,
    /// Nodes matching, over all pages.
    pub total: usize,
}

/// A space's nodes, of one type or all, filtered by part of a name, alias or key; by name.
#[tracing::instrument(name = "list_nodes", skip_all)]
pub async fn nodes(
    scope: Scope,
    State(_app): State<AppState>,
    query: Result<Query<NodesQuery>, QueryRejection>,
) -> Result<Json<NodeList>, ApiError> {
    let Query(q) = query?;
    let g = scope.tenant().graph().await?;
    let vis = g.visible(&scope);
    let space = g
        .space_index(&q.space)
        .filter(|&i| vis.space(i))
        .ok_or(ApiError::NotFound)?;
    let needle =
        q.q.as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
    let mut found: Vec<_> = g
        .graph
        .node_indices()
        .filter(|&n| {
            let d = &g.graph[n];
            d.space == space
                && q.ty.as_deref().is_none_or(|t| d.ty == t)
                && needle.as_deref().is_none_or(|s| {
                    d.name.to_lowercase().contains(s)
                        || d.key.to_lowercase().contains(s)
                        || d.aliases.iter().any(|a| a.to_lowercase().contains(s))
                })
        })
        .collect();
    found.sort_by(|a, b| {
        let (a, b) = (&g.graph[*a], &g.graph[*b]);
        (&a.ty, &a.name, &a.key).cmp(&(&b.ty, &b.name, &b.key))
    });
    let total = found.len();
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let nodes = found
        .into_iter()
        .skip(q.offset.unwrap_or(0))
        .take(limit)
        .map(|n| node_out(&g, n, 0))
        .collect();
    Ok(Json(NodeList { nodes, total }))
}
