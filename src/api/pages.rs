//! `GET /pages` and `GET /pages/{path}`: browsing the wiki. No LLM, no search.

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::identity::Scope;
use crate::kinds::Kind;
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
    let mut pages = tokio::task::spawn_blocking(move || wiki.list(user_dir.as_deref()))
        .await
        .map_err(anyhow::Error::from)??;
    if let Some(kind) = q.kind {
        pages.retain(|p| p.kind == Some(kind));
    }
    Ok(Json(ListResponse { pages }))
}

#[derive(Debug, Serialize)]
pub struct PageResponse {
    pub path: PagePath,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
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
    // Another user's page is reported exactly like a missing one.
    if !scope.can_see(&path) {
        return Err(ApiError::NotFound);
    }
    let wiki = scope.tenant().wiki();
    let page = tokio::task::spawn_blocking(move || wiki.read(&path))
        .await
        .map_err(anyhow::Error::from)??;
    Ok(Json(PageResponse {
        kind: page.path.kind(),
        path: page.path,
        front_matter: page.front,
        body: page.body,
    }))
}
