//! The four insert entry points the librarian files: `/ingest/experience`, `/decision`,
//! `/expertise` and `/preference`. Each checks the record, saves it unchanged in `sources/`,
//! queues a librarian job on the tenant's write queue and answers 202 with the job id
//! (design, "The librarian: how new content is filed").
//!
//! The caller picks the audience in the audience header (`X-Audience`), like its identity and
//! never in the body: `tenant`, `user` (the calling user) or `group:<id>` (one of the caller's
//! own groups), among those the kind allows. Without it, experience and expertise are for the
//! tenant, and decisions and preferences for the calling user.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::jobs::JobInfo;
use crate::kinds::Kind;
use crate::librarian::{DocumentIn, Ingest, PageAudience, Target, documents, sources};

/// Most characters of one text field; a document's `content` may be longer.
const MAX_FIELD: usize = 20_000;
const MAX_DOCUMENT: usize = 200_000;
const MAX_ENTITIES: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperienceIn {
    /// The task type, such as "partial refund after shipment".
    task: String,
    /// What happened.
    outcome: String,
    #[serde(default)]
    details: Option<String>,
    /// Only read by the librarian, from the saved record.
    #[serde(default)]
    #[allow(dead_code)]
    succeeded: Option<bool>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionIn {
    topic: String,
    decision: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpertiseIn {
    title: String,
    content: String,
    /// The id the service gave this document when it was first filed (`result.document` of
    /// that job): this upload is its new version. Without it, the librarian recognizes the
    /// document itself, or files it as a new one.
    #[serde(default)]
    document: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceIn {
    statement: String,
    /// An observed habit rather than something the user said. Only read by the librarian,
    /// from the saved record.
    #[serde(default)]
    #[allow(dead_code)]
    observed: bool,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    date: Option<String>,
}

/// What every record gives the job: the text and terms to find related pages with, and the
/// audience it asks for.
struct Parsed {
    query: String,
    keywords: Vec<String>,
    entities: Vec<String>,
    /// For a document: its title, its text and the id the caller sent.
    document: Option<(String, String, Option<String>)>,
}

fn field(what: &str, s: &str, max: usize) -> Result<String, ApiError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ApiError::BadRequest(format!("{what} is empty")));
    }
    if s.chars().count() > max {
        return Err(ApiError::BadRequest(format!(
            "{what} is longer than {max} characters"
        )));
    }
    Ok(s.to_owned())
}

fn optional(what: &str, s: &Option<String>) -> Result<String, ApiError> {
    match s {
        Some(s) if !s.trim().is_empty() => field(what, s, MAX_FIELD),
        _ => Ok(String::new()),
    }
}

fn entities(list: &[String]) -> Result<Vec<String>, ApiError> {
    if list.len() > MAX_ENTITIES || list.iter().any(|e| e.chars().count() > 100) {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_ENTITIES} entities of at most 100 characters"
        )));
    }
    Ok(list
        .iter()
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty())
        .collect())
}

fn date(d: &Option<String>) -> Result<(), ApiError> {
    let ok = d.as_deref().is_none_or(|d| {
        let b = d.as_bytes();
        b.len() == 10
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                _ => c.is_ascii_digit(),
            })
    });
    if ok {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "date must look like 2026-09-28".into(),
        ))
    }
}

fn parse<T: DeserializeOwned>(body: &serde_json::Value) -> Result<T, ApiError> {
    serde_json::from_value(body.clone()).map_err(|e| ApiError::BadRequest(e.to_string()))
}

fn parse_kind(kind: Kind, body: &serde_json::Value) -> Result<Parsed, ApiError> {
    Ok(match kind {
        Kind::Experience => {
            let r: ExperienceIn = parse(body)?;
            date(&r.date)?;
            let task = field("task", &r.task, 200)?;
            let text = [
                task.clone(),
                field("outcome", &r.outcome, MAX_FIELD)?,
                optional("details", &r.details)?,
            ]
            .join("\n");
            Parsed {
                query: text,
                keywords: vec![task],
                entities: entities(&r.entities)?,
                document: None,
            }
        }
        Kind::Decision => {
            let r: DecisionIn = parse(body)?;
            date(&r.date)?;
            let topic = field("topic", &r.topic, 200)?;
            let text = [
                topic.clone(),
                field("decision", &r.decision, MAX_FIELD)?,
                optional("context", &r.context)?,
                optional("outcome", &r.outcome)?,
            ]
            .join("\n");
            Parsed {
                query: text,
                keywords: vec![topic],
                entities: entities(&r.entities)?,
                document: None,
            }
        }
        Kind::Expertise => {
            let r: ExpertiseIn = parse(body)?;
            date(&r.date)?;
            let title = field("title", &r.title, 200)?;
            let content = field("content", &r.content, MAX_DOCUMENT)?;
            optional("source_url", &r.source_url)?;
            let id = r.document.map(|d| d.trim().to_owned());
            if id
                .as_deref()
                .is_some_and(|d| !documents::valid_document_id(d))
            {
                return Err(ApiError::BadRequest(
                    "document must be an id the service issued, like d_0123456789abcdef".into(),
                ));
            }
            Parsed {
                query: format!("{title}\n{content}"),
                keywords: vec![title.clone()],
                entities: entities(&r.entities)?,
                document: Some((title, content, id)),
            }
        }
        Kind::Preference => {
            let r: PreferenceIn = parse(body)?;
            date(&r.date)?;
            Parsed {
                query: field("statement", &r.statement, MAX_FIELD)?,
                keywords: Vec::new(),
                entities: entities(&r.entities)?,
                document: None,
            }
        }
        Kind::Graph => unreachable!("graph inserts have their own endpoints"),
    })
}

/// The audience header's value, if sent.
fn audience_header(app: &AppState, headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let name = &app.config.identity.audience_header;
    match headers.get(name.as_str()) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .map(|s| Some(s.trim().to_owned()))
            .map_err(|_| ApiError::BadRequest(format!("{name} is not valid text"))),
    }
}

/// The audience a record is for, checked against the caller's credentials and the kind:
/// the audience header's choice, or the kind's default.
fn audience(kind: Kind, scope: &Scope, asked: Option<&str>) -> Result<Target, ApiError> {
    let user = || {
        scope
            .user()
            .map(|u| PageAudience::User(u.dir.clone()))
            .ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "a {} for a user needs the user header",
                    kind.as_str()
                ))
            })
    };
    let audience = match asked {
        Some("tenant") => PageAudience::Tenant,
        Some("user") => user()?,
        Some(a) if a.starts_with("group:") => {
            let g = a["group:".len()..].trim();
            let own = scope.groups().iter().find(|x| x.id == g).ok_or_else(|| {
                ApiError::BadRequest(format!("group {g:?} is not one of the caller's groups"))
            })?;
            PageAudience::Group(own.dir.clone())
        }
        Some(a) => {
            return Err(ApiError::BadRequest(format!(
                "audience {a:?} is not tenant, user or group:<id>"
            )));
        }
        None => match kind {
            Kind::Experience | Kind::Expertise => PageAudience::Tenant,
            _ => user()?,
        },
    };
    Target::new(kind, audience).map_err(ApiError::BadRequest)
}

async fn insert(
    app: AppState,
    scope: Scope,
    headers: HeaderMap,
    kind: Kind,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let Json(record) = body?;
    if !record.is_object() {
        return Err(ApiError::BadRequest(
            "the body must be a JSON object".into(),
        ));
    }
    let parsed = parse_kind(kind, &record)?;
    let asked = audience_header(&app, &headers)?;
    let mut target = audience(kind, &scope, asked.as_deref())?;
    let librarian = app.librarian()?;
    let tenant = scope.tenant().clone();

    // A document named by its id: one the caller can see. Its pages keep their audience.
    let mut given = None;
    if let Some((_, _, Some(id))) = &parsed.document {
        let doc = tenant.store.document(id).await?.ok_or(ApiError::NotFound)?;
        let audience = PageAudience::from_key(&doc.audience);
        let visible = match &audience {
            PageAudience::Tenant => true,
            PageAudience::Group(g) => scope.groups().iter().any(|x| &x.dir == g),
            PageAudience::User(u) => scope.user().is_some_and(|x| &x.dir == u),
        };
        if !visible {
            return Err(ApiError::NotFound);
        }
        if asked.is_some() && target.audience != audience {
            return Err(ApiError::BadRequest(format!(
                "document {id} was filed for another audience; leave out {}",
                app.config.identity.audience_header
            )));
        }
        target = Target::new(kind, audience).map_err(ApiError::BadRequest)?;
        given = Some(doc);
    }
    let echo = given.as_ref().map(|d| d.id.clone());
    let document = parsed.document.map(|(title, content, _)| DocumentIn {
        given,
        title,
        content,
    });

    // Step 1: the raw record is kept unchanged before anything else happens.
    let today = crate::trace::now_rfc3339()[..10].to_owned();
    let source = sources::new_name(kind, &today);
    let (dir, name, rec) = (tenant.dir.clone(), source.clone(), record.clone());
    tokio::task::spawn_blocking(move || sources::save(&dir, &name, &rec))
        .await
        .map_err(anyhow::Error::from)?
        .map_err(anyhow::Error::from)?;

    let job = Ingest {
        target,
        writer: scope.clone(),
        record,
        source,
        query: parsed.query,
        keywords: parsed.keywords,
        entities: parsed.entities,
        document,
    };
    let owner = scope.user().map(|u| u.dir.clone());
    let dir_name = tenant.dir_name.clone();
    let info = app.jobs.submit(
        &dir_name,
        owner.as_deref(),
        kind.as_str(),
        Box::pin(async move { librarian.ingest(tenant, job).await }),
    );
    let info = JobInfo {
        document: echo,
        ..info
    };
    tracing::info!(job = %info.job, kind = kind.as_str(), "record queued for the librarian");
    Ok((StatusCode::ACCEPTED, Json(info)))
}

#[tracing::instrument(name = "ingest_experience", skip_all)]
pub async fn experience(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Experience, body).await
}

#[tracing::instrument(name = "ingest_decision", skip_all)]
pub async fn decision(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Decision, body).await
}

#[tracing::instrument(name = "ingest_expertise", skip_all)]
pub async fn expertise(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Expertise, body).await
}

#[tracing::instrument(name = "ingest_preference", skip_all)]
pub async fn preference(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Preference, body).await
}
