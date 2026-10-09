//! Deletion: `DELETE /users/{user_id}` (the tenant's caller; in JWT mode only the caller's own
//! user), `DELETE /admin/tenants/{id}/groups/{group_id}` and `DELETE /admin/tenants/{id}`
//! (admin key). User and group deletion run on the tenant's write queue and answer once done.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Serialize;

use crate::AppState;
use crate::config::IdentityMode;
use crate::deletion::{self, Deleted, Owner};
use crate::error::ApiError;
use crate::identity::{Admin, Scope};
use crate::jobs::{JobResult, JobState};
use crate::tenants::{Tenant, validate_id};

#[derive(Debug, Serialize)]
pub struct UserDeleted {
    pub user: String,
    #[serde(flatten)]
    pub deleted: Deleted,
}

#[derive(Debug, Serialize)]
pub struct GroupDeleted {
    pub tenant_id: String,
    pub group: String,
    #[serde(flatten)]
    pub deleted: Deleted,
}

#[tracing::instrument(name = "delete_user", skip_all)]
pub async fn user(
    State(app): State<AppState>,
    scope: Scope,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<UserDeleted>, ApiError> {
    let Path(user_id) = id?;
    validate_id("user", &user_id).map_err(ApiError::BadRequest)?;
    // A token stands for one end user, who may delete only their own data; another user's is
    // not found, as everywhere.
    if app.config.identity.mode == IdentityMode::Jwt && scope.user().is_none_or(|u| u.id != user_id)
    {
        return Err(ApiError::NotFound);
    }
    let owner = Owner::User(user_id.clone());
    let deleted = run(&app, scope.tenant().clone(), owner, scope.actor()).await?;
    Ok(Json(UserDeleted {
        user: user_id,
        deleted,
    }))
}

#[tracing::instrument(name = "delete_group", skip_all)]
pub async fn group(
    State(app): State<AppState>,
    _admin: Admin,
    ids: Result<Path<(String, String)>, PathRejection>,
) -> Result<Json<GroupDeleted>, ApiError> {
    let Path((tenant_id, group_id)) = ids?;
    validate_id("group", &group_id).map_err(ApiError::BadRequest)?;
    let tenant = app
        .tenants
        .by_id(&tenant_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = Owner::Group(group_id.clone());
    let deleted = run(&app, tenant, owner, "admin".to_owned()).await?;
    Ok(Json(GroupDeleted {
        tenant_id,
        group: group_id,
        deleted,
    }))
}

#[tracing::instrument(name = "delete_tenant", skip_all)]
pub async fn tenant(
    State(app): State<AppState>,
    _admin: Admin,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(tenant_id) = id?;
    if app.tenants.delete(&tenant_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Runs a user's or group's deletion on the tenant's write queue and waits for it.
async fn run(
    app: &AppState,
    tenant: Arc<Tenant>,
    owner: Owner,
    actor: String,
) -> Result<Deleted, ApiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dir_name = tenant.dir_name.clone();
    let kind = match owner {
        Owner::User(_) => "delete_user",
        Owner::Group(_) => "delete_group",
    };
    let job = app
        .jobs
        .run(
            &dir_name,
            None,
            kind,
            Box::pin(async move {
                let _ = tx.send(deletion::delete(&tenant, &owner, &actor).await?);
                Ok(JobResult::default())
            }),
        )
        .await;
    match (job.status, rx.await) {
        (JobState::Done, Ok(deleted)) => Ok(deleted),
        _ => Err(ApiError::Internal(anyhow::anyhow!(
            "deletion failed: {}",
            job.error.as_deref().unwrap_or("unknown error")
        ))),
    }
}
