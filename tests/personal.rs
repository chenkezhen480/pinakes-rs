//! Personal deployments (design, "Personal deployments"): the default tenant is created on
//! first start with its key in a file, and a request without the user header acts as the
//! default user, who has no admin rights.

use crate::common::app::{Caller, TestApp};
use axum::http::StatusCode;
use pinakes::PERSONAL_KEY_FILE;
use pinakes::tenants::user_dir_name;
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::json;

const PERSONAL: &str = "[identity]\ndeployment = \"personal\"\n";

fn seed_preferences(app: &TestApp, tenant: &str, user: &str, title: &str) {
    let wiki = WikiFs::new(app.wiki_dir(tenant));
    let path = format!("users/{}/preferences", user_dir_name(user));
    let mut page = Page::new(PagePath::parse(&path).unwrap());
    page.front.title = Some(title.into());
    page.front.aliases = vec!["reply style".into()];
    page.front.summary = Some(format!("{title}."));
    page.body = format!("# {title}\n\nShort answers, in English.\n");
    wiki.write(&page).unwrap();
}

#[tokio::test]
async fn first_start_creates_the_default_tenant_once() {
    let mut app = TestApp::with_config(PERSONAL);
    let key = app
        .state
        .prepare()
        .await
        .unwrap()
        .expect("created on first start");
    let file = app.config().data.dir.join(PERSONAL_KEY_FILE);
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), key);

    // Later starts keep the tenant and the key.
    app.restart();
    assert_eq!(app.state.prepare().await.unwrap(), None);
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), key);

    // A key is still required.
    let (status, _) = app
        .post(Caller::new("pnx_nope"), "/search", json!({ "query": "x" }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn no_user_header_means_the_default_user() {
    let mut app = TestApp::with_config(PERSONAL);
    let key = app.state.prepare().await.unwrap().unwrap();
    seed_preferences(&app, "000000", "admin", "Admin preferences");
    seed_preferences(&app, "000000", "bob", "Bob preferences");
    app.restart();

    let body = json!({ "query": "my reply style", "keywords": ["reply style"] });
    let pages = |resp: &serde_json::Value| -> Vec<String> {
        resp["memories"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["page"].as_str().unwrap().to_owned())
            .collect()
    };
    let admin_page = format!("users/{}/preferences", user_dir_name("admin"));
    let bob_page = format!("users/{}/preferences", user_dir_name("bob"));

    let (_, anon) = app.post(Caller::new(&key), "/search", body.clone()).await;
    assert_eq!(pages(&anon), vec![admin_page.clone()]);
    let (_, bob) = app
        .post(Caller::new(&key).user(Some("bob")), "/search", body)
        .await;
    assert_eq!(pages(&bob), vec![bob_page]);

    // `admin` is an ordinary user: admin routes still need the admin key.
    let (status, _) = app
        .post(
            Caller::new(&key),
            "/admin/tenants",
            json!({ "tenant_id": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn multi_tenant_deployments_keep_no_default_user() {
    let mut app = TestApp::new();
    assert_eq!(app.state.prepare().await.unwrap(), None);
    assert!(!app.config().data.dir.join(PERSONAL_KEY_FILE).exists());
    let key = app.create_tenant("acme").await;
    seed_preferences(&app, "acme", "admin", "Admin preferences");
    app.restart();
    let (_, resp) = app
        .post(
            Caller::new(&key),
            "/search",
            json!({ "query": "reply style", "keywords": ["reply style"] }),
        )
        .await;
    assert_eq!(resp["memories"]["hits"], json!([]));
}
