//! HTTP handlers. Every tenant route takes a `Scope`; admin routes take `Admin`.

pub(crate) mod acquisition;
mod admin;
mod browse;
mod delete;
pub(crate) mod draft;
mod embedding;
pub(crate) mod graph;
pub(crate) mod ingest;
pub(crate) mod insert;
mod jobs;
pub(crate) mod lint;
mod pages;
pub(crate) mod read;
pub(crate) mod search;
mod settings;
mod status;
#[cfg(test)]
mod types;

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
        .route("/", get(crate::console::root))
        .route("/ui", get(crate::console::root))
        .route("/ui/", get(crate::console::index))
        .route("/ui/{*path}", get(crate::console::asset))
        .route("/console/session", get(crate::console::session))
        .route("/console/sign-in", post(crate::console::sign_in))
        .route("/console/sign-out", post(crate::console::sign_out))
        .route(
            "/console/account",
            get(crate::console::account).put(crate::console::change_account),
        )
        .route("/status", get(status::status))
        .route("/pages", get(pages::list))
        .route("/pages/{*path}", get(pages::get).put(pages::put))
        .route("/history/{*path}", get(pages::history))
        .route("/search", post(search::search))
        .route("/read", post(read::read))
        .route("/graph", post(graph::graph))
        .route("/graph/path", post(graph::path))
        .route("/graph/draft", get(draft::get).post(draft::propose))
        .route("/graph/schemas", get(browse::schemas))
        .route("/graph/spaces", get(browse::spaces))
        .route("/graph/nodes", get(browse::nodes))
        .route("/ingest/experience", post(insert::experience))
        .route("/ingest/decision", post(insert::decision))
        // A document can be long.
        .route(
            "/ingest/expertise",
            // Room for an uploaded file (the converter takes up to 20 MB).
            post(insert::expertise).layer(axum::extract::DefaultBodyLimit::max(21 * 1024 * 1024)),
        )
        .route("/ingest/preference", post(insert::preference))
        .route("/jobs", get(jobs::list_jobs))
        .route("/jobs/{id}", get(jobs::get))
        .route("/proposals", get(jobs::list))
        .route("/proposals/{id}/approve", post(jobs::approve))
        .route("/proposals/{id}/reject", post(jobs::reject))
        .route("/acquisition", get(acquisition::status))
        .route(
            "/admin/tenants/{id}/acquisition",
            axum::routing::put(acquisition::put_settings),
        )
        .route("/lint", get(lint::report).post(lint::run))
        .route("/ingest/graph/schema", post(ingest::schema))
        .route("/ingest/graph/space", post(ingest::space))
        // A full batch (`graph::def::MAX_BATCH` nodes and edges) is well over axum's 2 MB default.
        .route(
            "/ingest/graph/data",
            post(ingest::data).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/users/{user_id}", delete(delete::user))
        .route(
            "/admin/tenants",
            get(admin::list_tenants).post(admin::create_tenant),
        )
        .route("/admin/tenants/{id}/audit", get(admin::tenant_audit))
        .route("/admin/settings", get(settings::get).put(settings::put))
        .route("/admin/environment", get(settings::environment))
        .route(
            "/admin/model",
            get(settings::get_model).put(settings::put_model),
        )
        .route("/admin/model/test", post(settings::test_model))
        .route("/admin/audit", get(settings::audit))
        .route("/admin/embedding", get(embedding::get).put(embedding::put))
        .route("/admin/embedding/files", delete(embedding::delete_files))
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
