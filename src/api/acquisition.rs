//! Acquisition's settings and status (design, "Filling gaps from outside, with approval"):
//! `PUT /admin/tenants/{id}/acquisition` turns it on for a tenant with its allowlist (admin key,
//! like approving what it proposes); `GET /acquisition` shows a tenant's callers the settings,
//! today's budget, the last run and the pages turned back or rejected.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use serde::Serialize;

use crate::AppState;
use crate::acquisition::crawl::CrawlReport;
use crate::acquisition::{self, Note, RunReport, Settings};
use crate::error::ApiError;
use crate::identity::{Admin, Scope};

#[derive(Debug, Serialize)]
pub struct Status {
    /// Whether the deployment has acquisition on.
    pub available: bool,
    pub settings: Settings,
    /// The last reading of the trusted sites.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_crawl: Option<CrawlReport>,
    pub budget: Budget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunReport>,
    /// Pages runs turned back, newest first.
    pub refused: Vec<Note>,
    /// Proposals a person rejected, newest first.
    pub rejected: Vec<Note>,
}

#[derive(Debug, Serialize)]
pub struct Budget {
    pub day: String,
    pub daily_tokens: u64,
    pub used: u64,
}

#[tracing::instrument(name = "acquisition_status", skip_all)]
pub async fn status(State(app): State<AppState>, scope: Scope) -> Result<Json<Status>, ApiError> {
    let tenant = scope.tenant();
    let settings = acquisition::settings(tenant).await?;
    let state = acquisition::state(tenant).await?;
    let today = crate::trace::now_rfc3339()[..10].to_owned();
    let used = if state.day == today { state.tokens } else { 0 };
    // A group's or user's gaps are their search terms: shown only to who may see them.
    let sees = |owner: &str| scope.sees_audience(owner);
    let last_run = state.last_run.map(|mut run| {
        run.matched.retain(|e| sees(&e.owner));
        run.skipped.retain(|e| sees(&e.owner));
        run
    });
    Ok(Json(Status {
        available: app.config.acquisition.enabled,
        settings,
        last_crawl: acquisition::last_crawl(tenant).await?,
        budget: Budget {
            day: today,
            daily_tokens: app.config.acquisition.daily_token_budget,
            used,
        },
        last_run,
        refused: state
            .refused
            .into_iter()
            .rev()
            .filter(|n| sees(&n.owner))
            .collect(),
        rejected: state
            .rejected
            .into_iter()
            .rev()
            .filter(|n| sees(&n.owner))
            .collect(),
    }))
}

#[tracing::instrument(name = "acquisition_settings", skip_all)]
pub async fn put_settings(
    State(app): State<AppState>,
    _admin: Admin,
    id: Result<Path<String>, PathRejection>,
    body: Result<Json<Settings>, JsonRejection>,
) -> Result<Json<Settings>, ApiError> {
    let Path(tenant_id) = id?;
    let Json(settings) = body?;
    let settings = settings.checked().map_err(ApiError::BadRequest)?;
    let tenant = app
        .tenants
        .by_id(&tenant_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    acquisition::put_settings(&tenant, &settings).await?;
    let domains: Vec<&str> = settings.sources.iter().map(|s| s.domain.as_str()).collect();
    tenant
        .store
        .audit(
            "admin",
            "acquisition.settings",
            &format!(
                "enabled: {}, sources: {}",
                settings.enabled,
                domains.join(", ")
            ),
        )
        .await?;
    Ok(Json(settings))
}
