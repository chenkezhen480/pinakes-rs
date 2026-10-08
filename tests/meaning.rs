//! Meaning-based search over HTTP with a stand-in embedding model (`common::embed::Fake`): page
//! vectors are stored in the tenant's SQLite file and kept across restarts, only changed parts
//! are embedded again, removed pages lose their vectors, and hits say how they were found.
//! Search quality with the real model is measured in `meaning_testset.rs`.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::embed::Fake;
use pinakes::store::{SqliteStore, Store};
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

fn page(path: &str, title: &str, aliases: &[&str], body: &str) -> Page {
    let mut p = Page::new(PagePath::parse(path).unwrap());
    p.front.title = Some(title.into());
    p.front.aliases = aliases.iter().map(|a| (*a).to_owned()).collect();
    p.front.summary = Some(format!("About {title}."));
    p.body = body.into();
    p
}

struct World {
    app: TestApp,
    fake: Arc<Fake>,
    key: String,
}

impl World {
    async fn new() -> Self {
        let fake = Fake::new();
        let mut app = TestApp::with_embedder("", fake.clone());
        let key = app.create_tenant("acme").await;
        let wiki = WikiFs::new(app.wiki_dir("acme"));
        wiki.write(&page(
            "expertise/returns",
            "Returns policy",
            &["退货"],
            "# Returns policy\n\nGoods may be returned within seven days.\n\n## Exceptions\n\nFresh food cannot be returned.\n",
        ))
        .unwrap();
        wiki.write(&page(
            "expertise/shipping",
            "Shipping times",
            &["配送"],
            "# Shipping times\n\nParcels arrive in two to four days.\n",
        ))
        .unwrap();
        app.restart();
        Self { app, fake, key }
    }

    async fn search(&self, body: Value) -> Value {
        let (status, resp) = self.app.post(Caller::new(&self.key), "/search", body).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        resp
    }

    fn store_file(&self) -> PathBuf {
        self.app
            .wiki_dir("acme")
            .parent()
            .unwrap()
            .join("pinakes.sqlite")
    }

    async fn stored(&self) -> Vec<(String, String)> {
        let store = SqliteStore::open_existing(&self.store_file()).unwrap();
        let mut rows: Vec<(String, String)> = store
            .embeddings("fake")
            .await
            .unwrap()
            .into_iter()
            .map(|v| (v.page, v.heading))
            .collect();
        rows.sort();
        rows
    }
}

fn hits(resp: &Value, slot: &str) -> Vec<(String, String)> {
    resp[slot]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["page"].as_str().unwrap().to_owned(),
                h["match"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn page_vectors_are_stored_and_only_changed_parts_embedded_again() {
    let mut w = World::new().await;
    w.search(json!({ "query": "anything", "mode": "semantic" }))
        .await;
    let first = w.fake.seen().len();
    let want = |page: &str, headings: &[&str]| -> Vec<(String, String)> {
        headings
            .iter()
            .map(|h| (page.to_owned(), (*h).to_owned()))
            .collect()
    };
    let mut expected = want("expertise/returns", &["", "Exceptions", "Returns policy"]);
    expected.extend(want("expertise/shipping", &["", "Shipping times"]));
    assert_eq!(w.stored().await, expected);
    // Five parts of pages, and the query once.
    let seen = w.fake.seen();
    assert_eq!(seen.iter().filter(|t| *t != "anything").count(), 5);
    assert_eq!(seen.iter().filter(|t| *t == "anything").count(), 1);
    assert_eq!(first, 6);

    // A restart reads the stored vectors: only the query is embedded.
    w.app.restart();
    w.search(json!({ "query": "anything", "mode": "semantic" }))
        .await;
    assert_eq!(w.fake.seen().len(), first + 1);

    // A hand edit to one section: that section alone is embedded again.
    let wiki = WikiFs::new(w.app.wiki_dir("acme"));
    let mut returns = wiki
        .read(&PagePath::parse("expertise/returns").unwrap())
        .unwrap();
    returns.body = returns.body.replace("Fresh food", "Fresh flowers");
    wiki.write(&returns).unwrap();
    w.app.restart();
    w.search(json!({ "query": "anything", "mode": "semantic" }))
        .await;
    let seen = w.fake.seen();
    let fresh: Vec<&String> = seen[first + 1..]
        .iter()
        .filter(|t| *t != "anything")
        .collect();
    assert_eq!(fresh.len(), 1, "{fresh:?}");
    assert!(fresh[0].contains("Fresh flowers"));

    // A removed page loses its vectors.
    std::fs::remove_file(w.app.wiki_dir("acme").join("expertise/shipping.md")).unwrap();
    w.app.restart();
    w.search(json!({ "query": "anything", "mode": "semantic" }))
        .await;
    assert_eq!(
        w.stored().await,
        want("expertise/returns", &["", "Exceptions", "Returns policy"])
    );
}

#[tokio::test]
async fn hits_say_how_they_were_found_in_each_mode() {
    let w = World::new().await;
    // Semantic: ranked by meaning alone (the fake model shares word pieces).
    let resp = w
        .search(json!({ "query": "returned goods within days", "mode": "semantic" }))
        .await;
    assert_eq!(resp["mode"], "semantic");
    assert_eq!(
        hits(&resp, "knowledge")[0],
        ("expertise/returns".into(), "meaning".into())
    );
    // A name in the query still pins its page, in every mode.
    let resp = w
        .search(json!({ "query": "shipping times", "mode": "semantic" }))
        .await;
    assert_eq!(
        hits(&resp, "knowledge")[0],
        ("expertise/shipping".into(), "alias".into())
    );
    // Both: a strong keyword hit keeps its place; meaning orders the rest.
    let resp = w
        .search(json!({ "query": "parcels arrive", "keywords": ["shipping"], "mode": "both" }))
        .await;
    assert_eq!(
        hits(&resp, "knowledge")[0],
        ("expertise/shipping".into(), "keyword".into())
    );
    // Auto: a strong keyword slot is left alone; a weak one is searched by meaning too.
    let resp = w
        .search(json!({ "query": "parcels arrive", "keywords": ["shipping"], "mode": "auto" }))
        .await;
    assert!(
        hits(&resp, "knowledge").iter().all(|(_, m)| m != "meaning"),
        "{resp}"
    );
    let resp = w
        .search(json!({ "query": "retourned goodz", "keywords": ["zzz"], "mode": "auto" }))
        .await;
    assert_eq!(
        hits(&resp, "knowledge")[0],
        ("expertise/returns".into(), "meaning".into())
    );
    // Keyword mode never embeds the query.
    let before = w.fake.seen().len();
    w.search(json!({ "query": "returned goods" })).await;
    assert_eq!(w.fake.seen().len(), before);
}

/// A model that always fails, as a broken model file would.
struct Broken;

impl pinakes::embed::Embedder for Broken {
    fn model(&self) -> &str {
        "broken"
    }

    fn dim(&self) -> usize {
        2
    }

    fn embed(&self, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("the model is broken")
    }
}

#[tokio::test]
async fn keyword_search_carries_on_when_page_vectors_fail() {
    let mut app = TestApp::with_embedder("", Arc::new(Broken));
    let key = app.create_tenant("acme").await;
    WikiFs::new(app.wiki_dir("acme"))
        .write(&page(
            "expertise/returns",
            "Returns policy",
            &[],
            "# Returns policy\n\nSeven days.\n",
        ))
        .unwrap();
    app.restart();
    let (status, resp) = app
        .post(
            Caller::new(&key),
            "/search",
            json!({ "query": "returns policy" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(hits(&resp, "knowledge")[0].0, "expertise/returns");
    let (status, resp) = app
        .post(
            Caller::new(&key),
            "/search",
            json!({ "query": "returns policy", "mode": "semantic" }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{resp}");
}

/// A model that isn't loaded yet, as `embed::Background` before its files are in.
struct Gated {
    fake: Arc<Fake>,
    ready: std::sync::atomic::AtomicBool,
}

impl pinakes::embed::Embedder for Gated {
    fn model(&self) -> &str {
        self.fake.model()
    }

    fn dim(&self) -> usize {
        self.fake.dim()
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::ensure!(self.ready(), "not loaded");
        self.fake.embed(texts)
    }

    fn ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// While the model loads, searches run on keywords and the meaning-based modes answer 503; once
/// it is in, the tenants opened meanwhile get their page vectors.
#[tokio::test]
async fn a_tenant_opened_while_the_model_loads_gets_its_vectors_after() {
    let gated = Arc::new(Gated {
        fake: Fake::new(),
        ready: false.into(),
    });
    let mut app = TestApp::with_embedder("", gated.clone());
    let key = app.create_tenant("acme").await;
    WikiFs::new(app.wiki_dir("acme"))
        .write(&page(
            "expertise/returns",
            "Returns policy",
            &[],
            "# Returns policy\n\nSeven days.\n",
        ))
        .unwrap();
    app.restart();
    let search = |mode: &'static str| {
        let app = &app;
        let key = &key;
        async move {
            app.post(
                Caller::new(key),
                "/search",
                json!({ "query": "returns policy", "mode": mode }),
            )
            .await
        }
    };
    assert_eq!(search("keyword").await.0, StatusCode::OK);
    let (status, resp) = search("semantic").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{resp}");
    assert!(
        gated.fake.seen().is_empty(),
        "nothing embedded before the model is in"
    );

    gated.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    for t in app.state.tenants.open_tenants().await {
        t.add_missing_vectors().await.unwrap();
    }
    let (status, resp) = search("semantic").await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(hits(&resp, "knowledge")[0].0, "expertise/returns");
}

/// With an embedding model, the librarian's related-page search ranks by meaning too: a record
/// whose words match no page still gets the page closest in meaning shown and offered as an
/// anchor. Without the model it doesn't.
#[tokio::test]
async fn the_librarian_finds_related_pages_by_meaning_too() {
    use common::llm::{FakeLlm, Step, all_ok, related_pages};
    for with_model in [true, false] {
        let llm = FakeLlm::new(|step, msg, _| match step {
            Step::Verify => all_ok(msg),
            _ => json!({ "edits": [] }).to_string(),
        });
        let mut app = if with_model {
            TestApp::with_llm_and_embedder(llm.llm(), Fake::new())
        } else {
            TestApp::with_llm(llm.llm())
        };
        let key = app.create_tenant("acme").await;
        WikiFs::new(app.wiki_dir("acme"))
            .write(&page(
                "expertise/returns",
                "Returns policy",
                &[],
                "# Returns policy\n\nGoods may be returned within seven days.\n",
            ))
            .unwrap();
        app.restart();
        let caller = Caller::new(&key).user(Some("ana"));
        let (status, queued) = app
            .post(
                caller,
                "/ingest/experience",
                json!({ "task": "retourned goodz", "outcome": "retourned goodz wiz zeven dayz" }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
        let job = app.wait_job(caller, queued["job"].as_str().unwrap()).await;
        assert_eq!(job["status"], "done", "{job}");
        let related = related_pages(&llm.calls(Step::Plan)[0]);
        assert_eq!(
            related.contains(&"expertise/returns".to_string()),
            with_model,
            "with model {with_model}: {related:?}"
        );
    }
}

/// Tenants opened without storing vectors (as `pinakes-replay` opens a live data folder) make
/// them in memory and leave the tenant's SQLite file alone.
#[tokio::test]
async fn tenants_can_build_vectors_without_storing_them() {
    let w = World::new().await;
    let tenants = pinakes::tenants::Tenants::new(
        w.app.config().tenants_dir(),
        w.app.config().store.file_name.clone(),
        std::time::Duration::from_secs(60),
    )
    .embedder(Some(w.fake.clone()))
    .read_only();
    let tenant = tenants.authenticate(&w.key).await.unwrap().unwrap();
    let snap = tenant.snapshot().await.unwrap();
    assert_eq!(snap.vectors.as_ref().map(|v| v.len()), Some(5));
    assert!(w.stored().await.is_empty(), "nothing was stored");
}
