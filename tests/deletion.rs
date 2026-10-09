//! M8 "done when", deletion: deleted data is gone from files, SQLite, the search index, page
//! vectors and the graph store, and what belongs to others stays.
//!
//! Each owner's records carry a marker word; after a deletion no file of the tenant holds it,
//! looked for byte by byte in every file (wiki, raw sources, SQLite with its WAL, the tantivy
//! segments, the wiki history). Search traces are the one exception, by design: they are kept
//! and expire with the retention period. The Neo4j part runs with `PINAKES_TEST_NEO4J_URI` set
//! (see `tests/graph_testset.rs`), and says so and passes without it.

mod common;

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::embed::Fake;
use common::llm::{FakeLlm, OK, Step, all_ok, create, describe_from_definition, folder, items};
use pinakes::tenants::{group_dir_name, user_dir_name};
use serde_json::{Value, json};

/// In lily's decision and preference.
const LILY: &str = "ZEBRA7731";
/// In the risk group's decision.
const RISK: &str = "QUOKKA4410";
/// In a tenant playbook, which stays.
const TENANT: &str = "KEEPME5150";
/// The name of a node in lily's private graph space.
const LEADS: &str = "PANGOLIN8820";

/// A librarian that files each record as one page holding its text.
fn filer() -> Arc<FakeLlm> {
    FakeLlm::new(|step, msg, _| match step {
        Step::Plan => {
            let dir = folder(msg);
            let edits: Vec<Value> = items(msg)
                .iter()
                .map(|item| {
                    let r = &item.content;
                    let text = ["decision", "statement", "outcome"]
                        .iter()
                        .find_map(|k| r[k].as_str())
                        .unwrap();
                    let title = ["topic", "task"]
                        .iter()
                        .find_map(|k| r[k].as_str())
                        .unwrap_or("Preferences");
                    let page = if dir.ends_with("preferences") {
                        dir.clone()
                    } else {
                        format!("{dir}/{}", title.replace(' ', "-"))
                    };
                    let body = format!("# {title}\n\n{text}\n");
                    let mut e = create(&page, title, text, &["commerce:order"], &body);
                    e["item"] = json!(item.id);
                    e
                })
                .collect();
            json!({ "edits": edits }).to_string()
        }
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => common::llm::skip_to_end(msg),
    })
}

fn schema() -> Value {
    json!({
        "schema": "commerce",
        "summary": "Orders and their buyers.",
        "nodes": {
            "order": { "key": "order_id", "aliases": ["orders", "订单"] },
            "customer": { "key": "customer_id", "aliases": ["customers", "客户"] }
        },
        "relations": {
            "placed_by": { "from": "order", "to": "customer", "via": "customer_id" }
        }
    })
}

async fn ok(app: &TestApp, caller: Caller<'_>, uri: &str, body: Value) -> Value {
    let (status, resp) = app.post(caller, uri, body).await;
    assert!(status.is_success(), "{uri}: {status} {resp}");
    resp
}

/// Inserts a record and waits until it is filed.
async fn file(app: &TestApp, caller: Caller<'_>, uri: &str, body: Value) {
    let resp = ok(app, caller, uri, body).await;
    let job = app.wait_job(caller, resp["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    assert!(
        !job["result"]["pages_created"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{job}"
    );
}

/// The files under `dir` (relative paths) whose bytes hold `needle`, as written or lowercased
/// (the index keeps tokens in lower case), leaving out the folders named in `skip`.
fn files_with(dir: &Path, needle: &str, skip: &[&str]) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, needles: &[Vec<u8>], skip: &[&str], out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if skip.contains(&rel.as_str()) {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, needles, skip, out);
            } else if let Ok(bytes) = std::fs::read(&path)
                && needles
                    .iter()
                    .any(|n| bytes.windows(n.len()).any(|w| w == n.as_slice()))
            {
                out.push(rel);
            }
        }
    }
    let needles = [
        needle.as_bytes().to_vec(),
        needle.to_lowercase().into_bytes(),
    ];
    let mut out = Vec::new();
    walk(dir, dir, &needles, skip, &mut out);
    out
}

/// Stored page vectors whose page starts with `prefix`.
fn vectors_under(tenant_dir: &Path, prefix: &str) -> i64 {
    let db = rusqlite::Connection::open(tenant_dir.join("pinakes.sqlite")).unwrap();
    db.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE substr(page, 1, length(?1)) = ?1",
        [prefix],
        |r| r.get(0),
    )
    .unwrap()
}

/// A registered space's definition, if it is still registered.
fn space(tenant_dir: &Path, name: &str) -> Option<Value> {
    let db = rusqlite::Connection::open(tenant_dir.join("pinakes.sqlite")).unwrap();
    db.query_row(
        "SELECT definition FROM graph_spaces WHERE name = ?1",
        [name],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .map(|d| serde_json::from_str(&d).unwrap())
}

fn pages_of(search: &Value) -> Vec<String> {
    ["memories", "knowledge"]
        .iter()
        .flat_map(|slot| search[slot]["hits"].as_array().unwrap().clone())
        .map(|h| h["page"].as_str().unwrap().to_owned())
        .collect()
}

/// A user's data goes, then a group's: pages, raw records, vectors, index, history, the user's
/// private space and their places on member lists. The tenant's own data stays.
#[tokio::test]
async fn deleting_a_user_and_a_group_leaves_nothing_of_theirs() {
    let app = TestApp::with_llm_and_embedder(filer().llm(), Fake::new());
    let key = app.create_tenant("acme").await;
    let host = Caller::new(&key);
    let lily = host.user(Some("lily"));
    let risk = vec!["risk".to_owned()];
    let marco = host.user(Some("marco")).groups(&risk);

    ok(&app, host, "/ingest/graph/schema", schema()).await;
    let new_space = |name: &str, visibility: &str| json!({ "space": name, "schema": "commerce", "visibility": visibility });
    ok(
        &app,
        lily,
        "/ingest/graph/space",
        new_space("leads", "private"),
    )
    .await;
    ok(
        &app,
        lily,
        "/ingest/graph/space",
        new_space("shop", "shared"),
    )
    .await;
    let mut vault = new_space("vault", "restricted");
    vault["members"] = json!({ "users": ["lily", "marco"], "groups": ["risk"] });
    ok(&app, marco, "/ingest/graph/space", vault).await;
    ok(
        &app,
        lily,
        "/ingest/graph/data",
        json!({ "space": "leads", "nodes": [{ "type": "customer", "key": "c1", "name": LEADS }] }),
    )
    .await;

    file(
        &app,
        lily,
        "/ingest/decision",
        json!({ "topic": "refund policy", "decision": format!("Refund shipping costs {LILY}") }),
    )
    .await;
    file(
        &app,
        lily,
        "/ingest/preference",
        json!({ "statement": format!("Answer briefly {LILY}") }),
    )
    .await;
    file(
        &app,
        marco.audience("group:risk"),
        "/ingest/decision",
        json!({ "topic": "chargebacks", "decision": format!("Dispute every chargeback {RISK}") }),
    )
    .await;
    file(
        &app,
        host,
        "/ingest/experience",
        json!({ "task": "refunds", "outcome": format!("Check the shipment first {TENANT}") }),
    )
    .await;
    let found = ok(&app, lily, "/search", json!({ "query": "refund policy" })).await;
    let lily_dir = format!("users/{}/", user_dir_name("lily"));
    assert!(
        pages_of(&found).iter().any(|p| p.starts_with(&lily_dir)),
        "{found}"
    );
    app.state.tenants.history_saved().await;

    let dir = app.tenant_dir("acme");
    // Before: the marker is everywhere a page's text goes.
    for place in ["wiki", "sources", "history", "index"] {
        assert!(
            !files_with(&dir.join(place), LILY, &[]).is_empty(),
            "{place} holds lily's text before the deletion"
        );
    }
    assert!(!files_with(&dir, LEADS, &[]).is_empty());
    assert!(vectors_under(&dir, &lily_dir) > 0);

    let (status, body) = app.delete(host, "/users/lily").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["user"], "lily");
    assert_eq!(body["pages_deleted"], 2, "{body}");
    assert_eq!(body["sources_deleted"], 2, "{body}");
    assert_eq!(body["spaces_deleted"], json!(["leads"]), "{body}");
    assert_eq!(body["spaces_changed"], json!(["shop", "vault"]), "{body}");

    // After: nowhere, except the search traces, which are kept by design.
    assert_eq!(files_with(&dir, LILY, &["logs"]), Vec::<String>::new());
    assert_eq!(files_with(&dir, LEADS, &["logs"]), Vec::<String>::new());
    assert_eq!(vectors_under(&dir, &lily_dir), 0);
    assert!(space(&dir, "leads").is_none());
    assert!(space(&dir, "shop").unwrap().get("owner").is_none());
    assert_eq!(
        space(&dir, "vault").unwrap()["members"],
        json!({ "users": ["marco"], "groups": ["risk"] })
    );
    // Search and the page list no longer know them; the others' pages are still found.
    let found = ok(&app, lily, "/search", json!({ "query": "refund policy" })).await;
    assert!(
        !pages_of(&found).iter().any(|p| p.starts_with(&lily_dir)),
        "{found}"
    );
    assert!(
        pages_of(&found).contains(&"experience/refunds".to_owned()),
        "{found}"
    );
    let (_, pages) = app.get(lily, "/pages").await;
    assert!(!pages.to_string().contains(&lily_dir), "{pages}");
    for marker in [TENANT, RISK] {
        assert!(!files_with(&dir.join("wiki"), marker, &[]).is_empty());
    }
    // Deleting again finds nothing more.
    let (status, body) = app.delete(host, "/users/lily").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pages_deleted"], 0, "{body}");

    // A group is deleted by the admin.
    let (status, _) = app.delete(host, "/admin/tenants/acme/groups/risk").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = app.delete_admin("/admin/tenants/acme/groups/risk").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pages_deleted"], 1, "{body}");
    assert_eq!(body["spaces_changed"], json!(["vault"]), "{body}");
    assert_eq!(files_with(&dir, RISK, &["logs"]), Vec::<String>::new());
    let group_dir = format!("groups/{}/", group_dir_name("risk"));
    assert_eq!(vectors_under(&dir, &group_dir), 0);
    assert_eq!(
        space(&dir, "vault").unwrap()["members"],
        json!({ "users": ["marco"] })
    );
    assert!(!files_with(&dir, TENANT, &["logs"]).is_empty());
    let found = ok(
        &app,
        host,
        "/search",
        json!({ "query": "refunds shipment" }),
    )
    .await;
    assert!(
        pages_of(&found).contains(&"experience/refunds".to_owned()),
        "{found}"
    );
    let (status, _) = app.delete_admin("/admin/tenants/nobody/groups/risk").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A tenant goes whole: its folder, its keys; its id can be used again for a new, empty tenant.
/// Another tenant is untouched.
#[tokio::test]
async fn deleting_a_tenant_removes_its_folder() {
    let mut app = TestApp::with_llm(filer().llm());
    let key = app.create_tenant("acme").await;
    let other = app.create_tenant("other").await;
    for k in [&key, &other] {
        ok(&app, Caller::new(k), "/ingest/graph/schema", schema()).await;
        file(
            &app,
            Caller::new(k),
            "/ingest/experience",
            json!({ "task": "refunds", "outcome": format!("Check the shipment first {TENANT}") }),
        )
        .await;
    }
    let dir = app.tenant_dir("acme");
    assert!(dir.is_dir());

    let (status, _) = app.delete(Caller::new(&key), "/admin/tenants/acme").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the tenant's key is no admin key"
    );
    let (status, body) = app.delete_admin("/admin/tenants/acme").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!dir.exists());
    let left: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(!left.iter().any(|n| n.starts_with('.')), "{left:?}");
    let (status, _) = app.get(Caller::new(&key), "/pages").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app.delete_admin("/admin/tenants/acme").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, found) = app
        .post(
            Caller::new(&other),
            "/search",
            json!({ "query": "refunds" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(pages_of(&found).contains(&"experience/refunds".to_owned()));

    // The same id makes a new tenant with nothing in it, also after a restart.
    let again = app.create_tenant("acme").await;
    app.restart();
    let (status, found) = app
        .post(
            Caller::new(&again),
            "/search",
            json!({ "query": "refunds" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(pages_of(&found).is_empty(), "{found}");
}

/// With the Neo4j backend, a user's private space and a deleted tenant leave no node there.
#[tokio::test]
async fn deletion_reaches_neo4j() {
    let Ok(uri) = std::env::var("PINAKES_TEST_NEO4J_URI") else {
        println!("PINAKES_TEST_NEO4J_URI is not set; skipping the Neo4j deletion test");
        return;
    };
    let config = format!(
        "[graph]\nbackend = \"neo4j\"\n[neo4j]\nuri = {uri:?}\npassword_env = \"PINAKES_TEST_NEO4J_PASSWORD\"\n"
    );
    let app = TestApp::with_llm_and_config(filer().llm(), &config);
    let tenant = format!("deletion-{}", hex::encode(rand::random::<[u8; 4]>()));
    let key = app.create_tenant(&tenant).await;
    let host = Caller::new(&key);
    let lily = host.user(Some("lily"));
    ok(&app, host, "/ingest/graph/schema", schema()).await;
    for (who, space, visibility) in [(lily, "leads", "private"), (host, "shop", "shared")] {
        ok(
            &app,
            who,
            "/ingest/graph/space",
            json!({ "space": space, "schema": "commerce", "visibility": visibility }),
        )
        .await;
        ok(
            &app,
            who,
            "/ingest/graph/data",
            json!({ "space": space, "nodes": [{ "type": "customer", "key": "c1", "name": LEADS }] }),
        )
        .await;
    }
    let graph = neo4rs::Graph::connect(
        neo4rs::ConfigBuilder::default()
            .uri(uri.as_str())
            .user("neo4j")
            .password(std::env::var("PINAKES_TEST_NEO4J_PASSWORD").unwrap())
            .build()
            .unwrap(),
    )
    .unwrap();
    let count = |space: Option<&str>| {
        let (graph, tenant) = (graph.clone(), tenant.clone());
        let space = space.map(str::to_owned);
        async move {
            let q = match &space {
                Some(s) => neo4rs::query(
                    "MATCH (n:PinakesNode {tenant: $t, space: $s}) RETURN count(n) AS n",
                )
                .param("t", tenant)
                .param("s", s.as_str()),
                None => neo4rs::query("MATCH (n:PinakesNode {tenant: $t}) RETURN count(n) AS n")
                    .param("t", tenant),
            };
            let mut rows = graph.execute(q).await.unwrap();
            rows.next().await.unwrap().unwrap().get::<i64>("n").unwrap()
        }
    };
    assert_eq!(count(Some("leads")).await, 1);

    let (status, body) = app.delete(host, "/users/lily").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(count(Some("leads")).await, 0);
    assert_eq!(count(Some("shop")).await, 1, "the tenant's own space stays");

    let (status, _) = app.delete_admin(&format!("/admin/tenants/{tenant}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(count(None).await, 0);
}
