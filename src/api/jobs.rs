//! `GET /jobs/{id}`, and the review queue: `GET /proposals`, `POST /proposals/{id}/approve`
//! and `POST /proposals/{id}/reject` (admin key only).

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::identity::{Admin, Scope, is_admin};
use crate::jobs::{JobInfo, JobState};
use crate::librarian::{self, Proposal, ProposalStatus};

#[tracing::instrument(name = "get_job", skip_all)]
pub async fn get(
    State(app): State<AppState>,
    headers: HeaderMap,
    scope: Scope,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<JobInfo>, ApiError> {
    let Path(id) = id?;
    let tenant = &scope.tenant().dir_name;
    let job = if is_admin(&headers, &app) {
        app.jobs.get_any(&id, tenant)
    } else {
        app.jobs
            .get(&id, tenant, scope.user().map(|u| u.dir.as_str()))
    };
    job.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct JobList {
    pub jobs: Vec<JobInfo>,
}

/// The tenant's jobs this caller may see, the newest first: its own and the tenant's lint runs,
/// or every one for the console's admin (who approves, and follows the filing jobs it starts).
#[tracing::instrument(name = "list_jobs", skip_all)]
pub async fn list_jobs(
    State(app): State<AppState>,
    headers: HeaderMap,
    scope: Scope,
) -> Json<JobList> {
    let tenant = &scope.tenant().dir_name;
    let jobs = if is_admin(&headers, &app) {
        app.jobs.list_all(tenant)
    } else {
        app.jobs.list(tenant, scope.user().map(|u| u.dir.as_str()))
    };
    Json(JobList { jobs })
}

/// A proposal as callers see it: without the writer's folders.
fn view(p: &Proposal) -> Proposal {
    let mut p = p.clone();
    if let Some(g) = p.graph.take() {
        p.graph = Some(Box::new(g.shown()));
    }
    p.user = None;
    p.groups.clear();
    p
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ProposalList {
    pub proposals: Vec<Proposal>,
}

/// Proposals waiting for review whose pages the caller may see.
#[tracing::instrument(name = "list_proposals", skip_all)]
pub async fn list(scope: Scope) -> Result<Json<ProposalList>, ApiError> {
    let held = scope.tenant().store.held_proposals().await?;
    // A graph draft is seen by who may see its space (a restricted space's members too).
    let graph = if held.iter().any(|p| p.graph.is_some()) {
        Some(scope.tenant().graph().await?)
    } else {
        None
    };
    Ok(Json(ProposalList {
        proposals: held
            .iter()
            .filter(|p| scope.sees_audience(&p.audience))
            .filter(|p| {
                graph
                    .as_ref()
                    .is_none_or(|g| super::draft::sees(g, &scope, p))
            })
            .map(view)
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Approved {
    pub proposal: String,
    pub status: ProposalStatus,
    /// The job that applied it, finished.
    pub job: JobInfo,
}

/// Applies a held proposal's edits through the tenant's write queue and answers once they are
/// written (200). A proposed source is queued for filing instead, and the answer is its job
/// (202): it is approved once that job has filed it.
#[tracing::instrument(name = "approve_proposal", skip_all)]
pub async fn approve(
    State(app): State<AppState>,
    _admin: Admin,
    id: Result<Path<String>, PathRejection>,
) -> Result<(StatusCode, Json<Approved>), ApiError> {
    let Path(id) = id?;
    let dir = librarian::tenant_dir_of_proposal(&id).ok_or(ApiError::NotFound)?;
    let tenant = app.tenants.open(&dir).await?.ok_or(ApiError::NotFound)?;
    let proposal = tenant
        .store
        .proposal(&id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if proposal.status != ProposalStatus::Held {
        return Err(ApiError::Conflict(format!(
            "proposal {id} is already {}",
            proposal.status.as_str()
        )));
    }
    if proposal.graph.is_some() {
        let job = super::draft::approve(&app, tenant, id.clone()).await?;
        tracing::info!(proposal = %id, "graph draft approved");
        return Ok((
            StatusCode::OK,
            Json(Approved {
                proposal: id,
                status: ProposalStatus::Approved,
                job,
            }),
        ));
    }
    if proposal.candidate.is_some() {
        let job = crate::acquisition::approve(&app, tenant, proposal).await?;
        tracing::info!(proposal = %id, job = %job.job, "proposed source approved");
        return Ok((
            StatusCode::ACCEPTED,
            Json(Approved {
                proposal: id,
                status: ProposalStatus::Approved,
                job,
            }),
        ));
    }
    let job = app
        .jobs
        .run(
            &tenant.dir_name,
            None,
            "approve",
            Box::pin(librarian::approve(tenant.clone(), proposal)),
        )
        .await;
    if job.status == JobState::Failed {
        // Another approval got there first.
        let now = tenant.store.proposal(&id).await?;
        // Deleted meanwhile, with its user or group.
        let Some(now) = now else {
            return Err(ApiError::NotFound);
        };
        if now.status != ProposalStatus::Held {
            return Err(ApiError::Conflict(format!(
                "proposal {id} is already approved"
            )));
        }
        let error = job.error.as_deref().unwrap_or("unknown error");
        if error.starts_with(librarian::NO_LONGER_APPLIES) {
            return Err(ApiError::Conflict(format!("proposal {id}: {error}")));
        }
        return Err(ApiError::Internal(anyhow::anyhow!(
            "approving {id} failed: {error}"
        )));
    }
    tracing::info!(proposal = %id, "proposal approved");
    Ok((
        StatusCode::OK,
        Json(Approved {
            proposal: id,
            status: ProposalStatus::Approved,
            job,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectRequest {
    /// Why: kept with the proposal, and for a proposed source shown to later runs.
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "RejectResponse"))]
pub struct Rejected {
    pub proposal: String,
    pub status: ProposalStatus,
}

/// Rejects a held proposal with a reason (design, "Filling gaps from outside", step 4).
#[tracing::instrument(name = "reject_proposal", skip_all)]
pub async fn reject(
    State(app): State<AppState>,
    _admin: Admin,
    id: Result<Path<String>, PathRejection>,
    body: Result<Json<RejectRequest>, JsonRejection>,
) -> Result<Json<Rejected>, ApiError> {
    let Path(id) = id?;
    let Json(req) = body?;
    let reason = req.reason.trim().to_owned();
    if reason.is_empty() || reason.chars().count() > 1000 {
        return Err(ApiError::BadRequest(
            "reason must say why, in at most 1000 characters".into(),
        ));
    }
    let dir = librarian::tenant_dir_of_proposal(&id).ok_or(ApiError::NotFound)?;
    let tenant = app.tenants.open(&dir).await?.ok_or(ApiError::NotFound)?;
    crate::acquisition::reject(&app, tenant, id.clone(), reason).await?;
    tracing::info!(proposal = %id, "proposal rejected");
    Ok(Json(Rejected {
        proposal: id,
        status: ProposalStatus::Rejected,
    }))
}
