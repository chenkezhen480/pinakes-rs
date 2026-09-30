//! M3 "done when": relationship questions return the right edges in the relations slot, even
//! when knowledge pages share their keywords; nothing from a space the caller can't see ever
//! appears; and the tests pass against the Neo4j container.
//!
//! Every scenario is loaded the way a host would load it: its wiki pages, then its schemas,
//! spaces and graph data through `/ingest/graph/*`. Then every M3 question is asked as its
//! caller, and every M2 question again, to check the graph doesn't disturb the other slots.
//!
//! The SQLite graph backend always runs. The Neo4j run needs a server:
//!
//! ```text
//! NEO4J_PASSWORD=... docker compose up -d neo4j
//! KB_TEST_NEO4J_URI=bolt://127.0.0.1:7687 KB_TEST_NEO4J_PASSWORD=... cargo test --test graph_testset
//! ```
//!
//! Without `KB_TEST_NEO4J_URI` the Neo4j test says so and passes. Run with `-- --nocapture` for
//! the per-category report.

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset::{self, Tally, describe, judge};
use serde_json::json;

struct Outcome {
    by_category: Tally,
    failures: Vec<String>,
    total: usize,
}

/// Loads every scenario into a service with `extra_config`, then asks the questions of the
/// given milestones. Tenant ids get `suffix`, so runs sharing one Neo4j database never meet.
async fn run(
    extra_config: &str,
    suffix: &str,
    milestones: &[&str],
) -> (TestApp, Vec<String>, Outcome) {
    let mut app = TestApp::with_config(extra_config);
    let tenants = testset::tenants();
    let mut keys = HashMap::new();
    for t in &tenants {
        let id = format!("{}{suffix}", t.id);
        keys.insert(t.id.clone(), app.create_tenant(&id).await);
        testset::install_wiki(&t.id, &app.wiki_dir(&id));
    }
    // Load the installed pages the way a restarted service would.
    app.restart();
    for t in &tenants {
        for call in testset::graph_calls(t) {
            let caller = Caller::new(&keys[&t.id])
                .user(call.user.as_deref())
                .groups(&call.groups);
            let (status, body) = app.post(caller, call.uri, call.body).await;
            assert!(
                status.is_success(),
                "{} {}: {status} {body}",
                t.id,
                call.uri
            );
        }
    }

    let mut out = Outcome {
        by_category: Tally::default(),
        failures: Vec::new(),
        total: 0,
    };
    for t in &tenants {
        for q in testset::questions(&t.id) {
            if !milestones.contains(&q.milestone.as_str()) {
                continue;
            }
            let groups = q
                .user
                .as_deref()
                .map(|u| t.groups_of(u))
                .unwrap_or_default();
            let caller = Caller::new(&keys[&t.id])
                .user(q.user.as_deref())
                .groups(&groups);
            let mut body = json!({ "query": q.query });
            if !q.keywords.is_empty() {
                body["keywords"] = json!(q.keywords);
            }
            let (status, resp) = app.post(caller, "/search", body).await;
            assert_eq!(status, StatusCode::OK, "{}: {resp}", q.id);
            let verdict = judge(&q, &resp);
            out.total += 1;
            out.by_category.add(&q.category, verdict.is_none());
            if let Some(why) = verdict {
                out.failures.push(format!(
                    "  {} ({}): {why}{}",
                    q.id,
                    q.query,
                    describe(&resp)
                ));
            }
        }
    }
    let keys = tenants.iter().map(|t| keys[&t.id].clone()).collect();
    (app, keys, out)
}

fn report(name: &str, out: &Outcome) {
    println!(
        "{name}: {}/{} passed\n{}",
        out.total - out.failures.len(),
        out.total,
        out.by_category.lines().join("\n")
    );
    if !out.failures.is_empty() {
        println!("failures:\n{}", out.failures.join("\n"));
    }
}

#[tokio::test]
async fn m3_questions_with_the_sqlite_backend() {
    let (_app, _keys, out) = run("", "", &["M2", "M3"]).await;
    report("M2 and M3 questions, SQLite graph", &out);
    assert!(
        out.failures.is_empty(),
        "{} questions failed",
        out.failures.len()
    );
}

#[tokio::test]
async fn m3_questions_with_the_neo4j_backend() {
    let Ok(uri) = std::env::var("KB_TEST_NEO4J_URI") else {
        println!("KB_TEST_NEO4J_URI is not set; skipping the Neo4j run");
        return;
    };
    let config = format!(
        "[graph]\nbackend = \"neo4j\"\n[neo4j]\nuri = {uri:?}\npassword_env = \"KB_TEST_NEO4J_PASSWORD\"\n"
    );
    let suffix = format!("-{}", hex_suffix());
    let (app, keys, out) = run(&config, &suffix, &["M2", "M3"]).await;
    report("M2 and M3 questions, Neo4j graph", &out);

    // After a restart the graph is read back from Neo4j, not from the old process's memory.
    let mut app = app;
    app.restart();
    let (status, resp) = app
        .post(
            Caller::new(&keys[testset::tenants().iter().position(|t| t.id == "shopline").unwrap()]),
            "/search",
            json!({ "query": "Which customer placed order 1042?", "keywords": ["order 1042", "placed by"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let edges = testset::slot_edges(&resp);
    assert!(
        edges.contains(&(
            "mainland:order/1042".into(),
            "placed_by".into(),
            "mainland:customer/c_5521".into()
        )),
        "{resp}"
    );

    // Leave nothing behind in the shared database.
    for key in &keys {
        let tenant = app.state.tenants.authenticate(key).await.unwrap().unwrap();
        tenant.graph_store.delete_all().await.unwrap();
        assert!(tenant.graph_store.load().await.unwrap().nodes.is_empty());
    }
    assert!(
        out.failures.is_empty(),
        "{} questions failed",
        out.failures.len()
    );
}

fn hex_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", nanos ^ u128::from(std::process::id()))
}
