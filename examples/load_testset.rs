//! Loads the six `testset/` scenarios into a data folder so the service can be tried by hand:
//!
//! ```text
//! cargo run --example load_testset -- ./data [config.toml]
//! cargo run
//! ```
//!
//! Each scenario becomes a tenant with the scenario's name: its wiki pages are copied in, and
//! its graph schemas, spaces and data go through `/ingest/graph/*` as a host would send them.
//! The graph backend comes from the config file when one is given (use the same file for
//! `cargo run`), otherwise SQLite. API keys and users are printed. Scenarios that already exist
//! in the folder are skipped.

#[path = "../tests/common/testset.rs"]
mod testset;

use std::path::PathBuf;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use pinakes::AppState;
use pinakes::config::Config;
use pinakes::tenants::CreateTenantError;
use tower::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let data = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data"));
    let mut config = match args.next() {
        Some(path) => Config::load(std::path::Path::new(&path))?,
        None => Config::default(),
    };
    config.data.dir = data;
    std::fs::create_dir_all(config.tenants_dir())?;
    let state = AppState::new(config, None);
    state.prepare().await?;
    let app = pinakes::router(state.clone());

    for t in testset::tenants() {
        let created = match state.tenants.create(&t.id, "testset").await {
            Ok(created) => created,
            Err(CreateTenantError::AlreadyExists) => {
                println!("{:<11} already exists, skipped", t.id);
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        testset::install_wiki(&t.id, &created.tenant.dir.join("wiki"));
        created.tenant.reindex().await?;
        for call in testset::graph_calls(&t) {
            let mut req = Request::post(call.uri)
                .header("Authorization", format!("Bearer {}", created.api_key))
                .header("Content-Type", "application/json");
            if let Some(u) = &call.user {
                req = req.header("X-User-Id", u);
            }
            if !call.groups.is_empty() {
                req = req.header("X-User-Groups", call.groups.join(","));
            }
            let resp = app
                .clone()
                .oneshot(req.body(Body::from(call.body.to_string()))?)
                .await?;
            let status = resp.status();
            let body = resp.into_body().collect().await?.to_bytes();
            anyhow::ensure!(
                status.is_success(),
                "{} {}: {status} {}",
                t.id,
                call.uri,
                String::from_utf8_lossy(&body)
            );
        }
        println!("{:<11} key {}", t.id, created.api_key);
        for u in &t.users {
            let groups = if u.groups.is_empty() {
                String::new()
            } else {
                format!("  X-User-Groups: {}", u.groups.join(","))
            };
            println!("            X-User-Id: {}{groups}", u.id);
        }
    }
    Ok(())
}
