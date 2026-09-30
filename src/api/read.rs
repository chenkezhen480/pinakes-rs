//! `POST /read`: a follow-up after `/search`. Returns whole pages or one heading of them, with
//! the read tracker. Takes page ids as search returns them; a page the caller can't see is
//! reported exactly like a missing one. Raw sources arrive in M4.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::kinds::Kind;
use crate::search::catalog::{Fence, heading_of};
use crate::search::text::normalize;
use crate::search::{SearchSnapshot, validate_session};
use crate::trace::Endpoint;
use crate::wiki::{Page, PagePath, WikiError};

const MAX_IDS: usize = 20;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub ids: Vec<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub with_links: bool,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LinkSummary {
    pub page: String,
    pub title: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct ReadItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Already read in this session: the text is left out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub already_read: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkSummary>,
    /// `not_found`, `unreadable` (the page's file can't be parsed) or `heading_not_found`;
    /// other fields are then absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
}

impl ReadItem {
    fn error(id: String, error: &'static str) -> Self {
        Self {
            id,
            kind: None,
            title: None,
            summary: None,
            heading: None,
            text: None,
            already_read: false,
            links: Vec::new(),
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ReadResponse {
    pub pages: Vec<ReadItem>,
}

#[tracing::instrument(name = "read", skip_all)]
pub async fn read(
    State(app): State<AppState>,
    scope: Scope,
    body: Result<Json<ReadRequest>, JsonRejection>,
) -> Result<Json<ReadResponse>, ApiError> {
    let started = std::time::Instant::now();
    let Json(req) = body?;
    if req.ids.is_empty() || req.ids.len() > MAX_IDS {
        return Err(ApiError::BadRequest(format!(
            "ids must hold 1 to {MAX_IDS} page ids"
        )));
    }
    if let Some(s) = &req.session {
        validate_session(s).map_err(ApiError::BadRequest)?;
    }
    if req.heading.as_deref().is_some_and(|h| h.trim().is_empty()) {
        return Err(ApiError::BadRequest("heading is empty".into()));
    }
    // Only pages that search can return: every kind of page, not `index` or `log`.
    let mut paths = Vec::new();
    for id in &req.ids {
        let path = PagePath::parse(id)?;
        let readable = path.kind().is_some() && scope.can_see(&path);
        paths.push((id.clone(), readable.then_some(path)));
    }

    let wiki = scope.tenant().wiki();
    let wanted: Vec<PagePath> = paths.iter().filter_map(|(_, p)| p.clone()).collect();
    let loaded = tokio::task::spawn_blocking(move || {
        wanted
            .into_iter()
            .map(|p| match wiki.read(&p) {
                Ok(page) => Ok(Ok(page)),
                Err(WikiError::NotFound) => Ok(Err("not_found")),
                Err(WikiError::Malformed { path, reason }) => {
                    tracing::warn!(%path, %reason, "reading a malformed page");
                    Ok(Err("unreadable"))
                }
                Err(e) => Err(e),
            })
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(anyhow::Error::from)??;

    let tenant = scope.tenant();
    // Link summaries come from the search catalog; plain reads don't need it.
    let snapshot = if req.with_links {
        Some(tenant.snapshot().await?)
    } else {
        None
    };
    let user_dir = scope.user().map(|u| u.dir.as_str());
    let mut loaded = loaded.into_iter();
    let mut out = Vec::new();
    for (id, path) in paths {
        let page = match path {
            Some(_) => loaded.next().unwrap_or(Err("not_found")),
            None => Err("not_found"),
        };
        let page = match page {
            Ok(page) => page,
            Err(code) => {
                out.push(ReadItem::error(id, code));
                continue;
            }
        };
        let (heading, text) = match &req.heading {
            None => (None, page.body.clone()),
            Some(h) => match section(&page.body, h) {
                Some((heading, text)) => (Some(heading), text),
                None => {
                    out.push(ReadItem::error(id, "heading_not_found"));
                    continue;
                }
            },
        };
        let already_read = req.session.as_deref().is_some_and(|s| {
            tenant
                .reads
                .record(user_dir, s, page.path.as_str(), heading.as_deref())
        });
        let links = match &snapshot {
            Some(snapshot) => link_summaries(snapshot, &scope, &page),
            None => Vec::new(),
        };
        out.push(ReadItem {
            id,
            kind: page.path.kind(),
            title: page.front.title.clone(),
            summary: page.front.summary.clone(),
            heading,
            text: (!already_read).then_some(text),
            already_read,
            links,
            error: None,
        });
    }
    let mut record = scope.trace_record(Endpoint::Read);
    record.snapshot = snapshot.as_ref().map(|s| s.version.clone());
    record.search_config = app.search_fingerprint.clone();
    record.request = serde_json::to_value(&req).unwrap_or_default();
    record.result = serde_json::json!({
        "pages": out.iter().map(|p| serde_json::json!({
            "id": p.id,
            "heading": p.heading,
            "already_read": p.already_read,
            "error": p.error,
        })).collect::<Vec<_>>(),
    });
    record.timings_us.insert(
        "total".into(),
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
    app.traces.record(record);
    Ok(Json(ReadResponse { pages: out }))
}

/// One heading of a Markdown body with everything under it, down to the next heading of the
/// same or a higher level. Headings are compared normalized.
fn section(body: &str, wanted: &str) -> Option<(String, String)> {
    let wanted = normalize(wanted.trim().trim_start_matches('#').trim());
    let mut found: Option<(usize, String)> = None;
    let mut lines = Vec::new();
    let mut fence = Fence::default();
    for line in body.lines() {
        let in_code = fence.step(line.trim());
        let heading = if in_code {
            None
        } else {
            heading_of(line.trim())
        };
        match (&found, heading) {
            (None, Some((level, text))) if normalize(text) == wanted => {
                found = Some((level, text.to_owned()));
                lines.push(line);
            }
            (None, _) => {}
            (Some((level, _)), Some((l, _))) if l <= *level => break,
            (Some(_), _) => lines.push(line),
        }
    }
    let (_, heading) = found?;
    let mut text = lines.join("\n");
    text.push('\n');
    Some((heading, text))
}

/// Summaries of the visible pages a page links to with `[[…]]`. A link is a page path, or a
/// page name looked up among the caller's pages (tenant pages first).
fn link_summaries(snapshot: &SearchSnapshot, scope: &Scope, page: &Page) -> Vec<LinkSummary> {
    let catalog = &snapshot.catalog;
    let mut out: Vec<LinkSummary> = Vec::new();
    for target in crate::search::catalog::links(&page.body) {
        let entry = catalog
            .get(target.trim_end_matches(".md"))
            .map(|(_, e)| e)
            .filter(|e| scope.can_see(&e.path))
            .or_else(|| {
                let mut named: Vec<_> = catalog
                    .pages
                    .iter()
                    .filter(|e| e.path.segments().last() == Some(target.as_str()))
                    .filter(|e| scope.can_see(&e.path))
                    .collect();
                named.sort_by_key(|e| e.audience != "tenant");
                named.into_iter().next()
            });
        if let Some(e) = entry
            && e.path != page.path
            && !out.iter().any(|l| l.page == e.path.as_str())
        {
            out.push(LinkSummary {
                page: e.path.as_str().to_owned(),
                title: e.title.clone(),
                summary: e.summary.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str =
        "# Title\n\nintro\n\n## Steps\n\n1. one\n\n### Detail\n\nd\n\n## Pitfalls\n\np\n";

    #[test]
    fn extracts_a_heading_with_its_subsections() {
        let (h, text) = section(BODY, "steps").unwrap();
        assert_eq!(h, "Steps");
        assert_eq!(text, "## Steps\n\n1. one\n\n### Detail\n\nd\n\n");
        let (_, text) = section(BODY, "## Pitfalls").unwrap();
        assert_eq!(text, "## Pitfalls\n\np\n");
        // A `#` comment in a code block neither starts nor ends a section.
        let code = "## Setup\n\n```bash\n# install deps\nmake\n```\n\n## Next\n";
        let (_, text) = section(code, "Setup").unwrap();
        assert!(text.contains("make"), "{text}");
        assert!(section(code, "install deps").is_none());
        assert!(section(BODY, "Missing").is_none());
    }
}
