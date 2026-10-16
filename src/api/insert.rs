//! The insert entry points the librarian files: `/ingest/experience`, `/decision`, `/expertise`
//! and `/preference`, and `/ingest/note`, which the librarian splits into decisions and lessons.
//! Each checks the record, saves it unchanged in `sources/`, queues a librarian job on the
//! tenant's write queue and answers 202 with the job id (design, "The librarian: how new
//! content is filed").
//!
//! The caller picks the audience in the audience header (`X-Audience`), like its identity and
//! never in the body: `tenant`, `user` (the calling user) or `group:<id>` (one of the caller's
//! own groups), among those the kind allows. Without it, experience and expertise are for the
//! tenant, and decisions and preferences for the calling user; a note's decisions are the
//! user's and its lessons the tenant's, unless the header names a group for both.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::AppState;
use crate::convert::{Converted, Read};
use crate::error::ApiError;
use crate::identity::Scope;
use crate::jobs::{JobInfo, JobResult, NoteFiled};
use crate::kinds::Kind;
use crate::librarian::{DocumentIn, Ingest, PageAudience, Target, documents, sources};

/// Most characters of one text field; a document's `content` may be longer.
const MAX_FIELD: usize = 20_000;
const MAX_DOCUMENT: usize = 200_000;
const MAX_ENTITIES: usize = 20;

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(crate) struct ExperienceIn {
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
    /// The session the agent searched and read with during the task (see [`read_pages`]).
    #[serde(default)]
    session: Option<String>,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(crate) struct DecisionIn {
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
    /// The session the agent searched and read with while deciding.
    #[serde(default)]
    session: Option<String>,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpertiseIn {
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
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(crate) struct PreferenceIn {
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

/// A note an agent recorded: a decision, a lesson or both, which the librarian splits (MCP
/// `record_decision_or_lesson`).
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteIn {
    /// What it is about, short: a decision's topic, or the kind of task a lesson is about.
    topic: String,
    /// What was decided and what was learned, with the exact numbers and conditions.
    content: String,
    /// The situation that led to it, and what came of it.
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    date: Option<String>,
    /// The session the agent searched and read with (see [`read_pages`]).
    #[serde(default)]
    session: Option<String>,
}

/// What every record gives the job: the text and terms to find related pages with, and the
/// audience it asks for.
struct Parsed {
    query: String,
    keywords: Vec<String>,
    entities: Vec<String>,
    /// For a document: its title, its text and the id the caller sent.
    document: Option<(String, String, Option<String>)>,
    /// The read-tracker session the record names (experience and decisions).
    session: Option<String>,
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

fn session(s: &Option<String>) -> Result<Option<String>, ApiError> {
    match s {
        None => Ok(None),
        Some(s) => {
            crate::search::validate_session(s).map_err(ApiError::BadRequest)?;
            Ok(Some(s.clone()))
        }
    }
}

/// The knowledge pages (expertise and schema descriptions) the caller's session read during
/// the task: what the memory most likely relied on. The librarian offers those the new page
/// may anchor to (up to 8) as anchors, so the memory-to-knowledge hop leads back to them.
fn read_pages(scope: &Scope, session: Option<&str>) -> Vec<String> {
    let Some(session) = session else {
        return Vec::new();
    };
    scope
        .read_state(Some(session))
        .pages()
        .into_iter()
        .filter(|p| {
            crate::wiki::PagePath::parse(p).is_ok_and(|p| {
                p.kind().is_some_and(|k| {
                    crate::search::catalog::Slot::of(k) == crate::search::catalog::Slot::Knowledge
                })
            })
        })
        .collect()
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
                session: session(&r.session)?,
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
                session: session(&r.session)?,
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
                session: None,
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
                session: None,
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

/// An uploaded file: already turned into the record's `content`, or a scan the model reads in
/// the job.
struct Upload {
    /// The name it was sent with, only ever kept as text.
    name: String,
    ext: String,
    bytes: axum::body::Bytes,
    read: Read,
}

/// A record checked, and what its job needs to file it.
struct Checked {
    kind: Kind,
    record: serde_json::Value,
    parsed: Parsed,
    target: Target,
    document: Option<DocumentIn>,
    /// The document id the caller sent, echoed in the answer.
    echo: Option<String>,
}

async fn insert(
    app: AppState,
    scope: Scope,
    headers: HeaderMap,
    kind: Kind,
    body: Result<Json<serde_json::Value>, JsonRejection>,
    upload: Option<Upload>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let Json(record) = body?;
    let scan = match &upload {
        Some(Upload {
            read: Read::Scan { pages },
            ..
        }) => Some(*pages),
        _ => None,
    };
    if let Some(pages) = scan {
        let upload = upload.expect("a scan is an upload");
        return insert_scan(app, scope, headers, kind, record, upload, pages).await;
    }
    let checked = check(&app, &scope, &headers, kind, record).await?;
    let librarian = app.librarian()?;
    let echo = checked.echo.clone();
    let converted = upload.as_ref().and_then(|u| match &u.read {
        Read::Text(c) => Some(c.clone()),
        Read::Scan { .. } => None,
    });
    let file_of = upload.as_ref().zip(converted.as_ref());
    let job = file(&scope, checked, file_of).await?;
    Ok(queue(&app, &scope, librarian, job, echo))
}

/// A scanned PDF's upload: its fields are checked now, with a stand-in for its text, and the
/// job has the model read the scan first, then checks the record again with the text, keeps it
/// and files it as any other. If the model can't read the scan, the job fails with why and
/// nothing is kept.
async fn insert_scan(
    app: AppState,
    scope: Scope,
    headers: HeaderMap,
    kind: Kind,
    mut record: serde_json::Value,
    upload: Upload,
    pages: usize,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let mut stand_in = record.clone();
    if let Some(o) = stand_in.as_object_mut() {
        o.insert("content".into(), json!("(a scan, read by the model)"));
    }
    let checked = check(&app, &scope, &headers, kind, stand_in).await?;
    let librarian = app.librarian()?;
    let echo = checked.echo.clone();
    let tenant = scope.tenant().clone();
    let (owner, dir_name) = (scope.user().map(|u| u.dir.clone()), tenant.dir_name.clone());
    let jobs_app = app.clone();
    let work: crate::jobs::Work = Box::pin(async move {
        let markdown = librarian
            .transcribe(&app.converter, &upload.bytes, pages)
            .await?;
        if let Some(o) = record.as_object_mut() {
            o.insert("content".into(), json!(markdown));
        }
        let checked = check(&app, &scope, &headers, kind, record).await?;
        let converted = Converted {
            markdown,
            converter: crate::convert::MODEL_NAME.into(),
            version: librarian.model_name(),
            links: Vec::new(),
        };
        let job = file(&scope, checked, Some((&upload, &converted))).await?;
        librarian.ingest(tenant, job).await
    });
    let info = jobs_app
        .jobs
        .submit(&dir_name, owner.as_deref(), kind.as_str(), work);
    tracing::info!(job = %info.job, pages, "scan queued for the model to read");
    Ok((
        StatusCode::ACCEPTED,
        Json(JobInfo {
            document: echo,
            ..info
        }),
    ))
}

/// The record checked: its fields, its audience, and the document it names.
async fn check(
    app: &AppState,
    scope: &Scope,
    headers: &HeaderMap,
    kind: Kind,
    mut record: serde_json::Value,
) -> Result<Checked, ApiError> {
    if !record.is_object() {
        return Err(ApiError::BadRequest(
            "the body must be a JSON object".into(),
        ));
    }
    let mut parsed = parse_kind(kind, &record)?;
    // The session only says which reads to look at; it is no part of what happened, so it is
    // neither kept with the record nor shown to the librarian.
    if let Some(o) = record.as_object_mut() {
        o.remove("session");
    }
    let asked = audience_header(app, headers)?;
    let mut target = audience(kind, scope, asked.as_deref())?;
    let tenant = scope.tenant();

    // A document named by its id: one the caller can see. Its pages keep their audience.
    let mut given = None;
    if let Some((_, _, Some(id))) = &parsed.document {
        let doc = tenant.store.document(id).await?.ok_or(ApiError::NotFound)?;
        if !scope.sees_audience(&doc.audience) {
            return Err(ApiError::NotFound);
        }
        let audience = PageAudience::from_key(&doc.audience);
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
    let document = parsed
        .document
        .take()
        .map(|(title, content, _)| DocumentIn {
            given,
            title,
            content,
        });
    Ok(Checked {
        kind,
        record,
        parsed,
        target,
        document,
        echo,
    })
}

/// Step 1: the raw record is kept unchanged before anything else happens; an uploaded file is
/// kept too, next to it, and the record says what it was and what converted it. Returns the
/// job that files it.
async fn file(
    scope: &Scope,
    checked: Checked,
    upload: Option<(&Upload, &Converted)>,
) -> Result<Ingest, ApiError> {
    let Checked {
        kind,
        mut record,
        parsed,
        target,
        document,
        ..
    } = checked;
    let tenant = scope.tenant();
    let today = crate::trace::now_rfc3339()[..10].to_owned();
    let source = sources::new_name(kind, &today);
    let original = match upload {
        None => None,
        Some((u, converted)) => {
            let original = format!("{}.{}", source.trim_end_matches(".json"), u.ext);
            record["file"] = json!({
                "name": u.name,
                "original": original,
                "converter": converted.converter,
                "version": converted.version,
            });
            Some((original, u.bytes.clone()))
        }
    };
    // Noted by audience first, so deleting a user or group finds its records: a note without
    // its file is harmless, a file without its note would be left behind.
    tenant
        .store
        .add_record(&source, target.audience.key())
        .await?;
    let (dir, name, rec) = (tenant.dir.clone(), source.clone(), record.clone());
    tokio::task::spawn_blocking(move || {
        if let Some((original, bytes)) = original {
            sources::save_file(&dir, &original, &bytes)?;
        }
        sources::save(&dir, &name, &rec)
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(anyhow::Error::from)?;

    Ok(Ingest {
        target,
        writer: scope.clone(),
        read: read_pages(scope, parsed.session.as_deref()),
        record,
        source,
        query: parsed.query,
        keywords: parsed.keywords,
        entities: parsed.entities,
        document,
    })
}

/// Queues the job that files a kept record, and answers 202 with it.
fn queue(
    app: &AppState,
    scope: &Scope,
    librarian: std::sync::Arc<crate::librarian::Librarian>,
    job: Ingest,
    echo: Option<String>,
) -> (StatusCode, Json<JobInfo>) {
    let tenant = scope.tenant().clone();
    let kind = job.target.kind;
    let owner = scope.user().map(|u| u.dir.clone());
    let dir_name = tenant.dir_name.clone();
    // A record waits to be filed with the records queued alongside it; a document is read.
    let work: crate::jobs::Work = if job.document.is_some() {
        Box::pin(async move { librarian.ingest(tenant, job).await })
    } else {
        librarian.queue_record(tenant, job)
    };
    let info = app
        .jobs
        .submit(&dir_name, owner.as_deref(), kind.as_str(), work);
    let info = JobInfo {
        document: echo,
        ..info
    };
    tracing::info!(job = %info.job, kind = kind.as_str(), "record queued for the librarian");
    (StatusCode::ACCEPTED, Json(info))
}

#[tracing::instrument(name = "ingest_experience", skip_all)]
pub async fn experience(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Experience, body, None).await
}

#[tracing::instrument(name = "ingest_decision", skip_all)]
pub async fn decision(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Decision, body, None).await
}

/// `POST /ingest/expertise`: a document as JSON (`title`, `content`, …), or as a file
/// (`multipart/form-data`: the `file` and the same fields as text, `entities` repeated; the
/// title defaults to the file name). A file is turned into Markdown first; a scanned PDF is
/// read by the model in the job.
#[tracing::instrument(name = "ingest_expertise", skip_all)]
pub async fn expertise(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    request: Request,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"));
    if !multipart {
        let body = Json::from_request(request, &app).await;
        return insert(app, scope, headers, Kind::Expertise, body, None).await;
    }
    // Checked before converting, so a file isn't converted for nothing.
    app.librarian()?;
    let form = Multipart::from_request(request, &app)
        .await
        .map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let (record, upload) = read_upload(&app, form).await?;
    insert(
        app,
        scope,
        headers,
        Kind::Expertise,
        Ok(Json(record)),
        Some(upload),
    )
    .await
}

/// The record a file upload stands for, and the file, converted (or found to be a scan).
async fn read_upload(
    app: &AppState,
    mut form: Multipart,
) -> Result<(serde_json::Value, Upload), ApiError> {
    let bad = |e: axum::extract::multipart::MultipartError| match e.status() {
        StatusCode::PAYLOAD_TOO_LARGE => ApiError::TooLarge(e.body_text()),
        _ => ApiError::BadRequest(e.body_text()),
    };
    let mut record = serde_json::Map::new();
    let mut entities = Vec::new();
    let mut file = None;
    while let Some(field) = form.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                let file_name = field.file_name().unwrap_or_default().to_owned();
                file = Some((file_name, field.bytes().await.map_err(bad)?));
            }
            // A form's blank inputs arrive empty: left out, as a JSON caller would.
            "title" | "document" | "source_url" | "date" => {
                let text = field.text().await.map_err(bad)?;
                if !text.trim().is_empty() {
                    record.insert(name, json!(text));
                }
            }
            "entities" => {
                let text = field.text().await.map_err(bad)?;
                if !text.trim().is_empty() {
                    entities.push(text);
                }
            }
            other => {
                return Err(ApiError::BadRequest(format!(
                    "unknown field {other:?}; a file upload takes file, title, document, \
                     source_url, date and entities"
                )));
            }
        }
    }
    let (name, bytes) = file.ok_or_else(|| ApiError::BadRequest("no file was sent".into()))?;
    if bytes.is_empty() {
        return Err(ApiError::BadRequest("the file is empty".into()));
    }
    // The extension is taken before a long name is shortened, so it is never cut off.
    let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
    let ext = ext.to_ascii_lowercase();
    let stem: String = stem.chars().take(200).collect();
    let name = format!("{stem}.{ext}");
    if ext.is_empty() || ext.len() > 10 || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(ApiError::UnsupportedMediaType(
            "the file's name needs an extension that says its format, such as .docx".into(),
        ));
    }
    if !record.contains_key("title") && !stem.trim().is_empty() {
        record.insert("title".into(), json!(stem.trim()));
    }
    // A scan's text is written by the model, in the job.
    let read = app.converter.read(&ext, bytes.clone()).await?;
    if let Read::Text(converted) = &read {
        record.insert("content".into(), json!(converted.markdown));
    }
    record.insert("entities".into(), json!(entities));
    let upload = Upload {
        name,
        ext,
        bytes,
        read,
    };
    Ok((serde_json::Value::Object(record), upload))
}

#[tracing::instrument(name = "ingest_preference", skip_all)]
pub async fn preference(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    insert(app, scope, headers, Kind::Preference, body, None).await
}

/// `POST /ingest/note`: a decision, a lesson or both, as an agent recorded it in the turn (MCP
/// `record_decision_or_lesson`). The note is kept as it came, for the user who sent it, and the
/// job has the librarian split it: each decision is filed for the user, each lesson for the
/// tenant, as `/ingest/decision` and `/ingest/experience` would file them (each a record of its
/// own, citing the note). `X-Audience: group:<id>` files both for that group; no other audience
/// fits both kinds.
#[tracing::instrument(name = "ingest_note", skip_all)]
pub async fn note(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let Json(mut record) = body?;
    if !record.is_object() {
        return Err(ApiError::BadRequest(
            "the body must be a JSON object".into(),
        ));
    }
    let r: NoteIn = parse(&record)?;
    date(&r.date)?;
    let topic = field("topic", &r.topic, 200)?;
    let content = field("content", &r.content, MAX_FIELD)?;
    let context = optional("context", &r.context)?;
    let entities = entities(&r.entities)?;
    let session = session(&r.session)?;
    if let Some(o) = record.as_object_mut() {
        o.remove("session");
    }
    let (decisions, lessons) = match audience_header(&app, &headers)?.as_deref() {
        None => {
            let user = scope.user().ok_or_else(|| {
                ApiError::BadRequest(
                    "a note needs the user header: its decisions are filed for the user".into(),
                )
            })?;
            (PageAudience::User(user.dir.clone()), PageAudience::Tenant)
        }
        Some(a) if a.starts_with("group:") => {
            let group = audience(Kind::Decision, &scope, Some(a))?.audience;
            (group.clone(), group)
        }
        Some(a) => {
            return Err(ApiError::BadRequest(format!(
                "a note's decisions are filed for the user and its lessons for the tenant; {} \
                 may only name one of the caller's groups (group:<id>) for both, not {a:?}",
                app.config.identity.audience_header
            )));
        }
    };
    let librarian = app.librarian()?;
    let tenant = scope.tenant().clone();
    let today = crate::trace::now_rfc3339()[..10].to_owned();
    let source = sources::new_note_name(&today);
    // Kept for whom its decisions are, so deleting the user (or group) deletes it.
    tenant.store.add_record(&source, decisions.key()).await?;
    let (dir, name, rec) = (tenant.dir.clone(), source.clone(), record.clone());
    tokio::task::spawn_blocking(move || sources::save(&dir, &name, &rec))
        .await
        .map_err(anyhow::Error::from)?
        .map_err(anyhow::Error::from)?;

    let read = read_pages(&scope, session.as_deref());
    let text = [topic.as_str(), content.as_str(), context.as_str()]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let owner = scope.user().map(|u| u.dir.clone());
    let dir_name = tenant.dir_name.clone();
    let work: crate::jobs::Work = Box::pin(async move {
        let items = librarian.split_note(&topic, &text).await?;
        let mut result = JobResult {
            source: Some(format!("sources/{source}")),
            ..JobResult::default()
        };
        for item in items {
            let kind = item.kind.kind();
            let audience = match kind {
                Kind::Decision => decisions.clone(),
                _ => lessons.clone(),
            };
            let target = Target::new(kind, audience).map_err(anyhow::Error::msg)?;
            let mut filed = match kind {
                Kind::Decision => json!({ "topic": item.title, "decision": item.text }),
                _ => json!({ "task": item.title, "outcome": item.text }),
            };
            filed["entities"] = json!(entities);
            if let Some(d) = &r.date {
                filed["date"] = json!(d);
            }
            filed["note"] = json!(source);
            let checked = Checked {
                kind,
                record: filed,
                parsed: Parsed {
                    query: format!("{}\n{}", item.title, item.text),
                    keywords: vec![item.title.clone()],
                    entities: entities.clone(),
                    document: None,
                    session: None,
                },
                target,
                document: None,
                echo: None,
            };
            let mut job = file(&scope, checked, None).await?;
            job.read = read.clone();
            let record_source = job.source.clone();
            let audience = job.target.audience.key().to_owned();
            let done = librarian.ingest(tenant.clone(), job).await?;
            result.pages_created.extend(done.pages_created);
            result.pages_updated.extend(done.pages_updated);
            result.rejected.extend(done.rejected);
            result.approved.extend(done.approved);
            if result.proposal.is_none() {
                result.proposal = done.proposal.clone();
            }
            result.split.push(NoteFiled {
                kind: kind.as_str().to_owned(),
                audience,
                source: format!("sources/{record_source}"),
                proposal: done.proposal,
            });
        }
        Ok(result)
    });
    let info = app.jobs.submit(&dir_name, owner.as_deref(), "note", work);
    tracing::info!(job = %info.job, "note queued for the librarian");
    Ok((StatusCode::ACCEPTED, Json(info)))
}
