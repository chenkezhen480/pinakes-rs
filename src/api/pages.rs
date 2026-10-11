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
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ListQuery {
    pub kind: Option<Kind>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
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
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PageResponse {
    #[serde(rename = "page")]
    #[cfg_attr(test, ts(type = "string"))]
    pub path: PagePath,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// The page's version; a `PUT` sends it back.
    pub version: String,
    pub front_matter: FrontMatter,
    pub body: String,
    /// The body as safe HTML, when asked for (`?html=true`): raw HTML shown as text, only
    /// web and mail links, no images, wiki links as console routes (`wiki::render`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Written from a page found outside (it cites an external source): lower trust.
    pub external: bool,
}

fn cites_external(front: &FrontMatter) -> bool {
    front
        .sources
        .iter()
        .any(|s| crate::librarian::sources::is_external(s))
}

/// Other query parameters are ignored, as they were before `html` existed (a proxy's
/// cache-buster, say).
#[derive(Debug, Deserialize)]
pub struct GetQuery {
    #[serde(default)]
    pub html: bool,
}

#[tracing::instrument(name = "get_page", skip_all, fields(path = tracing::field::Empty))]
pub async fn get(
    scope: Scope,
    raw: Result<Path<String>, PathRejection>,
    query: Result<Query<GetQuery>, QueryRejection>,
) -> Result<Json<PageResponse>, ApiError> {
    let Query(q) = query?;
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
        external: cites_external(&page.front),
        front_matter: page.front,
        html: q.html.then(|| crate::wiki::render::html(&page.body)),
        body: page.body,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    /// A version `GET /history/{path}` listed: answers with the page as it was then.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HistoryResponse {
    pub page: String,
    /// Each text the page had, oldest first, while the search traces that name its versions
    /// are kept (`[trace] retention_days`).
    pub versions: Vec<crate::wiki::PageVersion>,
}

/// `GET /history/{path}`: the versions of a page the service keeps (for replays), or with
/// `?version=` the page as it was in one of them, its body as HTML too.
#[tracing::instrument(name = "page_history", skip_all, fields(path = tracing::field::Empty))]
pub async fn history(
    scope: Scope,
    raw: Result<Path<String>, PathRejection>,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let Query(q) = query?;
    let Path(raw) = raw?;
    tracing::Span::current().record("path", raw.as_str());
    let path = PagePath::parse(&raw)?;
    if !scope.can_see(&path) {
        return Err(ApiError::NotFound);
    }
    let history = crate::wiki::History::new(&scope.tenant().dir);
    let key = path.to_string();
    match q.version {
        None => {
            let versions = tokio::task::spawn_blocking(move || history.page_versions(&key))
                .await
                .map_err(anyhow::Error::from)??;
            Ok(Json(HistoryResponse {
                page: path.to_string(),
                versions,
            })
            .into_response())
        }
        Some(v) => {
            let text = tokio::task::spawn_blocking(move || history.page_text(&v, &key))
                .await
                .map_err(anyhow::Error::from)??
                .ok_or(ApiError::NotFound)?;
            let mut page = crate::wiki::Page::parse(path, &text)?;
            // As for the page today: anchors the caller can't see are dropped.
            if !page.front.about.is_empty() {
                let graph = scope.tenant().graph_or_empty().await;
                let vis = graph.visible(&scope);
                let snapshot = scope.tenant().snapshot().await?;
                page.front.about =
                    visible_anchors(&scope, &snapshot.catalog, &graph, &vis, &page.front.about);
            }
            Ok(Json(PageResponse {
                kind: page.path.kind(),
                version: page.version(),
                html: Some(crate::wiki::render::html(&page.body)),
                external: cites_external(&page.front),
                path: page.path,
                front_matter: page.front,
                body: page.body,
            })
            .into_response())
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
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
    /// Also what `GET` returns, ignored: whether the page cites an outside source, and its
    /// rendered body.
    #[serde(default, rename = "external")]
    #[cfg_attr(test, ts(skip))]
    pub _external: Option<serde_json::Value>,
    #[serde(default, rename = "html")]
    #[cfg_attr(test, ts(skip))]
    pub _html: Option<serde_json::Value>,
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
