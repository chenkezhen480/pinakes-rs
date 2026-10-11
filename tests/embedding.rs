//! The embedding switch (design, "M10": vectors off by default): off at start, searches answer
//! by keywords and say so; turned on at `/admin/embedding` the model loads in the background
//! and semantic search works without a restart; turned off and on again, only pages changed
//! meanwhile are embedded. The model is a stand-in (`common::embed::Fake`) behind the switch.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::app::{Caller, TestApp};
use crate::common::embed::{FAKE_DIM, Fake};
use axum::http::StatusCode;
use pinakes::embed::{Embedder, Status, Switch};
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

const MODEL: &str = "[embedding]\nmodel = \"jina-embeddings-v2-base-zh\"\n";

fn page(path: &str, title: &str, body: &str) -> Page {
    let mut p = Page::new(PagePath::parse(path).unwrap());
    p.front.title = Some(title.into());
    p.front.summary = Some(format!("About {title}."));
    p.body = body.into();
    p
}

/// A switch whose loader counts its loads, reports a download step and hands over `fake`.
fn switch(fake: Arc<Fake>, loads: Arc<AtomicUsize>) -> Arc<Switch> {
    Arc::new(Switch::new(
        "fake",
        FAKE_DIM,
        Arc::new(move |progress| {
            loads.fetch_add(1, Ordering::SeqCst);
            progress(Status::Downloading {
                file: "onnx/model.onnx".into(),
                done_bytes: 1,
                total_bytes: 2,
                files_done: 4,
                files: 5,
            });
            Ok(fake.clone() as Arc<dyn Embedder>)
        }),
    ))
}

/// Waits until the switch's status is `state` and returns the status body.
async fn wait_for(app: &TestApp, state: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = app.get_admin("/admin/embedding").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["status"]["state"] == state {
            return body;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "still {body}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn search(app: &TestApp, key: &str, mode: &str) -> (StatusCode, Value) {
    app.post(
        Caller::new(key),
        "/search",
        json!({ "query": "how long may goods be returned", "mode": mode }),
    )
    .await
}

#[tokio::test]
async fn off_at_start_then_turned_on_and_off_without_a_restart() {
    let (fake, loads) = (Fake::new(), Arc::new(AtomicUsize::new(0)));
    let mut app = TestApp::with_switch(MODEL, switch(fake.clone(), loads.clone()));
    let key = app.create_tenant("acme").await;
    let wiki = WikiFs::new(app.wiki_dir("acme"));
    wiki.write(&page(
        "expertise/returns",
        "Returns policy",
        "# Returns policy\n\nGoods may be returned within seven days.\n",
    ))
    .unwrap();
    app.restart();

    // Off: nothing loaded, keyword search only, and it says so.
    let (status, body) = app.get_admin("/admin/embedding").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["status"]["state"], "off");
    assert_eq!(body["model"], "jina-embeddings-v2-base-zh");
    assert_eq!(body["files"]["bytes"], 0);
    let (status, body) = search(&app, &key, "semantic").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let why = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        why.contains("turned off") && why.contains("/admin/embedding"),
        "{body}"
    );
    let (status, body) = search(&app, &key, "auto").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["knowledge"]["hits"][0]["page"], "expertise/returns");
    assert!(
        body["note"]
            .as_str()
            .is_some_and(|n| n.contains("keywords only")),
        "{body}"
    );
    let (_, body) = search(&app, &key, "keyword").await;
    assert!(body.get("note").is_none(), "{body}");
    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert!(fake.seen().is_empty());

    // On: loaded in the background, the open tenant's pages embedded, semantic search works.
    let (status, body) = app
        .put_admin("/admin/embedding", json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    wait_for(&app, "on").await;
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(!fake.seen().is_empty(), "the page was embedded");
    let (status, body) = search(&app, &key, "semantic").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["knowledge"]["hits"][0]["page"], "expertise/returns");
    let (_, body) = search(&app, &key, "auto").await;
    assert!(body.get("note").is_none(), "{body}");
    // Turning it on again changes nothing.
    app.put_admin("/admin/embedding", json!({ "enabled": true }))
        .await;
    assert_eq!(wait_for(&app, "on").await["enabled"], true);
    assert_eq!(loads.load(Ordering::SeqCst), 1);

    // Off again: semantic search stops; a page added meanwhile gets no vector.
    let (status, body) = app
        .put_admin("/admin/embedding", json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"]["state"], "off");
    let (status, _) = search(&app, &key, "semantic").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    wiki.write(&page(
        "expertise/shipping",
        "Shipping times",
        "# Shipping times\n\nParcels arrive in two to four days.\n",
    ))
    .unwrap();
    app.restart();
    let (status, _) = search(&app, &key, "keyword").await;
    assert_eq!(status, StatusCode::OK);
    let embedded = fake.seen().len();

    // On once more: the stored vectors are kept, so only the new page is embedded.
    app.put_admin("/admin/embedding", json!({ "enabled": true }))
        .await;
    wait_for(&app, "on").await;
    assert_eq!(loads.load(Ordering::SeqCst), 2);
    let fresh = &fake.seen()[embedded..];
    assert!(!fresh.is_empty(), "the new page was embedded");
    assert!(
        fresh
            .iter()
            .all(|t| t.contains("Shipping") || t.contains("Parcels")),
        "only the new page: {fresh:?}"
    );
    let (status, body) = search(&app, &key, "semantic").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn only_the_admin_turns_it_on() {
    let (fake, loads) = (Fake::new(), Arc::new(AtomicUsize::new(0)));
    let app = TestApp::with_switch(MODEL, switch(fake, loads.clone()));
    let key = app.create_tenant("acme").await;
    let (status, _) = app.get(Caller::new(&key), "/admin/embedding").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app
        .put(
            Caller::new(&key),
            "/admin/embedding",
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(loads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn on_in_the_config_loads_at_start_and_a_default_mode_needing_it_keeps_it_on() {
    let (fake, loads) = (Fake::new(), Arc::new(AtomicUsize::new(0)));
    let app = TestApp::with_switch(
        &format!("[search]\ndefault_mode = \"both\"\n{MODEL}enabled = true\n"),
        switch(fake, loads.clone()),
    );
    wait_for(&app, "on").await;
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    let (status, body) = app
        .put_admin("/admin/embedding", json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(wait_for(&app, "on").await["enabled"], true);
}

#[tokio::test]
async fn a_model_that_cannot_be_loaded_says_why_and_can_be_tried_again() {
    let tries = Arc::new(AtomicUsize::new(0));
    let counted = tries.clone();
    let failing = Arc::new(Switch::new(
        "fake",
        FAKE_DIM,
        Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("huggingface.co can't be reached")
        }),
    ));
    let app = TestApp::with_switch(MODEL, failing);
    let key = app.create_tenant("acme").await;
    app.put_admin("/admin/embedding", json!({ "enabled": true }))
        .await;
    let body = wait_for(&app, "failed").await;
    assert_eq!(body["enabled"], false);
    assert!(
        body["status"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("can't be reached")),
        "{body}"
    );
    let (status, body) = search(&app, &key, "semantic").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("couldn't be loaded")),
        "{body}"
    );
    app.put_admin("/admin/embedding", json!({ "enabled": true }))
        .await;
    wait_for(&app, "failed").await;
    assert_eq!(tries.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_model_files_are_deleted_only_while_it_is_off() {
    let (fake, loads) = (Fake::new(), Arc::new(AtomicUsize::new(0)));
    let app = TestApp::with_switch(MODEL, switch(fake, loads));
    let files = app
        .data
        .path()
        .join("models/models--jinaai--jina-embeddings-v2-base-zh/blobs");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("weights"), vec![0u8; 1000]).unwrap();
    let (_, body) = app.get_admin("/admin/embedding").await;
    assert_eq!(body["files"]["bytes"], 1000);

    app.put_admin("/admin/embedding", json!({ "enabled": true }))
        .await;
    wait_for(&app, "on").await;
    let (status, body) = app.delete_admin("/admin/embedding/files").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(files.exists());

    app.put_admin("/admin/embedding", json!({ "enabled": false }))
        .await;
    let (status, body) = app.delete_admin("/admin/embedding/files").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["files"]["bytes"], 0);
    assert!(!files.exists());
}

/// A model that embeds pages but fails on one query, as if turned off mid-search.
struct GoneMidSearch(Arc<Fake>);

impl Embedder for GoneMidSearch {
    fn model(&self) -> &str {
        self.0.model()
    }
    fn dim(&self) -> usize {
        self.0.dim()
    }
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.len() == 1 && texts[0] == "lunar tides of the archive" {
            anyhow::bail!("the embedding model is off");
        }
        self.0.embed(texts)
    }
}

#[tokio::test]
async fn an_auto_search_answers_by_keywords_if_meaning_goes_away_midway() {
    let app = TestApp::with_embedder("", Arc::new(GoneMidSearch(Fake::new())));
    let key = app.create_tenant("acme").await;
    let wiki = WikiFs::new(app.wiki_dir("acme"));
    wiki.write(&page(
        "expertise/returns",
        "Returns",
        "# Returns

Goods may be returned within seven days.
",
    ))
    .unwrap();
    // Weak by keywords, so auto asks for meaning, which fails: keywords answer, and say so.
    let (status, body) = app
        .post(
            Caller::new(&key),
            "/search",
            json!({ "query": "lunar tides of the archive", "mode": "auto" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["note"]
            .as_str()
            .unwrap_or_default()
            .contains("keywords only"),
        "{body}"
    );
}
