//! Switching a tenant's graph store: a service configured for another store than the one
//! holding a tenant's graph data refuses to open the tenant (never an empty graph in silence),
//! and `pinakes-migrate-graph` (`graph::migrate::migrate`) moves the data there and back. The
//! move runs against Neo4j with `PINAKES_TEST_NEO4J_URI` set (see `tests/graph_testset.rs`),
//! and says so and passes without it.

mod common;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use pinakes::config::Config;
use pinakes::graph::GraphBackend;
use serde_json::{Value, json};

fn neo4j_config(uri: &str) -> String {
    format!(
        "[graph]\nbackend = \"neo4j\"\n[neo4j]\nuri = {uri:?}\npassword_env = \"PINAKES_TEST_NEO4J_PASSWORD\"\n"
    )
}

/// A schema, a space and one order placed by one customer.
async fn graph(app: &TestApp, key: &str) {
    let host = Caller::new(key);
    for (uri, body) in [
        (
            "/ingest/graph/schema",
            json!({ "schema": "commerce", "summary": "Orders and buyers.",
                    "nodes": { "order": { "key": "order_id", "aliases": ["order", "订单"] },
                               "customer": { "key": "customer_id" } },
                    "relations": { "placed_by": { "from": "order", "to": "customer",
                                                  "aliases": ["placed by"] } } }),
        ),
        (
            "/ingest/graph/space",
            json!({ "space": "mainland", "schema": "commerce" }),
        ),
        (
            "/ingest/graph/data",
            json!({ "space": "mainland",
                    "nodes": [{ "type": "order", "key": "1042", "name": "Order 1042" },
                              { "type": "customer", "key": "c1", "name": "Lin Xiao" }],
                    "edges": [{ "from": "order/1042", "type": "placed_by", "to": "customer/c1" }] }),
        ),
    ] {
        let (status, resp) = app.post(host, uri, body).await;
        assert!(status.is_success(), "{uri}: {status} {resp}");
    }
}

async fn edges(app: &TestApp, key: &str) -> (StatusCode, usize) {
    let (status, resp): (StatusCode, Value) = app
        .post(
            Caller::new(key),
            "/search",
            json!({ "query": "who placed order 1042", "keywords": ["order 1042", "placed by"] }),
        )
        .await;
    let n = resp["relations"]["edges"].as_array().map_or(0, Vec::len);
    (status, n)
}

/// The nodes the tenant's `graph.sqlite` holds.
fn sqlite_nodes(app: &TestApp, tenant: &str) -> i64 {
    let db = rusqlite::Connection::open(app.tenant_dir(tenant).join("graph.sqlite")).unwrap();
    db.query_row("SELECT count(*) FROM nodes", [], |r| r.get(0))
        .unwrap()
}

/// Removes the tenant's record of its store, as for a tenant from before the record.
fn forget_record(app: &TestApp, tenant: &str) {
    let db = rusqlite::Connection::open(app.tenant_dir(tenant).join("pinakes.sqlite")).unwrap();
    db.execute("DELETE FROM meta WHERE key = 'graph_backend'", [])
        .unwrap();
}

#[tokio::test]
async fn a_tenant_never_opens_on_the_wrong_store() {
    let mut app = TestApp::new();
    let key = app.create_tenant("acme").await;
    graph(&app, &key).await;
    assert_eq!(edges(&app, &key).await, (StatusCode::OK, 1));

    // Configured for Neo4j now: the tenant's data is in SQLite, so it doesn't open (nothing
    // is asked of Neo4j, which isn't there).
    app.restart_with_config(&neo4j_config("bolt://127.0.0.1:1"));
    assert_eq!(edges(&app, &key).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    // Back on SQLite, all is there.
    app.restart_with_config("");
    assert_eq!(edges(&app, &key).await, (StatusCode::OK, 1));

    // A tenant from before the record: its graph.sqlite says SQLite, and is recorded.
    forget_record(&app, "acme");
    app.restart();
    assert_eq!(edges(&app, &key).await, (StatusCode::OK, 1));
    app.restart_with_config(&neo4j_config("bolt://127.0.0.1:1"));
    assert_eq!(edges(&app, &key).await.0, StatusCode::INTERNAL_SERVER_ERROR);

    // Spaces without a graph.sqlite: the data was in Neo4j, the shipped store until then.
    app.restart_with_config("");
    let other = app.create_tenant("beta").await;
    let (status, resp) = app
        .post(
            Caller::new(&other),
            "/ingest/graph/schema",
            json!({ "schema": "commerce", "nodes": { "order": { "key": "order_id" } } }),
        )
        .await;
    assert!(status.is_success(), "{resp}");
    let (status, _) = app
        .post(
            Caller::new(&other),
            "/ingest/graph/space",
            json!({ "space": "mainland", "schema": "commerce" }),
        )
        .await;
    assert!(status.is_success());
    forget_record(&app, "beta");
    app.restart();
    assert_eq!(
        edges(&app, &other).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn graph_data_moves_to_neo4j_and_back() {
    let Ok(uri) = std::env::var("PINAKES_TEST_NEO4J_URI") else {
        println!("PINAKES_TEST_NEO4J_URI is not set; skipping the move to Neo4j");
        return;
    };
    let mut app = TestApp::new();
    let tenant = format!("switch-{}", hex::encode(rand::random::<[u8; 4]>()));
    let key = app.create_tenant(&tenant).await;
    graph(&app, &key).await;
    let root = app.config().tenants_dir();
    let file = app.config().store.file_name.clone();
    let neo4j = GraphBackend::from_config(&Config::from_toml_str(&neo4j_config(&uri)).unwrap());

    // Moved while the service is stopped.
    app.restart_with_config(&neo4j_config(&uri));
    let moved =
        pinakes::graph::migrate::migrate(&root, &file, &GraphBackend::Sqlite, &neo4j, false)
            .await
            .unwrap();
    assert_eq!((moved[0].nodes, moved[0].edges), (2, 1));
    // The old copy is gone: no deletion would reach it any more.
    assert_eq!(sqlite_nodes(&app, &tenant), 0);
    app.restart_with_config(&neo4j_config(&uri));
    assert_eq!(edges(&app, &key).await, (StatusCode::OK, 1));
    // Written to Neo4j from now on.
    let (status, _) = app
        .post(
            Caller::new(&key),
            "/ingest/graph/data",
            json!({ "space": "mainland",
                    "nodes": [{ "type": "order", "key": "1043", "name": "Order 1043" }],
                    "edges": [{ "from": "order/1043", "type": "placed_by", "to": "customer/c1" }] }),
        )
        .await;
    assert!(status.is_success());

    // Back to SQLite. Data already there for the tenant (left by a failed earlier move) is in
    // the way, and only removed with --replace.
    app.restart_with_config(&neo4j_config(&uri));
    let stale = pinakes::graph::sqlite::SqliteGraphStore::open(
        &app.tenant_dir(&tenant).join("graph.sqlite"),
        &tenant,
    )
    .unwrap();
    use pinakes::graph::GraphStore as _;
    stale
        .write(
            &[pinakes::graph::def::NodeRecord {
                space: "mainland".into(),
                ty: "order".into(),
                key: "stale".into(),
                name: "Stale order".into(),
                aliases: Vec::new(),
            }],
            &[],
        )
        .await
        .unwrap();
    stale.close();
    let refused =
        pinakes::graph::migrate::migrate(&root, &file, &neo4j, &GraphBackend::Sqlite, false).await;
    assert!(refused.is_err(), "data already in SQLite is in the way");
    let moved = pinakes::graph::migrate::migrate(&root, &file, &neo4j, &GraphBackend::Sqlite, true)
        .await
        .unwrap();
    assert_eq!((moved[0].nodes, moved[0].edges), (3, 2));
    assert_eq!(sqlite_nodes(&app, &tenant), 3, "the stale node is gone");
    assert!(
        neo4j
            .open(&tenant, &app.tenant_dir(&tenant))
            .unwrap()
            .load()
            .await
            .unwrap()
            .nodes
            .is_empty(),
        "the copy in Neo4j is gone"
    );
    app.restart_with_config("");
    let (status, resp) = app
        .post(
            Caller::new(&key),
            "/graph",
            json!({ "node": "mainland:customer/c1" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["edges"].as_array().unwrap().len(), 2, "{resp}");

    // Leave nothing in the shared Neo4j database.
    app.restart_with_config(&neo4j_config(&uri));
    neo4j.clear_tenant(&tenant).await.unwrap();
}
