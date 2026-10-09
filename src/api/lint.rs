//! `POST /lint` queues a lint run on the tenant's write queue (202 with the job); `GET /lint`
//! lists the last run's findings the caller may see.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::jobs::JobInfo;
use crate::librarian::lint::{Finding, LintRun};

#[tracing::instrument(name = "lint_queue", skip_all)]
pub async fn run(
    State(app): State<AppState>,
    scope: Scope,
) -> Result<(StatusCode, Json<JobInfo>), ApiError> {
    let librarian = app.librarian()?;
    let tenant = scope.tenant().clone();
    let (owner, dir_name) = (scope.user().map(|u| u.dir.clone()), tenant.dir_name.clone());
    let info = app.jobs.submit(
        &dir_name,
        owner.as_deref(),
        "lint",
        Box::pin(async move { librarian.lint(tenant).await }),
    );
    Ok((StatusCode::ACCEPTED, Json(info)))
}

#[derive(Debug, Serialize)]
pub struct LintReport {
    /// The last run; absent before the first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<LintRun>,
    pub findings: Vec<Finding>,
}

#[tracing::instrument(name = "lint_report", skip_all)]
pub async fn report(scope: Scope) -> Result<Json<LintReport>, ApiError> {
    let Some((run, stored)) = scope.tenant().store.lint_run().await? else {
        return Ok(Json(LintReport {
            run: None,
            findings: Vec::new(),
        }));
    };
    let findings: Vec<Finding> = stored
        .into_iter()
        .filter(|s| scope.sees_audience(&s.audience))
        .map(|s| s.finding)
        .collect();
    // The run as this caller sees it: what was found for others is not counted.
    let run = LintRun {
        findings: findings.len(),
        proposals: findings.iter().filter(|f| f.proposal.is_some()).count(),
        ..run
    };
    Ok(Json(LintReport {
        run: Some(run),
        findings,
    }))
}
