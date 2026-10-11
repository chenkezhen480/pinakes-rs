//! The endpoints the console needs (design, "M10": APIs the console needs): the overview
//! (`/status`), a tenant's jobs, the tenant list, the settings explained and written back, the
//! environment, the model changed without a restart and tested, the audit log, browsing the
//! graph, pages rendered as HTML and a page's history.

use crate::common::app::{ADMIN, Caller, TestApp};
use axum::http::StatusCode;
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

fn page(path: &str, title: &str, body: &str) -> Page {
    let mut p = Page::new(PagePath::parse(path).unwrap());
    p.front.title = Some(title.into());
    p.front.summary = Some(format!("About {title}."));
    p.body = body.into();
    p
}

#[tokio::test]
async fn the_overview_says_what_runs_and_lists_the_jobs() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key).user(Some("lin"));
    let (status, s) = app.get(c, "/status").await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["caller"]["tenant"], "acme");
    assert_eq!(s["caller"]["user"], "lin");
    assert_eq!(s["librarian"]["on"], true);
    assert_eq!(s["embedding"]["status"]["state"], "off");
    assert_eq!(s["embedding"]["configured"], false);
    assert!(
        s["converters"]["builtin"]
            .as_array()
            .unwrap()
            .contains(&json!("docx"))
    );
    assert_eq!(s["converters"]["markitdown"], false);
    assert_eq!(s["graph"], "sqlite");
    // How the service runs is the admin's to see, not a tenant's.
    assert_eq!(s["local_only"], false);
    assert_eq!(s["dev"], false);
    assert!(s["librarian"]["model"].is_null(), "{s}");

    // Deleting a user runs as a job on the tenant's queue, which the job list shows.
    let (status, resp) = app.delete(Caller::new(&key), "/users/lin").await;
    assert!(status.is_success(), "{resp}");
    let (status, jobs) = app.get(Caller::new(&key), "/jobs").await;
    assert_eq!(status, StatusCode::OK, "{jobs}");
    let jobs = jobs["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0]["kind"], "delete_user");
    assert_eq!(jobs[0]["status"], "done");
    // A job queued without a user isn't listed to a user: it may name a group's pages.
    let (_, theirs) = app.get(Caller::new(&key).user(Some("mia")), "/jobs").await;
    assert_eq!(theirs["jobs"], json!([]));
    let other = app.create_tenant("other").await;
    let (_, none) = app.get(Caller::new(&other), "/jobs").await;
    assert_eq!(
        none["jobs"],
        json!([]),
        "another tenant's jobs are not listed"
    );
}

#[tokio::test]
async fn the_admin_lists_tenants_and_reads_the_audit_logs() {
    let mut app = TestApp::new();
    let key = app.create_tenant("acme").await;
    app.create_tenant("beta").await;
    let (status, list) = app.get_admin("/admin/tenants").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let ids: Vec<&str> = list["tenants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tenant_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["acme", "beta"]);
    // After a restart, listing reads the ids without opening the tenants.
    app.restart();
    let (_, again) = app.get_admin("/admin/tenants").await;
    assert_eq!(again["tenants"].as_array().unwrap().len(), 2, "{again}");
    assert!(app.state.tenants.open_tenants().await.is_empty());
    let (status, _) = app.get(Caller::new(&key), "/admin/tenants").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    app.delete_admin("/admin/tenants/beta").await;
    let (_, audit) = app.get_admin("/admin/audit").await;
    let actions: Vec<&str> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["tenant.delete", "tenant.create", "tenant.create"]);
    let (status, own) = app.get_admin("/admin/tenants/acme/audit").await;
    assert_eq!(status, StatusCode::OK, "{own}");
    assert_eq!(own["entries"][0]["action"], "tenant.create");
    let (status, _) = app.get_admin("/admin/tenants/beta/audit").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn settings_are_explained_and_written_back_to_the_file() {
    let app =
        TestApp::with_config_file("[server]   # where it listens\nport = 8080   # the port\n");
    let (status, view) = app.get_admin("/admin/settings").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["file"].as_str().unwrap().ends_with("config.toml"));
    let field = |v: &Value, path: &str| {
        v["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == path)
            .cloned()
            .unwrap_or_else(|| panic!("no {path}"))
    };
    let port = field(&view, "server.port");
    assert_eq!(port["default"], 8080);
    assert_eq!(port["value"], 8080);
    assert_eq!(port["kind"], "integer");
    assert_eq!(port["applies"], "restart");
    assert!(port["description"].as_str().unwrap().contains("port"));
    let mode = field(&view, "search.default_mode");
    assert_eq!(mode["kind"], "choice");
    assert_eq!(
        mode["options"],
        json!(["keyword", "semantic", "both", "auto"])
    );
    assert_eq!(view["restart_needed"], false);

    let (status, view) = app
        .put_admin(
            "/admin/settings",
            json!({ "changes": { "server.port": 8181, "converter.url": "http://127.0.0.1:8090",
                                 "search.slots.knowledge": 5 } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(field(&view, "server.port")["saved"], 8181);
    assert_eq!(
        field(&view, "server.port")["value"],
        8080,
        "until a restart"
    );
    assert_eq!(view["restart_needed"], true);
    let text = std::fs::read_to_string(app.data.path().join("config.toml")).unwrap();
    assert!(text.contains("port = 8181   # the port"), "{text}");
    assert!(text.contains("[server]   # where it listens"), "{text}");
    assert!(text.contains("url = \"http://127.0.0.1:8090\""), "{text}");

    // Unknown fields, wrong kinds and a config that wouldn't load are refused; nothing is
    // written.
    for bad in [
        json!({ "nope.key": 1 }),
        json!({ "server.port": "eighty" }),
        json!({ "search.default_mode": "semantic" }),
    ] {
        let (status, resp) = app
            .put_admin("/admin/settings", json!({ "changes": bad }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {resp}");
    }
    assert!(
        std::fs::read_to_string(app.data.path().join("config.toml"))
            .unwrap()
            .contains("port = 8181")
    );
    // A change that can't be saved whole isn't saved in part.
    let (status, _) = app
        .put_admin(
            "/admin/settings",
            json!({ "changes": { "cache.idle_minutes": 5, "search.default_mode": "semantic" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !std::fs::read_to_string(app.data.path().join("config.toml"))
            .unwrap()
            .contains("idle_minutes")
    );
    // A default comes back with null.
    app.put_admin(
        "/admin/settings",
        json!({ "changes": { "converter.url": null } }),
    )
    .await;
    let text = std::fs::read_to_string(app.data.path().join("config.toml")).unwrap();
    assert!(!text.contains("8090"), "{text}");
    let (_, audit) = app.get_admin("/admin/audit").await;
    assert_eq!(audit["entries"][0]["action"], "settings.write");

    // Settings are the admin's.
    let key = app.create_tenant("acme").await;
    let (status, _) = app.get(Caller::new(&key), "/admin/settings").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A service started without a file has nothing to write to.
    let plain = TestApp::new();
    let (status, _) = plain
        .put_admin(
            "/admin/settings",
            json!({ "changes": { "server.port": 1 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_environment_never_shows_a_secret() {
    let app = TestApp::new();
    let (status, env) = app.get_admin("/admin/environment").await;
    assert_eq!(status, StatusCode::OK, "{env}");
    let vars = env["variables"].as_array().unwrap();
    for name in [
        "PINAKES_CONFIG",
        "PINAKES_ADMIN_KEY",
        "HF_ENDPOINT",
        "NEO4J_PASSWORD",
        "RUST_LOG",
    ] {
        assert!(vars.iter().any(|v| v["name"] == name), "{name} in {env}");
    }
    for v in vars.iter().filter(|v| v["secret"] == true) {
        assert!(v.get("value").is_none(), "{v}");
    }
    let commit = vars
        .iter()
        .find(|v| v["name"] == "PINAKES_GIT_COMMIT")
        .unwrap();
    assert_eq!(commit["set"], true);
}

/// A stand-in OpenAI-compatible model that answers every call with `{"ok": true}`; returns
/// its base URL.
async fn fake_openai() -> String {
    async fn chat() -> axum::Json<Value> {
        axum::Json(json!({
            "choices": [{ "finish_reason": "stop", "message": { "content": "{\"ok\": true}" } }]
        }))
    }
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn a_model_keeps_its_key_when_moved_and_takes_any_key_variable() {
    let app = TestApp::with_config_file("");
    let first = fake_openai().await;
    let (status, m) = app
        .put_admin(
            "/admin/model",
            json!({ "provider": "openai_compatible", "base_url": first,
                    "model": "m", "api_key": "sk-kept" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    // Another address without a key of its own: the stored key goes along.
    let second = fake_openai().await;
    let (status, m) = app
        .put_admin("/admin/model", json!({ "base_url": second }))
        .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["key"], "file");
    assert_eq!(m["librarian"], true);
    let file = std::fs::read_to_string(app.data.path().join("model.conf")).unwrap();
    assert!(file.contains("sk-kept"), "{file}");
    // `key_env` may name any variable.
    let (status, m) = app
        .put_admin("/admin/model", json!({ "key_env": "MY_LLM_TOKEN" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["key_env"], "MY_LLM_TOKEN");
}

#[tokio::test]
async fn the_model_is_changed_without_a_restart_and_its_key_never_returned() {
    let app = TestApp::with_config_file("");
    let (status, m) = app.get_admin("/admin/model").await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert!(m["file"].as_str().unwrap().ends_with("model.conf"));
    assert!(m["presets"].as_array().unwrap().len() >= 3);

    // Tested before saving: a server that isn't there fails, and nothing is written.
    let (status, t) = app
        .post_admin(
            "/admin/model/test",
            json!({ "provider": "openai_compatible", "base_url": "http://127.0.0.1:9/v1",
                    "model": "m", "api_key": "sk-test" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["ok"], false);
    assert!(!app.data.path().join("model.conf").exists());

    let url = fake_openai().await;
    let change = json!({ "provider": "openai_compatible", "base_url": url,
                         "model": "local-model", "api_key": "sk-very-secret" });
    let (_, t) = app.post_admin("/admin/model/test", change.clone()).await;
    assert_eq!(t["ok"], true, "{t}");
    let (status, m) = app.put_admin("/admin/model", change).await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["model"], "local-model");
    assert_eq!(m["key"], "file");
    assert_eq!(m["librarian"], true);
    assert!(!m.to_string().contains("sk-very-secret"));
    let file = std::fs::read_to_string(app.data.path().join("model.conf")).unwrap();
    assert!(file.contains("model = \"local-model\""), "{file}");
    assert!(
        file.contains("# The librarian's model"),
        "written from the example, comments kept: {file}"
    );
    // The model as saved answers the test too, and the overview names it.
    let (_, t) = app.post_admin("/admin/model/test", json!({})).await;
    assert_eq!(t["ok"], true, "{t}");
    let key = app.create_tenant("acme").await;
    let (_, s) = app.get(Caller::new(&key), "/status").await;
    assert_eq!(s["librarian"]["on"], true);
    assert!(
        s["librarian"]["model"].is_null(),
        "a tenant isn't told the model: {s}"
    );
    assert_eq!(
        app.state.model_state().config.model().as_deref(),
        Some("local-model")
    );
    let (_, audit) = app.get_admin("/admin/audit").await;
    let entry = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "model.write")
        .unwrap()
        .clone();
    assert_eq!(entry["detail"]["api_key_changed"], true);
    assert!(!audit.to_string().contains("sk-very-secret"));

    // An OpenAI-compatible model without its address is refused.
    let (status, _) = app
        .put_admin(
            "/admin/model",
            json!({ "provider": "openai_compatible", "base_url": null }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.get(Caller::new(&key), "/admin/model").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = ADMIN;
}

#[tokio::test]
async fn the_graph_is_browsed_within_what_the_caller_sees() {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    app.post(
        c,
        "/ingest/graph/schema",
        json!({ "schema": "commerce", "summary": "Orders.",
                "nodes": { "order": { "key": "order_id" }, "customer": { "key": "customer_id" } },
                "relations": { "placed_by": { "from": "order", "to": "customer" } } }),
    )
    .await;
    app.post(
        c,
        "/ingest/graph/space",
        json!({ "space": "mainland", "schema": "commerce" }),
    )
    .await;
    let jin = c.user(Some("jin"));
    app.post(
        jin,
        "/ingest/graph/space",
        json!({ "space": "secret", "schema": "commerce", "visibility": "private" }),
    )
    .await;
    let (status, r) = app
        .post(
            c,
            "/ingest/graph/data",
            json!({ "space": "mainland",
                    "nodes": [{ "type": "order", "key": "1042", "name": "Order 1042" },
                              { "type": "order", "key": "1043", "name": "Order 1043", "aliases": ["加急单"] },
                              { "type": "customer", "key": "c1", "name": "林晓" }],
                    "edges": [{ "from": "order/1042", "type": "placed_by", "to": "customer/c1" }] }),
        )
        .await;
    assert!(status.is_success(), "{r}");
    app.post(
        jin,
        "/ingest/graph/data",
        json!({ "space": "secret", "nodes": [{ "type": "order", "key": "7", "name": "Order 7" }] }),
    )
    .await;

    let (status, s) = app.get(c, "/graph/schemas").await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["schemas"][0]["schema"], "commerce");
    assert_eq!(s["schemas"][0]["page"], "graph/commerce");
    assert_eq!(s["schemas"][0]["spaces"], json!(["mainland"]));
    let (_, spaces) = app.get(c, "/graph/spaces").await;
    assert_eq!(spaces["spaces"].as_array().unwrap().len(), 1, "{spaces}");
    assert_eq!(
        spaces["spaces"][0]["nodes"],
        json!({ "customer": 1, "order": 2 })
    );
    assert_eq!(spaces["spaces"][0]["edges"], 1);
    let (_, jins) = app.get(jin, "/graph/spaces").await;
    let secret = jins["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["space"] == "secret")
        .unwrap()
        .clone();
    assert_eq!(secret["mine"], true);

    let (_, n) = app.get(c, "/graph/nodes?space=mainland&type=order").await;
    assert_eq!(n["total"], 2);
    assert_eq!(n["nodes"][0]["id"], "mainland:order/1042");
    let (_, n) = app
        .get(c, "/graph/nodes?space=mainland&q=%E5%8A%A0%E6%80%A5")
        .await;
    assert_eq!(n["total"], 1, "found by alias: {n}");
    let (_, n) = app
        .get(c, "/graph/nodes?space=mainland&limit=1&offset=1")
        .await;
    assert_eq!(
        (n["total"].as_u64(), n["nodes"].as_array().unwrap().len()),
        (Some(3), 1)
    );
    let (status, _) = app.get(c, "/graph/nodes?space=secret").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a hidden space is not found");
    let (status, _) = app.get(jin, "/graph/nodes?space=secret").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn pages_come_rendered_and_with_their_history() {
    let mut app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let wiki = WikiFs::new(app.wiki_dir("acme"));
    wiki.write(&page(
        "expertise/returns",
        "Returns",
        "# Returns\n\nSeven days. See [[expertise/shipping]].\n\n<script>alert(1)</script>\n",
    ))
    .unwrap();
    app.restart();
    // The first search builds the snapshot, and saves the wiki's first version.
    app.post(c, "/search", json!({ "query": "returns" })).await;
    app.state.tenants.history_saved().await;

    let (status, p) = app
        .get(c, "/pages/expertise/returns?html=true&_=1712345678")
        .await;
    assert_eq!(status, StatusCode::OK, "other parameters are ignored: {p}");
    let html = p["html"].as_str().unwrap();
    assert!(html.contains("<h1>Returns</h1>"), "{html}");
    assert!(
        html.contains(r##"href="#/wiki/expertise/shipping""##),
        "{html}"
    );
    assert!(!html.contains("<script"), "{html}");
    let (_, plain) = app.get(c, "/pages/expertise/returns").await;
    assert!(plain.get("html").is_none());

    wiki.write(&page(
        "expertise/returns",
        "Returns",
        "# Returns\n\nFourteen days now.\n",
    ))
    .unwrap();
    app.restart();
    app.post(c, "/search", json!({ "query": "returns" })).await;
    app.state.tenants.history_saved().await;

    // Back to the first text while another page changes (a new wiki version): listed again,
    // as the latest.
    wiki.write(&page(
        "expertise/returns",
        "Returns",
        "# Returns\n\nSeven days. See [[expertise/shipping]].\n\n<script>alert(1)</script>\n",
    ))
    .unwrap();
    wiki.write(&page("expertise/shipping", "Shipping", "Two days.\n"))
        .unwrap();
    app.restart();
    app.post(c, "/search", json!({ "query": "returns" })).await;
    app.state.tenants.history_saved().await;

    let (status, h) = app.get(c, "/history/expertise/returns").await;
    assert_eq!(status, StatusCode::OK, "{h}");
    let versions = h["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 3, "{h}");
    let latest = versions[2]["version"].as_str().unwrap();
    let (_, now) = app
        .get(c, &format!("/history/expertise/returns?version={latest}"))
        .await;
    assert!(
        now["body"].as_str().unwrap().contains("Seven days"),
        "{now}"
    );
    let first = versions[0]["version"].as_str().unwrap();
    let (status, old) = app
        .get(c, &format!("/history/expertise/returns?version={first}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert!(old["body"].as_str().unwrap().contains("Seven days"));
    assert!(old["html"].as_str().unwrap().contains("<p>Seven days."));

    // Another user's page is not found, with its history.
    let path = format!(
        "users/{}/decisions/tea",
        pinakes::tenants::user_dir_name("lin")
    );
    wiki.write(&page(&path, "Tea", "Green.\n")).unwrap();
    let (status, _) = app
        .get(c.user(Some("other")), &format!("/history/{path}"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app
        .get(c, "/history/expertise/returns?version=0000000000000000")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
