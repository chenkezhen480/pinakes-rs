//! HTTP handlers. Every tenant route takes a `Scope`; admin routes take `Admin`.

mod admin;
mod pages;

use axum::Router;
use axum::routing::{get, post};

use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/pages", get(pages::list))
        .route("/pages/{*path}", get(pages::get))
        .route("/admin/tenants", post(admin::create_tenant))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}
