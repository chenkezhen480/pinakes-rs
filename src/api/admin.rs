//! Service admin routes, guarded by the admin key.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Admin;
use crate::tenants::CreateTenantError;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct TenantOut {
    pub tenant_id: String,
    /// Its folder under `<data>/tenants`.
    pub dir: String,
    /// Whether its search snapshot is in memory now.
    pub in_memory: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct TenantList {
    pub tenants: Vec<TenantOut>,
}

/// Every tenant.
#[tracing::instrument(name = "list_tenants", skip_all)]
pub async fn list_tenants(
    _admin: Admin,
    State(app): State<AppState>,
) -> Result<Json<TenantList>, ApiError> {
    let tenants = app.tenants.list().await?;
    Ok(Json(TenantList {
        tenants: tenants
            .iter()
            .map(|t| TenantOut {
                tenant_id: t.id.clone(),
                dir: t.dir_name.clone(),
                in_memory: t.in_memory,
            })
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct TenantAudit {
    pub entries: Vec<crate::store::AuditEntry>,
}

/// A tenant's own audit log (deletions, approvals, graph changes), newest first.
#[tracing::instrument(name = "tenant_audit", skip_all)]
pub async fn tenant_audit(
    _admin: Admin,
    State(app): State<AppState>,
    id: Result<axum::extract::Path<String>, axum::extract::rejection::PathRejection>,
) -> Result<Json<TenantAudit>, ApiError> {
    let axum::extract::Path(id) = id?;
    let tenant = app.tenants.by_id(&id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(TenantAudit {
        entries: tenant.store.audit_log(500).await?,
    }))
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct CreateTenantRequest {
    pub tenant_id: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct CreateTenantResponse {
    pub tenant_id: String,
    /// Shown once; only its hash is stored.
    pub api_key: String,
}

#[tracing::instrument(name = "create_tenant", skip_all)]
pub async fn create_tenant(
    _admin: Admin,
    State(app): State<AppState>,
    body: Result<Json<CreateTenantRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CreateTenantResponse>), ApiError> {
    let Json(req) = body?;
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
    app.audit.record(
        "admin",
        "tenant.create",
        serde_json::json!({ "tenant": created.tenant.id }),
    );
    Ok((
        StatusCode::CREATED,
        Json(CreateTenantResponse {
            tenant_id: created.tenant.id.clone(),
            api_key: created.api_key,
        }),
    ))
}
