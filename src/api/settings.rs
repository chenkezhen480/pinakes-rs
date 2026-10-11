//! The console's settings, all behind the admin key (design, "M10": settings made clear):
//! `GET`/`PUT /admin/settings` (config.toml, explained field by field, written back with its
//! comments kept), `GET /admin/environment` (the variables the service reads, secrets shown
//! only as set or not), `GET`/`PUT /admin/model` and `POST /admin/model/test` (model.conf, the
//! key write-only, applied without a restart) and `GET /admin/audit`. Writes are audited.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::admin::{AuditEntry, EnvVar, ModelChange, ModelTest, ModelView};
use crate::error::ApiError;
use crate::identity::Admin;
use crate::settings::SettingsView;

pub async fn get(
    _admin: Admin,
    State(app): State<AppState>,
) -> Result<Json<SettingsView>, ApiError> {
    Ok(Json(view_off_runtime(app).await?))
}

/// The settings view reads and parses the config file: off the async threads.
async fn view_off_runtime(app: AppState) -> Result<SettingsView, ApiError> {
    Ok(tokio::task::spawn_blocking(move || view(&app))
        .await
        .map_err(anyhow::Error::from)?)
}

/// The settings with the embedding switch as it is now (it applies at once).
fn view(app: &AppState) -> SettingsView {
    crate::settings::view(&app.config, app.embedding.as_ref().map(|s| s.is_on()))
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct SettingsChange {
    /// `section.key` → the new value; `null` puts a field back to its default.
    pub changes: serde_json::Map<String, serde_json::Value>,
}

/// Writes settings to the config file, all of them in one write the whole config must still
/// load after, or none. `embedding.enabled` then applies at once (as `PUT /admin/embedding`
/// does); the others after a restart, which the answer says.
#[tracing::instrument(name = "put_settings", skip_all)]
pub async fn put(
    _admin: Admin,
    State(app): State<AppState>,
    body: Result<Json<SettingsChange>, JsonRejection>,
) -> Result<Json<SettingsView>, ApiError> {
    let Json(req) = body?;
    let Some(file) = app.config.file.clone() else {
        return Err(ApiError::Conflict(
            "the service was started without a config file; there is nothing to write to".into(),
        ));
    };
    let mut writes = Vec::new();
    let mut embedding = None;
    for (path, value) in &req.changes {
        let checked = crate::settings::check(path, value).map_err(ApiError::BadRequest)?;
        if path == "embedding.enabled" {
            if app.embedding.is_none() && value.as_bool() == Some(true) {
                return Err(ApiError::Conflict(
                    "no embedding model is configured: set [embedding] model in config.toml".into(),
                ));
            }
            embedding = Some(value.as_bool().unwrap_or(false));
        }
        writes.push((path.clone(), checked));
    }
    if embedding == Some(false) {
        app.can_turn_off_embedding()?;
    }
    if !writes.is_empty() {
        let w = writes.clone();
        let f = file.clone();
        tokio::task::spawn_blocking(move || crate::config::write_values(&f, &w))
            .await
            .map_err(anyhow::Error::from)?
            .map_err(|e| match crate::admin::not_written(&file, e) {
                ApiError::Internal(e) => ApiError::BadRequest(format!("{e:#}")),
                other => other,
            })?;
        app.audit.record(
            "admin",
            "settings.write",
            serde_json::Value::Object(req.changes.clone()),
        );
        tracing::info!(
            fields = writes.len(),
            "settings written; a restart applies them"
        );
    }
    if let Some(on) = embedding {
        app.apply_embedding(on);
    }
    Ok(Json(view_off_runtime(app).await?))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Environment {
    pub variables: Vec<EnvVar>,
}

pub async fn environment(_admin: Admin, State(app): State<AppState>) -> Json<Environment> {
    Json(Environment {
        variables: crate::admin::environment(&app),
    })
}

pub async fn get_model(_admin: Admin, State(app): State<AppState>) -> Json<ModelView> {
    Json(app.model_view())
}

#[tracing::instrument(name = "put_model", skip_all)]
pub async fn put_model(
    _admin: Admin,
    State(app): State<AppState>,
    body: Result<Json<ModelChange>, JsonRejection>,
) -> Result<Json<ModelView>, ApiError> {
    let Json(change) = body?;
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || app2.set_model(&change))
        .await
        .map_err(anyhow::Error::from)?
        .map(Json)
}

/// One tiny call to the model, as it is or as the body would change it (nothing is saved).
#[tracing::instrument(name = "test_model", skip_all)]
pub async fn test_model(
    _admin: Admin,
    State(app): State<AppState>,
    body: Option<Json<ModelChange>>,
) -> Json<ModelTest> {
    let change = body.map(|Json(c)| c).unwrap_or_default();
    Json(app.test_model(&change).await)
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
}

/// The service's audit log, newest first.
pub async fn audit(
    _admin: Admin,
    State(app): State<AppState>,
    query: Result<Query<AuditQuery>, QueryRejection>,
) -> Result<Json<AuditLog>, ApiError> {
    let Query(q) = query?;
    let limit = q.limit.unwrap_or(200).clamp(1, 2000);
    let app2 = app.clone();
    let entries = tokio::task::spawn_blocking(move || app2.audit.recent(limit))
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(AuditLog { entries }))
}
