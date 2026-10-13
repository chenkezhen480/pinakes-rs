//! The graph insert entry points: `/ingest/graph/schema`, `/ingest/graph/space` and
//! `/ingest/graph/data`. Code checks everything against the stored schema. Only a schema writes
//! the wiki (its description, `graph/<schema>.md`), and only a schema involves the librarian;
//! spaces and data never do. Each tenant applies graph writes one at a time, and searches keep reading the previous
//! in-memory graph until the new one is swapped in.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;

use crate::AppState;
use crate::error::ApiError;
use crate::graph::def::{
    self, DataRequest, Members, SchemaDef, SpaceAccess, SpaceDef, SpaceRequest, Visibility,
};
use crate::graph::{GraphSnapshot, describe};
use crate::identity::{Scope, is_admin};
use crate::librarian::DescribeError;
use crate::wiki::WikiError;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SchemaResponse {
    pub schema: String,
    /// The schema's wiki description.
    pub page: String,
    /// A new schema, rather than a change to an existing one.
    pub created: bool,
}

/// `POST /ingest/graph/schema`: create a schema, or change one additively. The librarian
/// writes its description first; then the definition (the authoritative copy) and the
/// description page are stored together. If the model can't be reached (502), the verifier
/// rejects the description twice (422), or storing fails, nothing is stored and a change
/// leaves the schema as it was.
#[tracing::instrument(name = "ingest_graph_schema", skip_all)]
pub async fn schema(
    State(app): State<AppState>,
    scope: Scope,
    body: Result<Json<SchemaDef>, JsonRejection>,
) -> Result<Json<SchemaResponse>, ApiError> {
    let Json(def) = body?;
    def.validate().map_err(ApiError::BadRequest)?;
    let tenant = scope.tenant();
    // The librarian is called without the graph writer lock, which would hold back every
    // graph write of the tenant for as long as the model takes; under the lock the schema is
    // checked to be still as it was described.
    let graph = tenant.graph().await?;
    let (stored, created) = schema_to_store(&graph, &def)?;
    let before_described = graph.schemas.get(&def.schema).cloned();

    let wiki = tenant.wiki();
    let path = describe::page_path(&stored.schema)?;
    let current = {
        let (wiki, path) = (wiki.clone(), path.clone());
        tokio::task::spawn_blocking(move || match wiki.read(&path) {
            Ok(page) => Ok(Some(page)),
            // A missing or broken page gets a new description.
            Err(WikiError::NotFound | WikiError::Malformed { .. }) => Ok(None),
            Err(e) => Err(e),
        })
        .await
        .map_err(anyhow::Error::from)??
    };
    // The same definition again, already described: nothing to do, and no model call, so a
    // host can send its schemas at every start.
    if !created && before_described.as_ref() == Some(&stored) && current.is_some() {
        return Ok(Json(SchemaResponse {
            page: path.to_string(),
            schema: stored.schema,
            created: false,
        }));
    }
    let librarian = app.librarian()?;
    let page = librarian
        .describe(&tenant.dir, &stored, current.as_ref())
        .await
        .map_err(|e| match e {
            DescribeError::Llm(_) => ApiError::BadGateway(e.to_string()),
            DescribeError::Rejected(_) => ApiError::Unprocessable(e.to_string()),
        })?;

    let _writer = tenant.lock_graph().await?;
    let graph = tenant.graph().await?;
    if graph.schemas.get(&def.schema) != before_described.as_ref() {
        return Err(ApiError::Conflict(format!(
            "schema {:?} changed while it was being described; send it again",
            def.schema
        )));
    }
    schema_to_store(&graph, &def)?;

    // The page and the definition are stored together: if the definition can't be stored,
    // the page goes back to what it was.
    let _wiki_writer = tenant.lock_wiki().await?;
    let before = {
        let (wiki, path, page) = (wiki.clone(), path.clone(), page.clone());
        tokio::task::spawn_blocking(move || -> Result<Option<String>, WikiError> {
            let before = wiki.read_text(&path)?;
            wiki.write(&page)?;
            Ok(before)
        })
        .await
        .map_err(anyhow::Error::from)??
    };
    if let Err(e) = tenant.store.put_graph_schema(&stored).await {
        let restored = tokio::task::spawn_blocking(move || wiki.restore_text(&path, before)).await;
        if !matches!(restored, Ok(Ok(()))) {
            tracing::error!(schema = %stored.schema, "could not put the description page back");
        }
        return Err(e.into());
    }
    tenant
        .store
        .audit(
            &scope.actor(),
            if created {
                "graph.schema.create"
            } else {
                "graph.schema.change"
            },
            &stored.schema,
        )
        .await?;
    let mut schemas: Vec<SchemaDef> = graph
        .schemas
        .values()
        .filter(|s| s.schema != stored.schema)
        .cloned()
        .collect();
    schemas.push(stored.clone());
    let spaces = graph.spaces.iter().map(|s| s.def.clone()).collect();
    let next = GraphSnapshot::build_blocking(graph, move |g| g.with_meta(schemas, spaces)).await?;
    tenant.set_graph(next);
    tenant.reindex().await?;
    crate::librarian::write_index(tenant).await?;
    crate::librarian::record_log(tenant, &format!("describe {}", page.path)).await?;
    tracing::info!(schema = %stored.schema, created, "graph schema stored");
    Ok(Json(SchemaResponse {
        page: page.path.to_string(),
        schema: stored.schema,
        created,
    }))
}

/// The definition a schema request stores, and whether it creates the schema: the request as
/// it is for a new schema, or its change applied to the stored one.
fn schema_to_store(graph: &GraphSnapshot, def: &SchemaDef) -> Result<(SchemaDef, bool), ApiError> {
    if graph.space(&def.schema).is_some() {
        return Err(ApiError::Conflict(format!(
            "{:?} is already a space name; schema and space names must differ",
            def.schema
        )));
    }
    match graph.schemas.get(&def.schema) {
        Some(old) => Ok((
            def.apply_change(old, graph).map_err(ApiError::BadRequest)?,
            false,
        )),
        None => Ok((def.clone(), true)),
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SpaceResponse {
    pub space: String,
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub visibility: Visibility,
    /// Who else sees a restricted space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Members>,
}

impl From<SpaceDef> for SpaceResponse {
    fn from(def: SpaceDef) -> Self {
        Self {
            members: (def.visibility == Visibility::Restricted).then_some(def.members),
            space: def.space,
            schema: def.schema,
            name: def.name,
            visibility: def.visibility,
        }
    }
}

/// `POST /ingest/graph/space`: register a space under a schema with its visibility. The owner
/// is the calling user; no wiki page is written.
#[tracing::instrument(name = "ingest_graph_space", skip_all)]
pub async fn space(
    State(_app): State<AppState>,
    scope: Scope,
    body: Result<Json<SpaceRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<SpaceResponse>), ApiError> {
    let Json(req) = body?;
    let owner = scope.user().map(|u| u.id.as_str());
    let def = req.into_def(owner).map_err(ApiError::BadRequest)?;
    let tenant = scope.tenant();
    let _writer = tenant.lock_graph().await?;
    let graph = tenant.graph().await?;
    if !graph.schemas.contains_key(&def.schema) {
        return Err(ApiError::BadRequest(format!(
            "unknown schema {:?}; create it with /ingest/graph/schema first",
            def.schema
        )));
    }
    if graph.schemas.contains_key(&def.space) {
        return Err(ApiError::Conflict(format!(
            "{:?} is a schema name; schema and space names must differ",
            def.space
        )));
    }
    // Space names are unique in a tenant, hidden ones included.
    if graph.space(&def.space).is_some() {
        return Err(ApiError::Conflict(format!(
            "space {:?} already exists",
            def.space
        )));
    }
    tenant.graph_store.prepare_space(&def.space).await?;
    tenant.store.add_graph_space(&def).await?;
    let schemas = graph.schemas.values().cloned().collect();
    let mut spaces: Vec<_> = graph.spaces.iter().map(|s| s.def.clone()).collect();
    spaces.push(def.clone());
    let next = GraphSnapshot::build_blocking(graph, move |g| g.with_meta(schemas, spaces)).await?;
    tenant.set_graph(next);
    tenant
        .store
        .audit(&scope.actor(), "graph.space.create", &def.space)
        .await?;
    tracing::info!(space = %def.space, schema = %def.schema, "graph space created");
    Ok((StatusCode::CREATED, Json(def.into())))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SpaceDeleted {
    pub space: String,
    pub nodes: usize,
    pub edges: usize,
}

/// `DELETE /graph/spaces/{space}`: the admin (the console) deletes a space with every node and
/// edge in it, and its pending draft. Anyone else proposes it (`delete_space` in a graph draft)
/// for review; a space they see is forbidden here, one they don't is not found. Pages anchored
/// to its nodes are left as they are: the read path drops anchors to nodes that are gone, and
/// lint reports them.
#[tracing::instrument(name = "graph_space_delete", skip_all)]
pub async fn delete_space(
    State(app): State<AppState>,
    headers: HeaderMap,
    scope: Scope,
    name: Result<Path<String>, PathRejection>,
) -> Result<Json<SpaceDeleted>, ApiError> {
    let Path(name) = name?;
    let admin = is_admin(&headers, &app);
    let tenant = scope.tenant();
    let _writer = tenant.lock_graph().await?;
    let graph = tenant.graph().await?;
    let Some(space) = graph.space(&name).filter(|s| admin || s.visible_to(&scope)) else {
        return Err(ApiError::NotFound);
    };
    if !admin {
        return Err(ApiError::Forbidden(
            "only the admin deletes a space; propose it (delete_space in a graph draft) for review"
                .into(),
        ));
    }
    let index = graph
        .spaces
        .iter()
        .position(|s| s.def.space == space.def.space)
        .unwrap_or(usize::MAX);
    let nodes = graph
        .graph
        .node_weights()
        .filter(|n| n.space == index)
        .count();
    let edges = graph
        .graph
        .edge_indices()
        .filter(|&e| {
            graph
                .graph
                .edge_endpoints(e)
                .is_some_and(|(a, _)| graph.graph[a].space == index)
        })
        .count();
    drop(graph);
    for p in tenant.store.held_proposals().await? {
        if p.graph.as_ref().is_some_and(|g| g.space == name) {
            tenant.store.delete_proposal(&p.id).await?;
        }
    }
    remove_space(tenant, &name).await?;
    tenant
        .store
        .audit(&scope.actor(), "graph.space.delete", &name)
        .await?;
    Ok(Json(SpaceDeleted {
        space: name,
        nodes,
        edges,
    }))
}

/// Deletes a space from the graph store and the registry, and the in-memory graph follows
/// (dropped, to be loaded again, if it can't be rebuilt). Callers hold the graph writer.
pub(crate) async fn remove_space(
    tenant: &crate::tenants::Tenant,
    name: &str,
) -> anyhow::Result<()> {
    tenant.graph_store.delete_space(name).await?;
    tenant.store.remove_graph_space(name).await?;
    if tenant.graph_after_deletion().await.is_err() {
        tenant.forget_graph().await;
    }
    tracing::info!(space = name, "graph space deleted");
    Ok(())
}

/// `PUT /graph/spaces/{space}`: change who sees a space, its visibility and members set whole.
/// Only its owner may, or the admin (the console), who may change any space of the tenant. A
/// space the caller can't see is not found; one it sees but doesn't own is forbidden. Pages
/// anchored to its nodes stay as they are: the read path drops anchors their readers can no
/// longer see, and lint reports the ones that break their page's audience.
#[tracing::instrument(name = "graph_space_access", skip_all)]
pub async fn space_access(
    State(app): State<AppState>,
    headers: HeaderMap,
    scope: Scope,
    name: Result<Path<String>, PathRejection>,
    body: Result<Json<SpaceAccess>, JsonRejection>,
) -> Result<Json<SpaceResponse>, ApiError> {
    let Path(name) = name?;
    let Json(access) = body?;
    let admin = is_admin(&headers, &app);
    let tenant = scope.tenant();
    let _writer = tenant.lock_graph().await?;
    let graph = tenant.graph().await?;
    let Some(space) = graph.space(&name).filter(|s| admin || s.visible_to(&scope)) else {
        return Err(ApiError::NotFound);
    };
    let me = scope.user().map(|u| u.id.as_str());
    if !admin && (me.is_none() || space.def.owner.as_deref() != me) {
        return Err(ApiError::Forbidden(
            "only the space's owner, or the admin, changes who sees it".into(),
        ));
    }
    let next = access.applied(&space.def).map_err(ApiError::BadRequest)?;
    tenant.store.put_graph_space(&next).await?;
    let schemas = graph.schemas.values().cloned().collect();
    let spaces: Vec<_> = graph
        .spaces
        .iter()
        .map(|s| {
            if s.def.space == next.space {
                next.clone()
            } else {
                s.def.clone()
            }
        })
        .collect();
    let built = GraphSnapshot::build_blocking(graph, move |g| g.with_meta(schemas, spaces)).await?;
    tenant.set_graph(built);
    tenant
        .store
        .audit(
            &scope.actor(),
            "graph.space.access",
            &format!("{} {:?}", next.space, next.visibility),
        )
        .await?;
    tracing::info!(space = %next.space, visibility = ?next.visibility, "graph space access changed");
    Ok(Json(next.into()))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DataResponse {
    pub space: String,
    pub nodes_written: usize,
    pub edges_written: usize,
}

/// `POST /ingest/graph/data`: add nodes and edges to a space the caller can see, checked
/// against its schema. Both ends of every edge must be in the space, in this request or
/// already stored. A space the caller can't see is reported as not found.
#[tracing::instrument(name = "ingest_graph_data", skip_all)]
pub async fn data(
    State(_app): State<AppState>,
    scope: Scope,
    body: Result<Json<DataRequest>, JsonRejection>,
) -> Result<Json<DataResponse>, ApiError> {
    let Json(req) = body?;
    if req.nodes.len() > def::MAX_BATCH || req.edges.len() > def::MAX_BATCH {
        return Err(ApiError::BadRequest(format!(
            "at most {} nodes and {} edges per request",
            def::MAX_BATCH,
            def::MAX_BATCH
        )));
    }
    let tenant = scope.tenant();
    let _writer = tenant.lock_graph().await?;
    let graph = tenant.graph().await?;
    let Some(space) = graph.space(&req.space).filter(|s| s.visible_to(&scope)) else {
        return Err(ApiError::NotFound);
    };
    let schema_name = space.def.schema.clone();
    if req.schema.as_ref().is_some_and(|s| *s != schema_name) {
        return Err(ApiError::BadRequest(format!(
            "space {} is built from schema {schema_name}",
            req.space
        )));
    }
    let schema = graph
        .schemas
        .get(&schema_name)
        .ok_or_else(|| anyhow::anyhow!("space {} has no stored schema", req.space))?;
    let nodes = def::check_nodes(schema, &req.space, req.nodes).map_err(ApiError::BadRequest)?;
    let edges = def::check_edges(schema, &req.space, req.edges).map_err(ApiError::BadRequest)?;
    let batch: std::collections::HashSet<(&str, &str)> = nodes
        .iter()
        .map(|n| (n.ty.as_str(), n.key.as_str()))
        .collect();
    for e in &edges {
        for (ty, key) in [(&e.from_type, &e.from_key), (&e.to_type, &e.to_key)] {
            let in_batch = batch.contains(&(ty.as_str(), key.as_str()));
            let stored = graph.node(&def::node_ref(&req.space, ty, key)).is_some();
            if !in_batch && !stored {
                return Err(ApiError::BadRequest(format!(
                    "edge {}/{} {} {}/{}: node {ty}/{key} is not in space {}",
                    e.from_type, e.from_key, e.ty, e.to_type, e.to_key, req.space
                )));
            }
        }
    }

    tenant.graph_store.write(&nodes, &edges).await?;
    let (n_nodes, n_edges) = (nodes.len(), edges.len());
    let next = GraphSnapshot::build_blocking(graph, move |g| g.with_data(nodes, edges)).await?;
    tenant.set_graph(next);
    tracing::info!(space = %req.space, nodes = n_nodes, edges = n_edges, "graph data written");
    Ok(Json(DataResponse {
        space: req.space,
        nodes_written: n_nodes,
        edges_written: n_edges,
    }))
}
