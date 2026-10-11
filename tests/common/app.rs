//! A service over a temporary data folder, driven through the axum `Router`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use pinakes::config::Config;
use pinakes::embed::{Embedder, Switch};
use pinakes::llm::Llm;
use pinakes::tenants::tenant_dir_name;
use pinakes::{AppState, router};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

pub const ADMIN: &str = "test-admin-key";

pub struct TestApp {
    pub router: Router,
    /// The state behind `router`, for startup work and flushing traces.
    pub state: AppState,
    /// Extra config TOML appended after the data folder.
    extra_config: String,
    /// The librarian's LLM, kept across restarts.
    llm: Option<Llm>,
    /// The embedding model, kept across restarts.
    embedder: Option<Arc<dyn Embedder>>,
    /// Or the embedding switch, turned on by `[embedding] enabled`, kept across restarts.
    switch: Option<Arc<Switch>>,
    /// Whether the config is read from a file, `<data>/config.toml`, as the service reads
    /// its own (so settings can be written back to it).
    from_file: bool,
    /// Last, so it is dropped after the service that holds its files open (Windows can't
    /// delete open files).
    pub data: TempDir,
}

/// Who is calling: an API key, plus the user, groups and audience headers the host would send.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub key: &'a str,
    pub user: Option<&'a str>,
    pub groups: &'a [String],
    pub audience: Option<&'a str>,
}

impl<'a> Caller<'a> {
    pub fn new(key: &'a str) -> Self {
        Self {
            key,
            user: None,
            groups: &[],
            audience: None,
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

    /// Who an insert is for: `tenant`, `user` or `group:<id>`.
    pub fn audience(mut self, audience: &'a str) -> Self {
        self.audience = Some(audience);
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

    /// A service whose librarian calls the given LLM, with extra config TOML.
    pub fn with_llm_and_config(llm: Llm, extra: &str) -> Self {
        Self::build(extra, Some(llm))
    }

    /// A service with extra config TOML whose pages and queries are embedded by `embedder`.
    pub fn with_embedder(extra: &str, embedder: Arc<dyn Embedder>) -> Self {
        Self::build_with(extra, Some(super::llm::describer().llm()), Some(embedder))
    }

    /// A service whose librarian calls `llm` and whose pages and queries `embedder` embeds.
    pub fn with_llm_and_embedder(llm: Llm, embedder: Arc<dyn Embedder>) -> Self {
        Self::build_with("", Some(llm), Some(embedder))
    }

    /// A service with extra config TOML whose embedding model sits behind `switch`, as the
    /// service's own does: off unless the config turns it on, then turned on and off at
    /// `/admin/embedding`.
    pub fn with_switch(extra: &str, switch: Arc<Switch>) -> Self {
        let data = tempfile::tempdir().unwrap();
        let llm = Some(super::llm::describer().llm());
        let (router, state) =
            Self::make_router(&data, extra, llm.clone(), None, Some(switch.clone()), false);
        Self {
            data,
            router,
            state,
            extra_config: extra.to_owned(),
            llm,
            embedder: None,
            switch: Some(switch),
            from_file: false,
        }
    }

    /// A service whose config is read from `<data>/config.toml` (written from `extra`), with
    /// the scripted librarian; its model file is `<data>/model.conf`.
    pub fn with_config_file(extra: &str) -> Self {
        let data = tempfile::tempdir().unwrap();
        let llm = Some(super::llm::describer().llm());
        let (router, state) = Self::make_router(&data, extra, llm.clone(), None, None, true);
        Self {
            router,
            state,
            extra_config: extra.to_owned(),
            llm,
            embedder: None,
            switch: None,
            from_file: true,
            data,
        }
    }

    fn build(extra: &str, llm: Option<Llm>) -> Self {
        Self::build_with(extra, llm, None)
    }

    fn build_with(extra: &str, llm: Option<Llm>, embedder: Option<Arc<dyn Embedder>>) -> Self {
        let data = tempfile::tempdir().unwrap();
        let (router, state) =
            Self::make_router(&data, extra, llm.clone(), embedder.clone(), None, false);
        Self {
            data,
            router,
            state,
            extra_config: extra.to_owned(),
            llm,
            embedder,
            switch: None,
            from_file: false,
        }
    }

    pub fn config(&self) -> &Config {
        &self.state.config
    }

    fn make_router(
        data: &TempDir,
        extra: &str,
        llm: Option<Llm>,
        embedder: Option<Arc<dyn Embedder>>,
        switch: Option<Arc<Switch>>,
        from_file: bool,
    ) -> (Router, AppState) {
        let text = format!("[data]\ndir = {:?}\n{extra}", data.path().to_str().unwrap());
        let mut cfg = if from_file {
            let file = data.path().join("config.toml");
            if !file.exists() {
                std::fs::write(&file, &text).unwrap();
            }
            Config::load(&file).unwrap()
        } else {
            Config::from_toml_str(&text).unwrap()
        };
        cfg.load_jwt_key(std::path::Path::new(".")).unwrap();
        std::fs::create_dir_all(cfg.tenants_dir()).unwrap();
        let state = match switch {
            Some(switch) => {
                let llm = llm.ok_or(pinakes::llm::Missing::Config("no model".into()));
                AppState::with_switch(cfg, Some(ADMIN), llm, Some(switch))
            }
            None => AppState::with_models(cfg, Some(ADMIN), llm, embedder),
        };
        (router(state.clone()), state)
    }

    /// Starts a fresh service over the same data folder, as after a restart: tenants load
    /// again from their files, and so do their search snapshots.
    pub fn restart(&mut self) {
        (self.router, self.state) = Self::make_router(
            &self.data,
            &self.extra_config,
            self.llm.clone(),
            self.embedder.clone(),
            self.switch.clone(),
            self.from_file,
        );
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
        req = with_caller(req, caller);
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    /// A PUT with a JSON body.
    pub async fn put(&self, caller: Caller<'_>, uri: &str, body: Value) -> (StatusCode, Value) {
        let mut req = Request::put(uri)
            .header("Authorization", format!("Bearer {}", caller.key))
            .header("Content-Type", "application/json");
        req = with_caller(req, caller);
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    /// A DELETE as `caller`.
    pub async fn delete(&self, caller: Caller<'_>, uri: &str) -> (StatusCode, Value) {
        let mut req =
            Request::delete(uri).header("Authorization", format!("Bearer {}", caller.key));
        req = with_caller(req, caller);
        self.send(req.body(Body::empty()).unwrap()).await
    }

    /// A GET with the admin key.
    pub async fn get_admin(&self, uri: &str) -> (StatusCode, Value) {
        self.get(Caller::new(ADMIN), uri).await
    }

    /// A PUT with the admin key.
    pub async fn put_admin(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.put(Caller::new(ADMIN), uri, body).await
    }

    /// A DELETE with the admin key.
    pub async fn delete_admin(&self, uri: &str) -> (StatusCode, Value) {
        self.delete(Caller::new(ADMIN), uri).await
    }

    /// The tenant's folder.
    pub fn tenant_dir(&self, tenant_id: &str) -> PathBuf {
        self.data
            .path()
            .join("tenants")
            .join(tenant_dir_name(tenant_id))
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
        req = with_caller(req, caller);
        self.send(req.body(Body::empty()).unwrap()).await
    }

    /// A `multipart/form-data` POST: text fields, and a file as (name, bytes).
    pub async fn post_form(
        &self,
        caller: Caller<'_>,
        uri: &str,
        fields: &[(&str, &str)],
        file: Option<(&str, &[u8])>,
    ) -> (StatusCode, Value) {
        const BOUNDARY: &str = "pinakes-test-boundary";
        let mut body: Vec<u8> = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        if let Some((file_name, bytes)) = file {
            body.extend_from_slice(
                format!(
                    "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let mut req = Request::post(uri)
            .header("Authorization", format!("Bearer {}", caller.key))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            );
        req = with_caller(req, caller);
        self.send(req.body(Body::from(body)).unwrap()).await
    }

    /// Any request, answered with its status, headers and body as text.
    pub async fn send_raw(&self, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
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

/// A request with the caller's identity and audience headers (the key is set by each method).
fn with_caller(
    mut req: axum::http::request::Builder,
    caller: Caller<'_>,
) -> axum::http::request::Builder {
    if let Some(u) = caller.user {
        req = req.header("X-User-Id", u);
    }
    if !caller.groups.is_empty() {
        req = req.header("X-User-Groups", caller.groups.join(","));
    }
    if let Some(a) = caller.audience {
        req = req.header("X-Audience", a);
    }
    req
}
