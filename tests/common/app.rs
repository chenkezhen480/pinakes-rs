//! A service over a temporary data folder, driven through the axum `Router`.

#![allow(dead_code)]

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pinakes::config::Config;
use pinakes::llm::Llm;
use pinakes::tenants::tenant_dir_name;
use pinakes::{AppState, router};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

pub const ADMIN: &str = "test-admin-key";

pub struct TestApp {
    pub data: TempDir,
    pub router: Router,
    /// The state behind `router`, for startup work and flushing traces.
    pub state: AppState,
    /// Extra config TOML appended after the data folder.
    extra_config: String,
    /// The librarian's LLM, kept across restarts.
    llm: Option<Llm>,
}

/// Who is calling: an API key, plus the user and groups headers the host would send.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub key: &'a str,
    pub user: Option<&'a str>,
    pub groups: &'a [String],
}

impl<'a> Caller<'a> {
    pub fn new(key: &'a str) -> Self {
        Self {
            key,
            user: None,
            groups: &[],
        }
    }

    pub fn user(mut self, user: Option<&'a str>) -> Self {
        self.user = user;
        self
    }

    pub fn groups(mut self, groups: &'a [String]) -> Self {
        self.groups = groups;
        self
    }
}

impl TestApp {
    pub fn new() -> Self {
        Self::with_config("")
    }

    /// A service with extra config TOML, such as a `[search.ablation]` section.
    /// The librarian is the scripted [`super::llm::describer`], so schemas can be created.
    pub fn with_config(extra: &str) -> Self {
        Self::build(extra, Some(super::llm::describer().llm()))
    }

    /// A service whose librarian is off.
    pub fn without_llm() -> Self {
        Self::build("", None)
    }

    /// A service whose librarian calls the given LLM, such as a scripted one.
    pub fn with_llm(llm: Llm) -> Self {
        Self::build("", Some(llm))
    }

    fn build(extra: &str, llm: Option<Llm>) -> Self {
        let data = tempfile::tempdir().unwrap();
        let (router, state) = Self::make_router(&data, extra, llm.clone());
        Self {
            data,
            router,
            state,
            extra_config: extra.to_owned(),
            llm,
        }
    }

    pub fn config(&self) -> &Config {
        &self.state.config
    }

    fn make_router(data: &TempDir, extra: &str, llm: Option<Llm>) -> (Router, AppState) {
        let cfg = Config::from_toml_str(&format!(
            "[data]\ndir = {:?}\n{extra}",
            data.path().to_str().unwrap()
        ))
        .unwrap();
        std::fs::create_dir_all(cfg.tenants_dir()).unwrap();
        let state = AppState::with_llm(cfg, Some(ADMIN), llm);
        (router(state.clone()), state)
    }

    /// Starts a fresh service over the same data folder, as after a restart: tenants load
    /// again from their files, and so do their search snapshots.
    pub fn restart(&mut self) {
        (self.router, self.state) =
            Self::make_router(&self.data, &self.extra_config, self.llm.clone());
    }

    /// Restarts over the same data folder with other extra config TOML.
    pub fn restart_with_config(&mut self, extra: &str) {
        self.extra_config = extra.to_owned();
        self.restart();
    }

    pub fn wiki_dir(&self, tenant_id: &str) -> PathBuf {
        self.data
            .path()
            .join("tenants")
            .join(tenant_dir_name(tenant_id))
            .join("wiki")
    }

    pub async fn create_tenant(&self, id: &str) -> String {
        let (status, body) = self
            .post_admin("/admin/tenants", json!({ "tenant_id": id }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["api_key"].as_str().unwrap().to_owned()
    }

    /// A POST with the admin key.
    pub async fn post_admin(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::post(uri)
            .header("Authorization", format!("Bearer {ADMIN}"))
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.send(req).await
    }

    pub async fn post(&self, caller: Caller<'_>, uri: &str, body: Value) -> (StatusCode, Value) {
        let mut req = Request::post(uri)
            .header("Authorization", format!("Bearer {}", caller.key))
            .header("Content-Type", "application/json");
        if let Some(u) = caller.user {
            req = req.header("X-User-Id", u);
        }
        if !caller.groups.is_empty() {
            req = req.header("X-User-Groups", caller.groups.join(","));
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    /// A PUT with a JSON body.
    pub async fn put(&self, caller: Caller<'_>, uri: &str, body: Value) -> (StatusCode, Value) {
        let mut req = Request::put(uri)
            .header("Authorization", format!("Bearer {}", caller.key))
            .header("Content-Type", "application/json");
        if let Some(u) = caller.user {
            req = req.header("X-User-Id", u);
        }
        if !caller.groups.is_empty() {
            req = req.header("X-User-Groups", caller.groups.join(","));
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    /// Waits until a job has finished and returns it.
    /// Long enough for jobs that call a real model.
    pub async fn wait_job(&self, caller: Caller<'_>, job: &str) -> Value {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(600) {
            let (status, body) = self.get(caller, &format!("/jobs/{job}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            if body["status"] == "done" || body["status"] == "failed" {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("job {job} did not finish");
    }

    pub async fn get(&self, caller: Caller<'_>, uri: &str) -> (StatusCode, Value) {
        let mut req = Request::get(uri).header("Authorization", format!("Bearer {}", caller.key));
        if let Some(u) = caller.user {
            req = req.header("X-User-Id", u);
        }
        if !caller.groups.is_empty() {
            req = req.header("X-User-Groups", caller.groups.join(","));
        }
        self.send(req.body(Body::empty()).unwrap()).await
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, value)
    }
}
