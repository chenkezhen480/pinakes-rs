//! M1 "done when": two tenants' pages and SQLite files are completely invisible to each other
//! over HTTP. Also covers user isolation inside a tenant and hostile paths and keys.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pinakes::config::Config;
use pinakes::store::{SqliteStore, Store};
use pinakes::tenants::{group_dir_name, hash_api_key, tenant_dir_name, user_dir_name};
use pinakes::wiki::{Page, PagePath, WikiFs};
use pinakes::{AppState, router};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const ADMIN: &str = "test-admin-key";

struct TestApp {
    data: TempDir,
    router: Router,
}

struct Call<'a> {
    method: &'a str,
    uri: &'a str,
    key: Option<&'a str>,
    user: Option<&'a str>,
    groups: Option<&'a str>,
    body: Option<Value>,
}

impl<'a> Call<'a> {
    fn get(uri: &'a str) -> Self {
        Self {
            method: "GET",
            uri,
            key: None,
            user: None,
            groups: None,
            body: None,
        }
    }
    fn post(uri: &'a str, body: Value) -> Self {
        Self {
            method: "POST",
            uri,
            key: None,
            user: None,
            groups: None,
            body: Some(body),
        }
    }
    fn key(mut self, key: &'a str) -> Self {
        self.key = Some(key);
        self
    }
    fn groups(mut self, groups: &'a str) -> Self {
        self.groups = Some(groups);
        self
    }
    fn user(mut self, user: &'a str) -> Self {
        self.user = Some(user);
        self
    }
}

impl TestApp {
    fn new() -> Self {
        Self::with_admin(Some(ADMIN))
    }

    fn with_admin(admin: Option<&str>) -> Self {
        let data = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[data]\ndir = {:?}\n",
            data.path().to_str().unwrap()
        ))
        .unwrap();
        std::fs::create_dir_all(cfg.tenants_dir()).unwrap();
        let router = router(AppState::new(cfg, admin));
        Self { data, router }
    }

    async fn call(&self, c: Call<'_>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(c.method).uri(c.uri);
        if let Some(k) = c.key {
            req = req.header("Authorization", format!("Bearer {k}"));
        }
        if let Some(g) = c.groups {
            req = req.header("X-User-Groups", g);
        }
        if let Some(u) = c.user {
            req = req.header("X-User-Id", u);
        }
        let body = match c.body {
            Some(v) => {
                req = req.header("Content-Type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self
            .router
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, value)
    }

    async fn create_tenant(&self, id: &str) -> String {
        let (status, body) = self
            .call(Call::post("/admin/tenants", json!({ "tenant_id": id })).key(ADMIN))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["api_key"].as_str().unwrap().to_owned()
    }

    fn tenant_dir(&self, tenant_id: &str) -> std::path::PathBuf {
        self.data
            .path()
            .join("tenants")
            .join(tenant_dir_name(tenant_id))
    }

    /// Seeds a page straight into a tenant's files (the librarian arrives in M4).
    fn seed(&self, tenant_id: &str, path: &str, title: &str, body: &str) {
        let wiki = WikiFs::new(self.tenant_dir(tenant_id).join("wiki"));
        let mut page = Page::new(PagePath::parse(path).unwrap());
        page.front.title = Some(title.into());
        page.front.summary = Some(format!("summary of {title}"));
        page.body = body.into();
        wiki.write(&page).unwrap();
    }
}

fn paths(list: &Value) -> Vec<String> {
    list["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["page"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn health_needs_no_credentials() {
    let app = TestApp::new();
    let (status, body) = app.call(Call::get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn two_tenants_never_see_each_other() {
    let app = TestApp::new();
    let key_a = app.create_tenant("acme").await;
    let key_b = app.create_tenant("globex").await;

    // The same path in both tenants, plus one page only each has.
    app.seed("acme", "expertise/refunds", "Acme refunds", "ACME-SECRET\n");
    app.seed(
        "globex",
        "expertise/refunds",
        "Globex refunds",
        "GLOBEX-SECRET\n",
    );
    app.seed("acme", "expertise/acme-only", "Acme only", "a\n");
    app.seed("globex", "experience/globex-only", "Globex only", "b\n");

    let (s, list_a) = app.call(Call::get("/pages").key(&key_a)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        paths(&list_a),
        vec!["expertise/acme-only", "expertise/refunds", "index", "log"]
    );
    let (_, list_b) = app.call(Call::get("/pages").key(&key_b)).await;
    assert_eq!(
        paths(&list_b),
        vec![
            "experience/globex-only",
            "expertise/refunds",
            "index",
            "log"
        ]
    );

    // Shared path: each key reads its own content only.
    let (_, page_a) = app
        .call(Call::get("/pages/expertise/refunds").key(&key_a))
        .await;
    assert_eq!(page_a["body"], "ACME-SECRET\n");
    let (_, page_b) = app
        .call(Call::get("/pages/expertise/refunds").key(&key_b))
        .await;
    assert_eq!(page_b["body"], "GLOBEX-SECRET\n");

    // Pages only the other tenant has are simply not found.
    let (s, _) = app
        .call(Call::get("/pages/experience/globex-only").key(&key_a))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .call(Call::get("/pages/expertise/acme-only").key(&key_b))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Nothing in either response mentions the other tenant.
    for (resp, other) in [(&list_a, "Globex"), (&list_b, "Acme")] {
        assert!(!resp.to_string().contains(other), "{resp}");
    }
}

#[tokio::test]
async fn sqlite_files_are_separate() {
    let app = TestApp::new();
    let key_a = app.create_tenant("acme").await;
    let key_b = app.create_tenant("globex").await;

    for (tenant, own, other) in [("acme", &key_a, &key_b), ("globex", &key_b, &key_a)] {
        let file = app.tenant_dir(tenant).join("pinakes.sqlite");
        let store = SqliteStore::open_existing(&file).unwrap();
        assert_eq!(store.tenant_id().await.unwrap().as_deref(), Some(tenant));
        let hashes = store.api_key_hashes().await.unwrap();
        assert_eq!(hashes, vec![hash_api_key(own)]);
        assert!(!hashes.contains(&hash_api_key(other)));
        let log = store.audit_log(10).await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].action, "tenant.create");
    }
}

#[tokio::test]
async fn forged_and_malformed_keys_are_rejected() {
    let app = TestApp::new();
    let key_a = app.create_tenant("acme").await;
    let _key_b = app.create_tenant("globex").await;
    app.seed("globex", "expertise/x", "X", "x\n");

    // A's secret routed to B's folder.
    let b_hash = &tenant_dir_name("globex")[2..];
    let secret = key_a.rsplit('_').next().unwrap();
    let forged = format!("pnx_{b_hash}_{secret}");
    // B's folder with a made-up secret.
    let guessed = format!("pnx_{b_hash}_{}", "0".repeat(64));
    // Well-formed key for a tenant that doesn't exist.
    let ghost = format!("pnx_{}_{}", &tenant_dir_name("nobody")[2..], "0".repeat(64));

    for key in [forged.as_str(), &guessed, &ghost, "pnx_garbage", ADMIN] {
        let (s, body) = app.call(Call::get("/pages").key(key)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{key}: {body}");
    }
    let (s, _) = app.call(Call::get("/pages")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // A probe for an unknown tenant must not create anything on disk.
    assert!(
        !app.data
            .path()
            .join("tenants")
            .join(tenant_dir_name("nobody"))
            .exists()
    );
}

#[tokio::test]
async fn users_inside_a_tenant_are_isolated() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let (alice, bob) = (user_dir_name("alice"), user_dir_name("bob"));
    app.seed(
        "acme",
        &format!("users/{alice}/preferences"),
        "Alice prefs",
        "tea\n",
    );
    app.seed(
        "acme",
        &format!("users/{bob}/preferences"),
        "Bob prefs",
        "coffee\n",
    );
    app.seed(
        "acme",
        &format!("users/{bob}/decisions/refund-policy"),
        "Bob decision",
        "d\n",
    );

    let (_, list) = app.call(Call::get("/pages").key(&key).user("alice")).await;
    let alice_pages = paths(&list);
    assert!(alice_pages.contains(&format!("users/{alice}/preferences")));
    assert!(
        !alice_pages.iter().any(|p| p.contains(&bob)),
        "{alice_pages:?}"
    );

    // Without a user, no user pages at all.
    let (_, list) = app.call(Call::get("/pages").key(&key)).await;
    assert!(!paths(&list).iter().any(|p| p.starts_with("users/")));

    // Bob's pages are not found for Alice or for an anonymous caller.
    for uri in [
        format!("/pages/users/{bob}/preferences"),
        format!("/pages/users/{bob}/decisions/refund-policy"),
    ] {
        let (s, _) = app.call(Call::get(&uri).key(&key).user("alice")).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
        let (s, _) = app.call(Call::get(&uri).key(&key)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
    let (s, page) = app
        .call(
            Call::get(&format!("/pages/users/{bob}/preferences"))
                .key(&key)
                .user("bob"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["body"], "coffee\n");
    assert_eq!(page["kind"], "preference");

    let (_, list) = app
        .call(Call::get("/pages?kind=decision").key(&key).user("bob"))
        .await;
    assert_eq!(
        paths(&list),
        vec![format!("users/{bob}/decisions/refund-policy")]
    );
}

#[tokio::test]
async fn same_user_id_in_two_tenants_is_two_users() {
    let app = TestApp::new();
    let key_a = app.create_tenant("acme").await;
    let key_b = app.create_tenant("globex").await;
    let alice = user_dir_name("alice");
    app.seed(
        "acme",
        &format!("users/{alice}/preferences"),
        "Acme Alice",
        "a\n",
    );

    let uri = format!("/pages/users/{alice}/preferences");
    let (s, _) = app.call(Call::get(&uri).key(&key_a).user("alice")).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = app.call(Call::get(&uri).key(&key_b).user("alice")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hostile_paths_never_escape() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    // A file outside the wiki folder that a traversal would aim for.
    std::fs::write(app.tenant_dir("acme").join("secret.md"), "TOP-SECRET").unwrap();

    for uri in [
        "/pages/..%2Fsecret",
        "/pages/expertise%2F..%2F..%2Fsecret",
        "/pages/expertise/..%2F..%2Fsecret",
        "/pages/expertise/%2E%2E",
        "/pages/expertise/..%5C..%5Csecret",
        "/pages/sources/raw",
        "/pages/users/alice/preferences",
        "/pages/C:%2Fwindows",
    ] {
        let (s, body) = app.call(Call::get(uri).key(&key)).await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND,
            "{uri}: {s} {body}"
        );
        assert!(!body.to_string().contains("TOP-SECRET"), "{uri}");
    }
}

#[tokio::test]
async fn hostile_tenant_ids_stay_inside_the_data_folder() {
    let app = TestApp::new();
    for id in ["../escape", "..\\escape", "/abs", "C:\\x", "a/b"] {
        let key = app.create_tenant(id).await;
        let (s, _) = app.call(Call::get("/pages").key(&key)).await;
        assert_eq!(s, StatusCode::OK);
    }
    let names: Vec<String> = std::fs::read_dir(app.data.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["tenants"]);
    for e in std::fs::read_dir(app.data.path().join("tenants")).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        assert!(name.starts_with("t_") && name.len() == 18, "{name}");
    }
}

#[tokio::test]
async fn admin_routes_need_the_admin_key() {
    let app = TestApp::new();
    let body = json!({ "tenant_id": "acme" });
    let (s, _) = app.call(Call::post("/admin/tenants", body.clone())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app
        .call(Call::post("/admin/tenants", body.clone()).key("wrong"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let key = app.create_tenant("acme").await;
    // A tenant key is not an admin key.
    let (s, _) = app
        .call(Call::post("/admin/tenants", json!({ "tenant_id": "other" })).key(&key))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Creating the same tenant twice conflicts.
    let (s, _) = app
        .call(Call::post("/admin/tenants", body).key(ADMIN))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = app
        .call(Call::post("/admin/tenants", json!({ "tenant_id": "" })).key(ADMIN))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // With no admin key configured, admin routes are closed to everyone.
    let closed = TestApp::with_admin(None);
    let (s, _) = closed
        .call(Call::post("/admin/tenants", json!({ "tenant_id": "x" })).key(""))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bad_user_header_is_rejected() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let long = "x".repeat(300);
    let (s, _) = app.call(Call::get("/pages").key(&key).user(&long)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn request_errors_use_the_json_error_shape() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;

    let (s, body) = app.call(Call::get("/pages?kind=nonsense").key(&key)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request", "{body}");

    let (s, body) = app
        .call(Call::post("/admin/tenants", json!({ "wrong_field": 1 })).key(ADMIN))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request", "{body}");
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let req = Request::builder()
        .uri("/pages")
        .header("Authorization", format!("bearer {key}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn groups_see_only_their_own_pages() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let (risk, ops) = (group_dir_name("risk-team"), group_dir_name("ops"));
    let risk_page = format!("groups/{risk}/decisions/bin-blocks");
    let ops_page = format!("groups/{ops}/experience/playbook");
    app.seed("acme", &risk_page, "BIN blocks", "RISK-ONLY\n");
    app.seed("acme", &ops_page, "Ops playbook", "OPS-ONLY\n");

    // A member of risk-team sees its pages and nothing of ops.
    let (_, list) = app
        .call(
            Call::get("/pages")
                .key(&key)
                .user("marco")
                .groups("risk-team"),
        )
        .await;
    let seen = paths(&list);
    assert!(seen.contains(&risk_page), "{seen:?}");
    assert!(!seen.contains(&ops_page), "{seen:?}");
    let (s, page) = app
        .call(
            Call::get(&format!("/pages/{risk_page}"))
                .key(&key)
                .groups("risk-team"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(page["kind"], "decision");
    let (s, _) = app
        .call(
            Call::get(&format!("/pages/{ops_page}"))
                .key(&key)
                .groups("risk-team"),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // No groups: no group pages at all. Both groups: both pages.
    let (_, list) = app.call(Call::get("/pages").key(&key).user("marco")).await;
    assert!(!paths(&list).iter().any(|p| p.starts_with("groups/")));
    let (s, _) = app
        .call(
            Call::get(&format!("/pages/{risk_page}"))
                .key(&key)
                .user("marco"),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, list) = app
        .call(Call::get("/pages").key(&key).groups("ops, risk-team"))
        .await;
    let seen = paths(&list);
    assert!(
        seen.contains(&risk_page) && seen.contains(&ops_page),
        "{seen:?}"
    );

    // Group folders are named by hash; a raw group name in a path is not a wiki location.
    let (s, _) = app
        .call(
            Call::get("/pages/groups/risk-team/decisions/bin-blocks")
                .key(&key)
                .groups("risk-team"),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn malformed_group_header_is_rejected() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let too_many = (0..65)
        .map(|i| format!("g{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let (s, body) = app
        .call(Call::get("/pages").key(&key).groups(&too_many))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let long = "x".repeat(300);
    let (s, _) = app.call(Call::get("/pages").key(&key).groups(&long)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
