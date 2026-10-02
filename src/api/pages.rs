//! `GET /pages` and `GET /pages/{path}`: browsing the wiki. No LLM, no search.

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::identity::Scope;
use crate::kinds::Kind;
use crate::search::visible_anchors;
use crate::wiki::{FrontMatter, PagePath, PageSummary};

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub kind: Option<Kind>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub pages: Vec<PageSummary>,
}

#[tracing::instrument(name = "list_pages", skip_all)]
pub async fn list(
    scope: Scope,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<ListResponse>, ApiError> {
    let Query(q) = query?;
    let wiki = scope.tenant().wiki();
    let user_dir = scope.user().map(|u| u.dir.clone());
    let group_dirs: Vec<String> = scope.groups().iter().map(|g| g.dir.clone()).collect();
    let mut pages =
        tokio::task::spawn_blocking(move || wiki.list(user_dir.as_deref(), &group_dirs))
            .await
            .map_err(anyhow::Error::from)??;
    if let Some(kind) = q.kind {
        pages.retain(|p| p.kind == Some(kind));
    }
    Ok(Json(ListResponse { pages }))
}

#[derive(Debug, Serialize)]
pub struct PageResponse {
    #[serde(rename = "page")]
    pub path: PagePath,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// The page's version; a `PUT` sends it back.
    pub version: String,
    pub front_matter: FrontMatter,
    pub body: String,
}

#[tracing::instrument(name = "get_page", skip_all, fields(path = tracing::field::Empty))]
pub async fn get(
    scope: Scope,
    raw: Result<Path<String>, PathRejection>,
) -> Result<Json<PageResponse>, ApiError> {
    let Path(raw) = raw?;
    tracing::Span::current().record("path", raw.as_str());
    let path = PagePath::parse(&raw)?;
    // Another user's or group's page is reported exactly like a missing one.
    if !scope.can_see(&path) {
        return Err(ApiError::NotFound);
    }
    let wiki = scope.tenant().wiki();
    let mut page = tokio::task::spawn_blocking(move || wiki.read(&path))
        .await
        .map_err(anyhow::Error::from)??;
    let version = page.version();
    // Anchors into pages or graph spaces the caller can't see are dropped, as in search hits.
    // Without the graph, every node anchor is dropped.
    if !page.front.about.is_empty() {
        let graph = scope.tenant().graph_or_empty().await;
        let vis = graph.visible(&scope);
        let snapshot = scope.tenant().snapshot().await?;
        page.front.about =
            visible_anchors(&scope, &snapshot.catalog, &graph, &vis, &page.front.about);
    }
    Ok(Json(PageResponse {
        kind: page.path.kind(),
        path: page.path,
        version,
        front_matter: page.front,
        body: page.body,
    }))
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutPage {
    /// The `version` `GET` returned; none (or absent) for a new page.
    #[serde(default)]
    pub version: Option<String>,
    /// What `GET` also returns, so its answer can be sent back as it is; ignored (the path
    /// says which page, and code writes `kind`).
    #[serde(default, rename = "page")]
    pub _page: Option<serde_json::Value>,
    #[serde(default, rename = "kind")]
    pub _kind: Option<serde_json::Value>,
    pub front_matter: FrontMatter,
    pub body: String,
}

/// `PUT /pages/{path}`: a hand edit, the whole page in the shape `GET` returns. Code checks it
/// at once (400 with the reason); then a job has the librarian verify it, and saves it or
/// holds it as a proposal (design, "Manual edits get the same check"). Schema descriptions
/// change through `/ingest/graph/schema`; `index` and `log` are the service's own.
#[tracing::instrument(name = "put_page", skip_all, fields(path = tracing::field::Empty))]
pub async fn put(
    axum::extract::State(app): axum::extract::State<crate::AppState>,
    scope: Scope,
    raw: Result<Path<String>, PathRejection>,
    body: Result<axum::Json<PutPage>, axum::extract::rejection::JsonRejection>,
) -> Result<(axum::http::StatusCode, Json<crate::jobs::JobInfo>), ApiError> {
    use crate::librarian::{Edit, PageAudience, Target};
    let Path(raw) = raw?;
    tracing::Span::current().record("path", raw.as_str());
    let path = PagePath::parse(&raw)?;
    if !scope.can_see(&path) {
        return Err(ApiError::NotFound);
    }
    let axum::Json(req) = body?;
    let kind = match path.kind() {
        None => {
            return Err(ApiError::BadRequest(
                "index and log are written by the service".into(),
            ));
        }
        Some(Kind::Graph) => {
            return Err(ApiError::BadRequest(
                "schema descriptions change through /ingest/graph/schema".into(),
            ));
        }
        Some(k) => k,
    };
    let librarian = app.librarian()?;
    let target = Target::new(kind, PageAudience::of(&path)).map_err(ApiError::BadRequest)?;
    let edit = Edit::Replace {
        page: path.to_string(),
        base: req.version,
        front_matter: req.front_matter,
        body: req.body,
    };
    let tenant = scope.tenant().clone();
    crate::librarian::check_edit(&tenant, &target, &scope, &edit)
        .await?
        .map_err(ApiError::BadRequest)?;
    let owner = scope.user().map(|u| u.dir.clone());
    let writer = scope.clone();
    let dir_name = tenant.dir_name.clone();
    let info = app.jobs.submit(
        &dir_name,
        owner.as_deref(),
        "hand_edit",
        Box::pin(async move { librarian.review(tenant, writer, target, edit).await }),
    );
    Ok((axum::http::StatusCode::ACCEPTED, Json(info)))
}
