//! `GET /jobs/{id}`, and the review queue: `GET /proposals` and
//! `POST /proposals/{id}/approve` (admin key only).

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::{Admin, Scope};
use crate::jobs::{JobInfo, JobState};
use crate::librarian::{self, Proposal, ProposalStatus};

#[tracing::instrument(name = "get_job", skip_all)]
pub async fn get(
    State(app): State<AppState>,
    scope: Scope,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<JobInfo>, ApiError> {
    let Path(id) = id?;
    let user = scope.user().map(|u| u.dir.as_str());
    app.jobs
        .get(&id, &scope.tenant().dir_name, user)
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// A proposal as callers see it: without the writer's folders.
#[derive(Debug, Serialize)]
pub struct ProposalView {
    #[serde(flatten)]
    proposal: serde_json::Value,
}

fn view(p: &Proposal) -> ProposalView {
    let mut v = serde_json::to_value(p).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("user");
        o.remove("groups");
    }
    ProposalView { proposal: v }
}

#[derive(Debug, Serialize)]
pub struct ProposalList {
    pub proposals: Vec<ProposalView>,
}

/// Proposals waiting for review whose pages the caller may see.
#[tracing::instrument(name = "list_proposals", skip_all)]
pub async fn list(scope: Scope) -> Result<Json<ProposalList>, ApiError> {
    let held = scope.tenant().store.held_proposals().await?;
    Ok(Json(ProposalList {
        proposals: held
            .iter()
            .filter(|p| scope.sees_audience(&p.audience))
            .map(view)
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
pub struct Approved {
    pub proposal: String,
    pub status: ProposalStatus,
    /// The job that applied it, finished.
    pub job: JobInfo,
}

/// Applies a held proposal's edits through the tenant's write queue and answers once they are
/// written.
#[tracing::instrument(name = "approve_proposal", skip_all)]
pub async fn approve(
    State(app): State<AppState>,
    _admin: Admin,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<Approved>, ApiError> {
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
            "proposal {id} is already approved"
        )));
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
    Ok(Json(Approved {
        proposal: id,
        status: ProposalStatus::Approved,
        job,
    }))
}
