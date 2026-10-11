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
    let dir_name = tenant.dir_name.clone();
    // One run at a time: a run queued or running already is the answer, so repeated calls
    // don't stack model-backed runs on the write queue. Seen by every caller of the tenant:
    // the run is the tenant's, not one caller's.
    // Acquisition follows each lint run, when it is on (`crate::acquisition`).
    let app2 = app.clone();
    let info = app.jobs.submit_once(
        &dir_name,
        "lint",
        Box::pin(async move {
            let result = librarian.lint(tenant.clone()).await?;
            crate::acquisition::after_lint(&app2, &tenant).await;
            Ok(result)
        }),
    );
    Ok((StatusCode::ACCEPTED, Json(info)))
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
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
