//! `GET /status`: the service's state for the console's overview, as one tenant's caller sees
//! it: who is calling, the librarian, embeddings, the converters, the graph store, the
//! tenant's jobs and the proposals waiting. Never the model's key or the converter's address.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::jobs::JobState;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Caller {
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Librarian {
    pub on: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_because: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Embedding {
    /// Whether a model is configured at all.
    pub configured: bool,
    pub enabled: bool,
    pub status: crate::embed::Status,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Converters {
    /// File extensions the service converts itself (besides Markdown and text).
    pub builtin: &'static [&'static str],
    /// Whether MarkItDown is set up, and whether it answers (with its version).
    pub markitdown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markitdown_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markitdown_problem: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Jobs {
    pub queued: usize,
    pub running: usize,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Status {
    /// The code answering: crate version and commit.
    pub service: &'static str,
    pub deployment: crate::config::Deployment,
    pub identity: crate::config::IdentityMode,
    pub caller: Caller,
    pub librarian: Librarian,
    pub embedding: Embedding,
    pub converters: Converters,
    /// `sqlite` or `neo4j`.
    pub graph: &'static str,
    pub jobs: Jobs,
    /// Proposals waiting for review that the caller may see.
    pub proposals: usize,
    /// Whether the service listens only on this machine.
    pub local_only: bool,
    /// Whether the developer chat (`/dev/chat`, `PINAKES_DEV=1`) is on.
    pub dev: bool,
}

#[tracing::instrument(name = "status", skip_all)]
pub async fn status(
    State(app): State<AppState>,
    scope: Scope,
    headers: axum::http::HeaderMap,
) -> Result<Json<Status>, ApiError> {
    // The service's own workings (its model and files, why something is off, how it listens)
    // are the admin's to see; a tenant's caller learns only what is on.
    let admin = crate::identity::is_admin(&headers, &app);
    let tenant = scope.tenant();
    let model = app.model_state();
    let librarian = match app.librarian() {
        Ok(_) => Librarian {
            on: true,
            model: model.config.model().filter(|_| admin),
            off_because: None,
        },
        Err(e) => Librarian {
            on: false,
            model: model.config.model().filter(|_| admin),
            off_because: admin.then(|| e.to_string()),
        },
    };
    let (enabled, status) = match &app.embedding {
        Some(s) if admin => (s.is_on(), s.status()),
        Some(s) if crate::embed::Embedder::ready(&**s) => (true, crate::embed::Status::On),
        Some(s) => (s.is_on(), crate::embed::Status::Off),
        None if app.embedder.is_some() => (true, crate::embed::Status::On),
        None => (false, crate::embed::Status::Off),
    };
    let (markitdown_version, markitdown_problem) =
        if admin && app.converter.markitdown_url().is_some() {
            match app.converter.markitdown_health().await {
                Ok(v) => (Some(v), None),
                Err(why) => (None, Some(why)),
            }
        } else {
            (None, None)
        };
    let user = scope.user().map(|u| u.dir.as_str());
    let jobs = app.jobs.list(&tenant.dir_name, user);
    // Counted as `GET /proposals` lists them: a graph draft only for who may see its space.
    let held = tenant.store.held_proposals().await?;
    let graph = if held.iter().any(|p| p.graph.is_some()) {
        Some(tenant.graph().await?)
    } else {
        None
    };
    let proposals = held
        .iter()
        .filter(|p| scope.sees_audience(&p.audience))
        .filter(|p| {
            graph
                .as_ref()
                .is_none_or(|g| super::draft::sees(g, &scope, p))
        })
        .count();
    let host = app.config.server.host.as_str();
    Ok(Json(Status {
        service: crate::SERVICE,
        deployment: app.config.identity.deployment,
        identity: app.config.identity.mode,
        caller: Caller {
            tenant: tenant.id.clone(),
            user: scope.user().map(|u| u.id.clone()),
            groups: scope.groups().iter().map(|g| g.id.clone()).collect(),
        },
        librarian,
        embedding: Embedding {
            configured: app.config.embedding.model.is_some() || app.embedder.is_some(),
            enabled,
            status,
        },
        converters: Converters {
            builtin: crate::convert::BUILTIN,
            markitdown: app.converter.markitdown_url().is_some(),
            markitdown_version,
            markitdown_problem,
        },
        graph: app.tenants.graph().name(),
        jobs: Jobs {
            queued: jobs.iter().filter(|j| j.status == JobState::Queued).count(),
            running: jobs
                .iter()
                .filter(|j| j.status == JobState::Running)
                .count(),
        },
        proposals,
        local_only: admin && matches!(host, "127.0.0.1" | "localhost" | "::1"),
        dev: admin && crate::dev::enabled(),
    }))
}
