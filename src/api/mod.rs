//! HTTP handlers. Every tenant route takes a `Scope`; admin routes take `Admin`.

mod admin;
mod graph;
mod ingest;
mod pages;
mod read;
mod search;

use axum::Router;
use axum::routing::{get, post};

use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/pages", get(pages::list))
        .route("/pages/{*path}", get(pages::get))
        .route("/search", post(search::search))
        .route("/read", post(read::read))
        .route("/graph", post(graph::graph))
        .route("/graph/path", post(graph::path))
        .route("/ingest/graph/schema", post(ingest::schema))
        .route("/ingest/graph/space", post(ingest::space))
        // A full batch (`graph::def::MAX_BATCH` nodes and edges) is well over axum's 2 MB default.
        .route(
            "/ingest/graph/data",
            post(ingest::data).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/admin/tenants", post(admin::create_tenant))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}
