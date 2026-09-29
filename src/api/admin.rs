//! Service admin routes, guarded by the admin key.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Admin;
use crate::tenants::CreateTenantError;

#[derive(Debug, Deserialize)]
pub struct CreateTenantRequest {
    pub tenant_id: String,
}

#[derive(Debug, Serialize)]
pub struct CreateTenantResponse {
    pub tenant_id: String,
    /// Shown once; only its hash is stored.
    pub api_key: String,
}

#[tracing::instrument(name = "create_tenant", skip_all)]
pub async fn create_tenant(
    _admin: Admin,
    State(app): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> Result<(StatusCode, Json<CreateTenantResponse>), ApiError> {
    let created = app
        .tenants
        .create(&req.tenant_id, "initial")
        .await
        .map_err(|e| match e {
            CreateTenantError::InvalidId(msg) => ApiError::BadRequest(msg),
            CreateTenantError::AlreadyExists => ApiError::Conflict(e.to_string()),
            CreateTenantError::Internal(err) => ApiError::Internal(err),
        })?;
    tracing::info!(tenant = %created.tenant.dir_name, "tenant created");
    Ok((
        StatusCode::CREATED,
        Json(CreateTenantResponse {
            tenant_id: created.tenant.id.clone(),
            api_key: created.api_key,
        }),
    ))
}
