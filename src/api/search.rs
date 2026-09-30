//! `POST /search`: the only search entry. Reads the tenant's in-memory snapshot; no LLM.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::search::{SearchRequest, SearchResponse};
use crate::trace::Endpoint;

#[tracing::instrument(name = "search", skip_all)]
pub async fn search(
    State(app): State<AppState>,
    scope: Scope,
    body: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Json<SearchResponse>, ApiError> {
    let started = std::time::Instant::now();
    let Json(req) = body?;
    req.validate().map_err(ApiError::BadRequest)?;
    let tenant = scope.tenant();
    let snapshot = tenant.snapshot().await?;
    // The graph loads on the tenant's first use. If its stores can't be read (Neo4j down), this
    // search answers from the wiki alone, with an empty relations slot.
    let graph = tenant.graph_or_empty().await;
    let reads = scope.read_state(req.session.as_deref());
    let response =
        crate::search::search(&snapshot, &graph, &scope, &req, &app.config.search, &reads)?;

    let mut record = scope.trace_record(Endpoint::Search);
    record.snapshot = Some(snapshot.version.clone());
    record.graph = Some(graph.version.clone());
    record.search_config = app.search_fingerprint.clone();
    record.request = serde_json::to_value(&req).unwrap_or_default();
    record.result = response.trace_summary();
    record.timings_us = response
        .timings_us
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    record.timings_us.insert(
        "total".into(),
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
    app.traces.record(record);
    Ok(Json(response))
}
