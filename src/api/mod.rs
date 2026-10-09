//! HTTP handlers. Every tenant route takes a `Scope`; admin routes take `Admin`.

mod admin;
mod delete;
pub(crate) mod graph;
mod ingest;
pub(crate) mod insert;
mod jobs;
mod lint;
mod pages;
pub(crate) mod read;
pub(crate) mod search;

use axum::Router;
use axum::routing::{delete, get, post};

use crate::AppState;

pub fn router(state: AppState) -> Router {
    let mcp = Router::new()
        .nest_service("/mcp", crate::mcp::service(state.clone()))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::mcp::authenticate,
        ));
    let api = Router::new()
        .route("/health", get(health))
        .route("/pages", get(pages::list))
        .route("/pages/{*path}", get(pages::get).put(pages::put))
        .route("/search", post(search::search))
        .route("/read", post(read::read))
        .route("/graph", post(graph::graph))
        .route("/graph/path", post(graph::path))
        .route("/ingest/experience", post(insert::experience))
        .route("/ingest/decision", post(insert::decision))
        // A document can be long.
        .route(
            "/ingest/expertise",
            // Room for an uploaded file (the converter takes up to 20 MB).
            post(insert::expertise).layer(axum::extract::DefaultBodyLimit::max(21 * 1024 * 1024)),
        )
        .route("/ingest/preference", post(insert::preference))
        .route("/jobs/{id}", get(jobs::get))
        .route("/proposals", get(jobs::list))
        .route("/proposals/{id}/approve", post(jobs::approve))
        .route("/lint", get(lint::report).post(lint::run))
        .route("/ingest/graph/schema", post(ingest::schema))
        .route("/ingest/graph/space", post(ingest::space))
        // A full batch (`graph::def::MAX_BATCH` nodes and edges) is well over axum's 2 MB default.
        .route(
            "/ingest/graph/data",
            post(ingest::data).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/users/{user_id}", delete(delete::user))
        .route("/admin/tenants", post(admin::create_tenant))
        .route("/admin/tenants/{id}", delete(delete::tenant))
        .route(
            "/admin/tenants/{id}/groups/{group_id}",
            delete(delete::group),
        )
        .with_state(state.clone())
        .merge(mcp);
    // The test chat, only when asked for (`PINAKES_DEV=1`).
    if crate::dev::enabled() {
        api.merge(crate::dev::router(state))
    } else {
        api
    }
}

async fn health() -> &'static str {
    "ok"
}
